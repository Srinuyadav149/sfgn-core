# SFGN Core Workspace Architecture

Simple Flat Graph Notation (`SFGN v1.0`) is a zero-copy, dual-topology graph serialization format and progressive retrieval engine designed for bounded-context LLM agents.

## Workspace Architecture

The workspace separates the zero-copy core tape engine from domain-specific graph compilers. See [Extractor Crates](#extractor-crates) for domain compilers and [SFGN Grammar](#sfgn-grammar) for the formal syntax.

| Crate | Role | Output Artifact |
| :--- | :--- | :--- |
| `sfgn_core` | Zero-copy Pass 1 scanner, Pass 2 topology index, builder & visualizer | `.sfgn` tape & views |
| `sfgn-extract-fs` | `.gitignore`-aware filesystem & `gix` commit/co-change compiler | `fs.sfgn` |
| `sfgn-extract-code` | Feature-gated `tree-sitter` AST compiler & disk line-span patcher | `code.sfgn` |
| `sfgn-extract-docs` | CommonMark/GFM outline/table compiler, media proxy & GBNF transpiler | `doc.sfgn` / `.gbnf` |

### Core Pipeline

1. **Pass 1 Linear Scanner**: Scans UTF-8 bytes into flat `NodeEntry`, `PropertyEntry`, and `RelationEntry` tapes with `O(1)` auxiliary stack space.
2. **Pass 2 Topology Index**: Resolves canonical `NodePath` identifiers, verifies single-rooted reachability from `>:`, and builds `O(1)` lookup maps.
3. **Minimal Collision Qualification**: Implemented in [SfgnBuilder](src/builder.rs), automatically disambiguating sibling collisions (`#0`, `#1`) and cross-branch collisions (`"Parent">"Child"`).
4. **Dual-Topology Visualization**: Implemented in [viz.rs](src/viz.rs), rendering ASCII trees and Mermaid flowcharts (`graph TD` and `graph LR`).

```rust
use sfgn_core::{render_ascii_tree, Document};

let doc = Document::parse(sfgn_source)?;
println!("{}", render_ascii_tree(&doc));
```

## Extractor Crates

### Filesystem & Git Compiler

Implemented in [sfgn-extract-fs](crates/sfgn-extract-fs/src/lib.rs), traversing repositories via `ignore::WalkBuilder` and extracting working-tree status, `last_commit`, submodules, and co-modification `>>` edges via `gix`.

### Source Code AST Compiler

Implemented in [sfgn-extract-code](crates/sfgn-extract-code/src/lib.rs) with `default = ["rust"]` and opt-in `python` and `typescript` features. Bidirectional disk edits are handled by [patch.rs](crates/sfgn-extract-code/src/patch.rs) using `"source_file"` and `"line_span": [start, end]`.

### Documentation & Media Compiler

Implemented in [sfgn-extract-docs](crates/sfgn-extract-docs/src/lib.rs), compiling Markdown headings into `>` containment trees, tables into `"""csv` payloads, binary media headers into `media_proxy` nodes, and EBNF rules into `llama.cpp` GBNF via [gbnf.rs](crates/sfgn-extract-docs/src/gbnf.rs).

## SFGN Grammar

```ebnf
SfgnDocument = RootNode, { NodeBlock } ;
RootNode     = ">:", EOL, { IndentedItem } ;
NodeBlock    = NodePath, ":", EOL, { IndentedItem } ;
NodePath     = Segment, { ">", Segment } ;
Segment      = QuotedString, [ "#", Digit, { Digit } ] ;
```

