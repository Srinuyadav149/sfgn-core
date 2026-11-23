# sfgn_core

Zero-copy Simple Flat Graph Notation (**SFGN v1.0**) parser, collision-disambiguating builder, dual-topology visualizer, lazy multi-domain workspace graph compiler, and progressive-reveal LLM agent harness in Rust.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the internal Pass 1 / Pass 2 pipeline, lazy flat hydration model, progressive-reveal agent architecture, and fail-fast diagnostic catalog.

## Workspace Crates

- **`sfgn_core` (`src/`)**: Zero-copy Pass 1 linear scanner (`StructuralTape`), Pass 2 topology indexer (`TopologyIndex`), `O(1)` node slicer (`Document`), minimal-collision-qualified graph compiler (`SfgnBuilder`), flat `:`-delimited artifact path codec (`flat_path`), and ASCII / Mermaid visualizer (`viz`).
- **`sfgn-extract-fs` (`crates/sfgn-extract-fs`)**: `.gitignore`-aware filesystem walker and `gix` Git analyzer emitting eager root `fs.sfgn` graphs with file stats, working-tree status, `last_commit`, historical co-change `>>` edges, and unhydrated lazy portals (`<"sfgn-codes/...">` and `<"sfgn-docs/...">`).
- **`sfgn-extract-code` (`crates/sfgn-extract-code`)**: `tree-sitter` CST/symbol compiler emitting per-file flat `sfgn-codes/<path>.sfgn` graphs (`"kind": "code_file"`) with zero-parse import-to-portal resolution and bidirectional disk line-span patching (`patch_node_source`). Defaults to `rust`; optional `python` and `typescript` features.
- **`sfgn-extract-docs` (`crates/sfgn-extract-docs`)**: CommonMark/GFM outline (`H1 > H2 > H3`) and CSV table compiler, binary/vector media header scanner (`PNG`, `JPEG`, `GIF`, `WebP`, `WAV`, `PDF`, `SVG`), and EBNF-to-GBNF grammar transpiler emitting per-file flat `sfgn-docs/<path>.sfgn` graphs (`"kind": "document"` / `"media_proxy"`).
- **`sfgn-llm` (`crates/sfgn-llm`)**: Zero-dependency HTTP/1.1 SSE `/v1/chat/completions` client, RFC 8259 JSON parser, GBNF grammar transport, and dual `reasoning_content` / `<think>` stream demuxer tuned for local LFM3 1.7B execution.
- **`sfgn-context` (`crates/sfgn-context`)**: Progressive Level 0/1/2 SFGN node projector, unified `tools>:` registry with dynamic GBNF compilation, `sfgn-memories/` cross-session note store, `sfgn-logs/` session trace writer, and KV-cache-ordered prompt assembler.
- **`sfgn-agent` (`crates/sfgn-agent`)**: Unified `Assemble -> Stream -> Dispatch -> Record -> Retry` agent loop (`AgentRunner` / `AgentSession`) with read-before-patch guards and structured Error-to-History recovery hints.
- **`sfgn-cli` (`crates/sfgn-cli`)**: Unified `sfgn` command-line tool combining autonomous agent execution (`run`, `chat`) with workspace graph compilation and tape inspection (`compile`, `hydrate`, `check`, `slice`, `grep`, `viz`, `patch`, `gbnf`).

## Workspace Graph Layout (Lazy Flat Hydration)

Compiling a workspace or starting an agent session initializes root-level SFGN artifacts without parsing source or documentation files upfront:

```text
<workspace_root>/
├── fs.sfgn                             # Eagerly compiled directory tree, Git metadata & lazy portals
├── sfgn-codes/                         # Lazily hydrated per-source-file graphs (`:` path delimiter)
│   └── src:main.rs.sfgn                # "kind": "code_file", "source_file": "src/main.rs"
├── sfgn-docs/                          # Lazily hydrated per-doc & media graphs (`:` path delimiter)
│   ├── README.md.sfgn                  # "kind": "document", "source_file": "README.md"
│   └── resources:banner.svg.sfgn       # "kind": "media_proxy", "source_file": "resources/banner.svg"
├── sfgn-memories/                      # Persistent cross-session notes & >> anchor portals
│   ├── index.sfgn
│   └── architecture.sfgn
└── sfgn-logs/                          # Turn-by-turn agent audit traces & token metrics
    └── session-<timestamp>.sfgn
```

