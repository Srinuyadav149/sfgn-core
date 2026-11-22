use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sfgn_core::{Document, NodeHandle, Number, Scalar, SfgnBuilder, Value};
use sfgn_extract_docs::{
    DocsExtractOptions, DocsExtractor, compile_ebnf_to_gbnf, extract_docs_to_sfgn,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDocsDir {
    path: PathBuf,
}

impl TempDocsDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{}_{}_{}_{}", prefix, pid, nanos, seq));
        fs::create_dir_all(&path).expect("create temp docs dir");
        Self { path }
    }

    fn write_bytes(&self, rel: &str, bytes: &[u8]) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(full, bytes).expect("write file");
    }

    fn write_str(&self, rel: &str, content: &str) {
        self.write_bytes(rel, content.as_bytes());
    }
}

impl Drop for TempDocsDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn compiles_markdown_outline_tables_media_proxies_and_cross_links() {
    let dir = TempDocsDir::new("sfgn_docs_extract");

    dir.write_str(
        "docs/architecture.md",
        r###"# System Architecture

High-level overview of the storage engine.

## Storage Tier

See [Benchmark Results](#benchmark-results), [Deployment Guide](deploy.md#cluster-setup),
the diagram ![Diagram](../assets/diagram.png), and [Builder Source](../src/builder.rs).

### Benchmark Results

| Engine | Throughput (ops/s) | Latency p99 |
| :--- | ---: | ---: |
| SFGN Tape | 1250000 | 0.4ms |
| JSON DOM | 310000 | 2.1ms |

```rust
let doc = Document::parse(src)?;
```
"###,
    );

    dir.write_str(
        "docs/deploy.md",
        r###"# Deployment

## Cluster Setup

Run three replicas in production.
"###,
    );

    // Minimal valid 320x180 PNG header
    let mut png_bytes = Vec::new();
    png_bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    png_bytes.extend_from_slice(&13u32.to_be_bytes());
    png_bytes.extend_from_slice(b"IHDR");
    png_bytes.extend_from_slice(&320u32.to_be_bytes());
    png_bytes.extend_from_slice(&180u32.to_be_bytes());
    png_bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    dir.write_bytes("assets/diagram.png", &png_bytes);

    // Minimal 16kHz mono PCM WAV header (48,000 data bytes = 1.5s)
    let mut wav_bytes = Vec::new();
    wav_bytes.extend_from_slice(b"RIFF");
    wav_bytes.extend_from_slice(&36u32.to_le_bytes());
    wav_bytes.extend_from_slice(b"WAVE");
    wav_bytes.extend_from_slice(b"fmt ");
    wav_bytes.extend_from_slice(&16u32.to_le_bytes());
    wav_bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav_bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav_bytes.extend_from_slice(&16000u32.to_le_bytes()); // 16kHz
    wav_bytes.extend_from_slice(&32000u32.to_le_bytes()); // byte rate
    wav_bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav_bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav_bytes.extend_from_slice(b"data");
    wav_bytes.extend_from_slice(&48000u32.to_le_bytes());
    dir.write_bytes("assets/sample.wav", &wav_bytes);

    let extractor = DocsExtractor::new(DocsExtractOptions {
        fs_portal_file: Some("fs.sfgn".to_owned()),
        ..DocsExtractOptions::default()
    });
    let doc_sfgn = extractor.extract(&dir.path).expect("extract doc.sfgn");
    let doc = Document::parse(&doc_sfgn).expect("emitted doc.sfgn passes strict validation");

    // Verify hierarchical heading tree: `System Architecture > Storage Tier > Benchmark Results`
    let storage_tier = doc
        .node_by_path(r###""Storage Tier""###)
        .unwrap()
        .expect("Storage Tier section");

    let bench = doc
        .node_by_path(r###""Benchmark Results""###)
        .unwrap()
        .expect("Benchmark Results section");

    match bench.property("table").unwrap().decode().unwrap() {
        Value::Multiline(payload) => {
            assert_eq!(payload.tag, Some("csv"));
            assert!(
                payload
                    .raw
                    .contains("Engine,Throughput (ops/s),Latency p99")
            );
            assert!(payload.raw.contains("SFGN Tape,1250000,0.4ms"));
        }
        other => panic!("expected csv table multiline, got {:?}", other),
    }

    match bench.property("code").unwrap().decode().unwrap() {
        Value::Multiline(payload) => {
            assert_eq!(payload.tag, Some("rust"));
            assert_eq!(payload.raw.trim_end(), "let doc = Document::parse(src)?;");
        }
        other => panic!("expected rust code multiline, got {:?}", other),
    }

    // Verify `>>` relative links from `Storage Tier` to `#benchmark-results`, `deploy.md#cluster-setup`, and `diagram.png`
    let rels: Vec<String> = storage_tier
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert!(rels.contains(&r###""Benchmark Results""###.to_owned()));
    assert!(rels.contains(&r###""Cluster Setup""###.to_owned()));
    assert!(rels.contains(&r###""diagram.png""###.to_owned()));

    // Verify external portal `>> <"fs.sfgn">"src">"builder.rs"`
    let portals: Vec<(String, Option<String>)> = storage_tier
        .external_portals()
        .map(|p| (p.file_path.to_owned(), p.sub_path.map(|s| s.to_string())))
        .collect();
    assert_eq!(
        portals,
        vec![(
            "fs.sfgn".to_owned(),
            Some(r###""src">"builder.rs""###.to_owned())
        )]
    );

    // Verify PNG & WAV binary media proxy metadata
    let png_node = doc
        .node_by_path(r###""diagram.png""###)
        .unwrap()
        .expect("diagram.png proxy");
    assert_eq!(
        png_node.property("width").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::Number(Number::new("320")))
    );
    assert_eq!(
        png_node.property("height").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::Number(Number::new("180")))
    );

    let wav_node = doc
        .node_by_path(r###""sample.wav""###)
        .unwrap()
        .expect("sample.wav proxy");
    assert_eq!(
        wav_node
            .property("sample_rate_hz")
            .unwrap()
            .decode()
            .unwrap(),
        Value::Scalar(Scalar::Number(Number::new("16000")))
    );
    assert_eq!(
        wav_node
            .property("duration_secs")
            .unwrap()
            .decode()
            .unwrap(),
        Value::Scalar(Scalar::Number(Number::new("1.5")))
    );

    // Smoke test default convenience function
    assert!(extract_docs_to_sfgn(&dir.path).is_ok());
}

#[test]
fn resolves_full_path_external_portals_against_minimal_qualified_fs_graph() {
    let dir = TempDocsDir::new("sfgn_docs_full_portal");
    dir.write_str(
        "README.md",
        "# Docs\n\nSee [FS Lib](crates/sfgn-extract-fs/src/lib.rs) and [Core Lib](src/lib.rs).\n",
    );

    let extractor = DocsExtractor::new(DocsExtractOptions {
        fs_portal_file: Some("fs.sfgn".to_owned()),
        ..DocsExtractOptions::default()
    });
    let doc_sfgn = extractor.extract(&dir.path).expect("extract doc.sfgn");
    let doc = Document::parse(&doc_sfgn).expect("valid doc.sfgn");

    // Build a target `fs.sfgn` where `lib.rs` collides across crates and gets minimal-qualified
    let mut fs_builder = SfgnBuilder::new();
    let crates = fs_builder.add_node(NodeHandle::ROOT, "crates");
    let fs_crate = fs_builder.add_node(crates, "sfgn-extract-fs");
    let fs_src = fs_builder.add_node(fs_crate, "src");
    let fs_lib = fs_builder.add_node(fs_src, "lib.rs");
    fs_builder.add_prop(fs_lib, "crate", "sfgn-extract-fs");

    let root_src = fs_builder.add_node(NodeHandle::ROOT, "src");
    let core_lib = fs_builder.add_node(root_src, "lib.rs");
    fs_builder.add_prop(core_lib, "crate", "sfgn_core");

    let fs_sfgn = fs_builder.build().expect("build fs.sfgn");
    let fs_doc = Document::parse(&fs_sfgn).expect("valid fs.sfgn");
    let section = doc.node_by_path(r###""Docs""###).unwrap().unwrap();
    let resolved_crates: Vec<String> = section
        .external_portals()
        .map(|portal| {
            assert_eq!(portal.file_path, "fs.sfgn");
            let target = fs_doc
                .node_by_canonical_path(portal.sub_path.expect("sub_path"))
                .expect("full portal path resolves in fs.sfgn");
            match target.property("crate").unwrap().decode().unwrap() {
                Value::Scalar(scalar) => scalar.as_str().unwrap().to_owned(),
                other => panic!("expected scalar string, got {:?}", other),
            }
        })
        .collect();

    assert_eq!(resolved_crates, vec!["sfgn-extract-fs", "sfgn_core"]);
}

#[test]
fn compiles_ebnf_grammar_to_valid_llama_cpp_gbnf() {
    let ebnf = r###"
(* Minimal SFGN v1.0 EBNF subset *)
SfgnDocument = RootNode, { NodeBlock } ;
RootNode     = ">:", EOL, { IndentedItem } ;
NodeBlock    = NodePath, ":", EOL, { IndentedItem } ;
NodePath     = Segment, { ">", Segment } ;
Segment      = QuotedString, [ "#", Digit, { Digit } ] ;
Digit        = '0'..'9' ;
EOL          = #x0A | "\r\n" ;
"###;

    let gbnf = compile_ebnf_to_gbnf(ebnf).expect("compile EBNF to GBNF");
    assert!(gbnf.starts_with("root ::= sfgn-document\n"));
    assert!(gbnf.contains("sfgn-document ::= root-node (node-block)*"));
    assert!(gbnf.contains("segment ::= quoted-string (\"#\" digit (digit)*)?"));
    assert!(gbnf.contains("digit ::= [0-9]"));
    assert!(gbnf.contains("eol ::= \"\\x0a\" | \"\\r\\n\""));
}
#[test]
fn extracts_single_markdown_and_media_file_with_lazy_flat_portals() {
    let dir = TempDocsDir::new("sfgn_docs_single_file");

    dir.write_str(
        "docs/guide.md",
        r###"# User Guide

See [Details](#details), [Releasing](../resources/releasing.md),
[Main Source](../src/main.rs), [Banner](../resources/banner.svg),
and [Manifest](../Cargo.toml).

## Details

Local section content.
"###,
    );
    dir.write_str("resources/releasing.md", "# Releasing\n");
    dir.write_str("resources/banner.svg", "<svg></svg>\n");
    dir.write_str("src/main.rs", "fn main() {}\n");
    dir.write_str("Cargo.toml", "[package]\nname = \"demo\"\n");

    // Extract only `docs/guide.md` into its flat document graph
    let guide_sfgn = sfgn_extract_docs::extract_doc_file_to_sfgn(&dir.path, "docs/guide.md")
        .expect("extract single markdown file");
    let guide_doc = Document::parse(&guide_sfgn).expect("valid guide sfgn");

    assert!(guide_sfgn.contains("\"kind\": \"document\""));
    assert!(guide_sfgn.contains("\"source_file\": \"docs/guide.md\""));
    assert!(guide_sfgn.contains("\"User Guide\":"));
    assert!(guide_sfgn.contains("\"Details\":"));

    let user_guide = guide_doc
        .node_by_path(r###""User Guide""###)
        .unwrap()
        .expect("User Guide node");

    // Local `#details` anchor resolves to local section node
    let local_rels: Vec<String> = user_guide
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(local_rels, vec![r###""Details""###.to_owned()]);

    // External links resolve to flat `sfgn-docs/`, `sfgn-codes/`, and `fs.sfgn` portals
    let portals: Vec<(String, Option<String>)> = user_guide
        .external_portals()
        .map(|p| (p.file_path.to_owned(), p.sub_path.map(|s| s.to_string())))
        .collect();
    assert!(portals.contains(&("sfgn-docs/resources:releasing.md.sfgn".to_owned(), None)));
    assert!(portals.contains(&("sfgn-codes/src:main.rs.sfgn".to_owned(), None)));
    assert!(portals.contains(&("sfgn-docs/resources:banner.svg.sfgn".to_owned(), None)));
    assert!(portals.contains(&("fs.sfgn".to_owned(), Some(r###""Cargo.toml""###.to_owned()))));

    // Extract single media proxy (`resources/banner.svg`)
    let svg_sfgn = sfgn_extract_docs::extract_doc_file_to_sfgn(&dir.path, "resources/banner.svg")
        .expect("extract single svg media proxy");
    let svg_doc = Document::parse(&svg_sfgn).expect("valid svg sfgn");
    assert_eq!(
        svg_doc.root().property("kind").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("media_proxy".into()))
    );
    assert_eq!(
        svg_doc
            .root()
            .property("source_file")
            .unwrap()
            .decode()
            .unwrap(),
        Value::Scalar(Scalar::String("resources/banner.svg".into()))
    );
}
