use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sfgn_core::{Document, Number, Scalar, Value};
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
        code_portal_file: Some("code.sfgn".to_owned()),
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
            assert_eq!(payload.raw, "let doc = Document::parse(src)?;");
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

    // Verify external portal `>> <"code.sfgn">"builder.rs"`
    let portals: Vec<(String, Option<String>)> = storage_tier
        .external_portals()
        .map(|p| (p.file_path.to_owned(), p.sub_path.map(|s| s.to_string())))
        .collect();
    assert_eq!(
        portals,
        vec![(
            "code.sfgn".to_owned(),
            Some(r###""builder.rs""###.to_owned())
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