- **Flat `:` Path Delimiter**: Workspace-relative paths map 1-to-1 to flat filenames (`src/flat_path.rs` <-> `sfgn-codes/src:flat_path.rs.sfgn`), while each `.sfgn` root (`>:`) also stores `"source_file"` explicitly.
- **Zero-Parse Cross-File Portals**: Hydrating a single code or Markdown file resolves imports, `mod` declarations, and relative links into unhydrated `<"sfgn-codes/...">`, `<"sfgn-docs/...">`, or `<"fs.sfgn">` portals via filesystem existence checks without opening target files.
- **Automatic `mtime` Staleness & Patch Refresh**: `sfgn hydrate`, `sfgn slice --follow-portals`, and agent inspections skip work when `artifact_mtime >= source_mtime`, and `sfgn patch` / `patch_symbol` refresh the target `sfgn-codes/<path>.sfgn` artifact immediately after splicing disk changes.

## Quickstart

### Build & Run Tests

```bash
cargo test --workspace
```

To enable Python and TypeScript/JavaScript AST extraction in `sfgn-extract-code`:

```bash
cargo test -p sfgn-extract-code --all-features
```

### Run the Autonomous SFGN Agent (`sfgn run` & `sfgn chat`)

Start a local `llama-server` instance (defaults to `http://127.0.0.1:9931/v1/chat/completions` with LFM3 1.7B):

```bash
# 1. Execute a single goal with progressive graph reveal & dynamic GBNF constraints
cargo run -p sfgn-cli -- run "Inspect src/flat_path.rs and how encode_flat_rel_path works, then record a summary note in sfgn-memories"

# 2. Start an interactive multi-goal REPL sharing memories, unlocked tools, and turn history
cargo run -p sfgn-cli -- chat --root .

# 3. Run against a custom OpenAI-compatible endpoint without GBNF grammar constraints
cargo run -p sfgn-cli -- run "Summarize ARCHITECTURE.md" --url [http://127.0.0.1:8080/v1/chat/completions](http://127.0.0.1:8080/v1/chat/completions) --no-grammar
```

### Compile, Hydrate & Inspect a Workspace with `sfgn`

```bash
# 1. Compile eager root fs.sfgn and initialize sfgn-codes/ and sfgn-docs/
cargo run -p sfgn-cli -- compile .

# 2. Lazily hydrate a single source or Markdown file on demand (mtime-cached)
cargo run -p sfgn-cli -- hydrate . src/flat_path.rs

# 3. Slice a node in O(1) and auto-hydrate/follow linked <"sfgn-codes/..."> portals
cargo run -p sfgn-cli -- slice fs.sfgn '"src">"flat_path.rs"' --follow-portals

# 4. Validate any .sfgn file through Pass 1 (scanner) and Pass 2 (topology)
cargo run -p sfgn-cli -- check sfgn-codes/src:flat_path.rs.sfgn

# 5. Linear scan across node headers, properties, and multiline blocks
cargo run -p sfgn-cli -- grep sfgn-codes/src:flat_path.rs.sfgn "encode_flat_rel_path"

# 6. Render an ASCII dual-topology tree or left-to-right Mermaid flowchart
cargo run -p sfgn-cli -- viz sfgn-docs/README.md.sfgn
cargo run -p sfgn-cli -- viz sfgn-docs/README.md.sfgn --mermaid --lr

# 7. Splice updated code back into disk via "source_file" + "line_span" and refresh cache
cargo run -p sfgn-cli -- patch . sfgn-codes/src:math.rs.sfgn '"fn add"' --body-file updated_add.rs

# 8. Transpile an EBNF grammar to llama.cpp GBNF
cargo run -p sfgn-cli -- gbnf path/to/grammar.ebnf --out target/grammar.gbnf
```

### Run Standalone Extractor Examples

```bash
cargo run -p sfgn-extract-fs --example inspect_workspace
cargo run -p sfgn-extract-code --example inspect_codebase -- src/flat_path.rs
cargo run -p sfgn-extract-docs --example inspect_docs -- README.md
```

## Library Usage

```rust
use sfgn_core::{Document, NodeHandle, SfgnBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = SfgnBuilder::new();
    builder.set_root_prop("project", "demo");

    let engine = builder.add_node(NodeHandle::ROOT, "StorageEngine");
    let wal = builder.add_node(NodeHandle::ROOT, "WAL");
    builder.add_prop(wal, "sync_mode", "fdatasync");
    builder.add_relative(engine, wal);

    let sfgn_text = builder.build()?;
    let doc = Document::parse(&sfgn_text)?;

    let wal_slice = doc.node_slice_by_path("\"WAL\"")?.expect("WAL node");
    println!("{}", wal_slice);
    Ok(())
}
```
