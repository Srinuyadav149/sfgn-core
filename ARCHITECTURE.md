# SFGN v1.0 Architecture

Simple Flat Graph Notation (**SFGN v1.0**) is a zero-copy, indentation-locked flat graph format and multi-domain workspace indexer engineered for deterministic `O(1)` subgraph retrieval and low-context LLM agent orchestration.

---

## 1. Format & Dual-Topology Model

Every SFGN document is a flat sequence of Depth 0 vertex blocks starting with the mandatory root vertex `>:`:

- **Root & Node Headers (Depth 0)**: `>:` declares the mandatory document root (`NodeId(0)`). Non-root vertices are declared at column 0 as `NodePath:` where `NodePath = Segment { ">" Segment }` and each `Segment` is a double-quoted identifier with optional zero-based ordinal indices (`"Worker"#0`).
- **Dual-Topology Edges (Depth 1)**:
  - `>` (**Hierarchical Containment**): Defines a strict directed acyclic tree rooted at `>:`. Every non-root vertex in a strict document must be reachable from `>:` along `>` edges (`E017`).
  - `>>` (**Associative Relations**): Defines arbitrary cross-branch references (function calls, type dependencies, Git co-changes, Markdown cross-links, and external subgraph portals).
  - Both axes accept a single target or a 1D target array (`> ["A", "B"]`, `>> ["C", <"other.sfgn">"Node"]`).
- **External Subgraph Portals**: `<"relative/path.sfgn">` and `<"relative/path.sfgn">"Node">"SubNode"` link across `.sfgn` files without requiring the target file to be loaded or parsed upfront.
- **Properties & Opaque Multiline Payloads (Depth 1)**:
  - Scalars (`"str"`, numbers, `true`, `false`, `null`), 1D flat collections (`[1, 2, 3]`), and ordinal-indexed keys (`"weights"#0: [0.4, 0.6]`).
  - Opaque multiline blocks (`"""[format_tag]\n...\n"""`) preserve arbitrary source code, Markdown, or CSV tables without internal escaping; lines starting with `[OWS]"""` inside a block are escaped as `[OWS]\"""` and lazily unescaped on decode.
- **Document-Wide Indentation Locking**: The first Depth 1 line locks the document's indentation style to either a single `\t` or `N >= 2` spaces (`E004`), preventing ambiguous nesting.

---

## 2. Two-Pass Zero-Copy Compiler & Slicer (`sfgn_core`)

`sfgn_core` separates syntactic scanning from topological resolution so node slicing never allocates or copies source payloads.

```text
UTF-8 Buffer (&[u8] / &str)
       │
       ▼  Pass 1: Linear Scanner (`src/lexer.rs`, `src/scanner.rs`)
StructuralTape (`src/tape.rs`)
  ├── nodes:      Vec<NodeEntry>      (header span, full byte_range, prop/rel tape slices)
  ├── properties: Vec<PropertyEntry>  (key span, value span, format_tag span, kind)
  └── relations:  Vec<RelationEntry>  (`>` vs `>>`, single vs `[...]` array span)
       │
       ▼  Pass 2: Topology Indexer (`src/topology.rs`)
TopologyIndex
  ├── path_to_id: HashMap<CanonicalNodePath, NodeId>  (rejects duplicates with E016)
  ├── containment tree & parent_of links              (enforces reachability with E017)
  └── parsed `>` and `>>` RelationTargets (local NodeId + ExternalPortal)
       │
       ▼  Zero-Copy View (`src/document.rs`)
Document<'a>::node_slice_by_path(path) ──► &src[byte_range.start..byte_range.end]  (O(1))
```

### Pass 1: Linear Scanner (`O(N)` Time, `O(1)` Stack)
- Rejects leading UTF-8 BOM (`0xEF 0xBB 0xBF`), standalone `\r`, and illegal ASCII control bytes including `NUL` (`0x00`) with `E001`.
- Records exact byte spans (`Span`) and contiguous half-open node encapsulation ranges (`ByteRange`) into flat `Vec` tapes without heap-allocating strings.

### Pass 2: Topology Indexer & Fail-Fast Diagnostics (`E001`–`E017`)
- Canonicalizes quoted escapes and whitespace in `NodePath` headers into `CanonicalNodePath` keys, rejecting duplicate canonical vertices with `E016`.
- Verifies that every non-root vertex is reachable from `>:` via `>` containment edges under `OrphanPolicy::StrictError` (`E017`), or attaches unreachable subtrees to `>:` under `OrphanPolicy::AttachToRoot`.

### Programmatic Graph Construction (`src/builder.rs`)
- `SfgnBuilder` constructs graphs from `NodeHandle`s and runs **Two-Pass Minimal Collision Qualification**:
  1. Assigns `#0`, `#1`, ... ordinal indices only when identical local names share the exact same `>` parent.
  2. Prepends the shortest unique ancestor prefix (`"Parent">"Child"`) only when local names collide across distinct branches.
- Automatically runs `Document::parse` as a validation gate inside `SfgnBuilder::build()` before returning SFGN text.

---

