# sfgn_core

Zero-copy **Simple Flat Graph Notation (`SFGN v1.0`)** parser, builder, dual-topology visualizer, and multi-domain graph compiler workspace in Rust.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the internal Pass 1 / Pass 2 pipeline and formal grammar specification.

## Workspace Crates

- **`sfgn_core`** (`src/`): Zero-copy Pass 1 linear scanner (`StructuralTape`), Pass 2 topology indexer (`TopologyIndex`), `O(1)` node slicer (`Document`), collision-disambiguating graph compiler (`SfgnBuilder`), and ASCII/Mermaid visualizer (`viz`).
- **`sfgn-extract-fs`** (`crates/sfgn-extract-fs`): `.gitignore`-aware filesystem walker and `gix` repository analyzer emitting `fs.sfgn` with file stats, working-tree status, `last_commit`, and historical co-change `>>` edges.
- **`sfgn-extract-code`** (`crates/sfgn-extract-code`): Pure `tree-sitter` AST/symbol compiler emitting type-prefixed `code.sfgn` graphs (`struct`, `enum`, `trait`, `mod`, `fn`, `method`) and bidirectional disk line-span patching (`patch_node_source`). Defaults to `rust`; optional `python` and `typescript` features.
- **`sfgn-extract-docs`** (`crates/sfgn-extract-docs`): CommonMark/GFM outline (`H1 > H2 > H3`) and CSV table compiler (`doc.sfgn`), binary media header scanner (`PNG`, `JPEG`, `GIF`, `WebP`, `WAV`, `PDF`), and EBNF-to-GBNF grammar transpiler.
- **`sfgn-cli`** (`crates/sfgn-cli`): Unified `sfgn` command-line tool for compiling and inspecting `.sfgn` graphs.

## Quickstart

### Build & Run Tests

```bash
cargo test --workspace
```

To enable Python and TypeScript/JavaScript AST extraction in `sfgn-extract-code`:

```bash
cargo test -p sfgn-extract-code --all-features
```

### Compile & Inspect a Workspace with `sfgn`

```bash
# Compile fs.sfgn, code.sfgn, and doc.sfgn into target/sfgn/
cargo run -p sfgn-cli -- compile . --out-dir target/sfgn

# Validate a compiled graph
cargo run -p sfgn-cli -- check target/sfgn/code.sfgn

# Extract an O(1) zero-copy node slice and follow external <"fs.sfgn"> portals
cargo run -p sfgn-cli -- slice target/sfgn/doc.sfgn '"Core Pipeline"' --follow-portals

# Search headers, properties, and multiline payloads
cargo run -p sfgn-cli -- grep target/sfgn/code.sfgn "SfgnBuilder"

# Render an ASCII dual-topology tree or left-to-right Mermaid flowchart
cargo run -p sfgn-cli -- viz target/sfgn/doc.sfgn
cargo run -p sfgn-cli -- viz target/sfgn/doc.sfgn --mermaid --lr

# Transpile an EBNF grammar to llama.cpp GBNF
cargo run -p sfgn-cli -- gbnf path/to/grammar.ebnf --out target/grammar.gbnf
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