## 3. Lazy Flat Workspace Graph Layout (`src/flat_path.rs`)

Instead of compiling monolithic `code.sfgn` and `doc.sfgn` files upfront, workspaces materialize a lightweight eager root filesystem graph paired with flat, lazily hydrated per-file graphs:

```text
<workspace_root>/
├── fs.sfgn                             # Eagerly compiled at session start
├── sfgn-codes/                         # Flat directory of lazily hydrated code graphs
│   └── crates:sfgn-cli:src:main.rs.sfgn
└── sfgn-docs/                          # Flat directory of lazily hydrated doc & media graphs
    ├── README.md.sfgn
    └── resources:banner.svg.sfgn
```

### Design Invariants
1. **Reversible `:` Path Codec (`src/flat_path.rs`)**:
   - `encode_flat_rel_path` maps `/` and `\` to `:` (`src/main.rs` -> `src:main.rs`), avoiding collisions with `__` in filenames such as `__init__.py`.
   - `decode_flat_artifact_path` recovers the workspace-relative source `PathBuf` directly from `sfgn-codes/<stem>.sfgn` or `sfgn-docs/<stem>.sfgn`, while every hydrated file also stores `"source_file"` in its `>:` root block.
2. **Self-Indexing Exclusion**:
   - `is_generated_sfgn_artifact` skips `fs.sfgn`, `sfgn-codes/`, and `sfgn-docs/` in all directory walkers and Git history scans so generated graph caches never pollute workspace topology.
3. **Incremental `mtime` Cache & Patch Coherence**:
   - `sfgn hydrate` and `sfgn slice --follow-portals` check `artifact_mtime >= source_mtime` before invoking `tree-sitter` or CommonMark parsing.
   - `sfgn patch` splices updated symbol code onto disk via `"source_file"` and `"line_span"` (`crates/sfgn-extract-code/src/patch.rs`) and immediately re-hydrates that single file's `sfgn-codes/<encoded>.sfgn` artifact.

---

## 4. Domain Extractors & Tooling

### Filesystem & Git Extractor (`sfgn-extract-fs`)
- Walks the repository with `ignore::WalkBuilder` (honoring `.gitignore`, `.git/info/exclude`, and global excludes).
- Queries `gix` for working-tree status (`clean`, `modified`, `staged`, `untracked`, `conflicted`, `renamed`, `deleted`), submodule markers (`git_submodule: true`), per-file `last_commit`, and historical co-change `>>` edges.
- Emits unhydrated lazy portals (`>> <"sfgn-codes/<encoded>.sfgn">` or `>> <"sfgn-docs/<encoded>.sfgn">`) on every supported code, Markdown, and media file vertex.

### Single-File Code Extractor (`sfgn-extract-code`)
- `CodeExtractor::extract_file` parses a single source file with `tree-sitter` and emits a `"kind": "code_file"` graph with top-level items (`mod`, `struct`, `enum`, `trait`, `impl`, `fn`, `type`, `const`, `static`, `macro`, `class`, `interface`) attached directly to `>:`.
- Inherent `impl Type` methods merge under `struct Type` / `enum Type` when defined in the same module.
- **Zero-Parse Import-to-Portal Resolution**: Intra-file calls and type references emit local `>>` edges, while imports (`use crate::...`, `use <crate>::...`, Python relative/package imports, JS/TS relative imports) and external `mod foo;` declarations resolve via `Path::exists` to `<"sfgn-codes/<encoded>.sfgn">` portals without opening target files.

### Single-File Docs, Media & Grammar Extractor (`sfgn-extract-docs`)
- `DocsExtractor::extract_file` compiles Markdown documents into `"kind": "document"` graphs (`H1 > H2 > H3` section hierarchy, prose, fenced code blocks, and GFM tables serialized as `"""csv` with `"table_shape"`) and binary/vector assets (`PNG`, `JPEG`, `GIF`, `WebP`, `WAV`, `PDF`, `SVG`) into `"kind": "media_proxy"` graphs from header bytes alone.
- Same-document `#anchor` links emit local `>>` edges; cross-file links resolve to `<"sfgn-docs/<encoded>.sfgn">`, `<"sfgn-codes/<encoded>.sfgn">`, or `<"fs.sfgn">"..."` portals.
- `compile_ebnf_to_gbnf` (`crates/sfgn-extract-docs/src/gbnf.rs`) transpiles EBNF specifications into `llama.cpp`-compatible GBNF grammars (`root ::= ...`) for constrained token decoding.

---

## 5. Agent Harness Boundary

SFGN serves as the internal indexing, slicing, and patch-anchoring substrate rather than raw prompt syntax for small quantized models:

- **Internal Storage Layer**: The execution harness manages `fs.sfgn`, `sfgn-codes/`, and `sfgn-docs/`, performing `O(1)` node slices, portal traversals, `mtime` checks, and `line_span` disk patches.
- **Projected Model Interface**: The harness strips SFGN delimiters and projects clean, token-minimal views—real repository paths (`src/flat_path.rs`), `[start, end]` line spans, signatures, caller/callee paths, and raw Rust or Markdown payloads—constrained by GBNF action grammars.
