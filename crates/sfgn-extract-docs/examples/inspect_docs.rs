use std::env;
use std::fs;
use std::path::Path;

use sfgn_core::{Document, MermaidDirection, render_ascii_tree, render_mermaid_with_direction};
use sfgn_extract_docs::{DocsExtractOptions, DocsExtractor};

fn main() {
    let target = env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let target_path = Path::new(&target);

    let extractor = DocsExtractor::new(DocsExtractOptions {
        fs_portal_file: Some("fs.sfgn".to_owned()),
        code_portal_file: Some("code.sfgn".to_owned()),
        ..DocsExtractOptions::default()
    });

    let doc_sfgn = extractor.extract(target_path).unwrap_or_else(|err| {
        eprintln!("Failed to extract SFGN documentation graph: {}", err);
        std::process::exit(1);
    });

    let doc = Document::parse(&doc_sfgn).unwrap_or_else(|err| {
        eprintln!("Validation failed on generated doc.sfgn: {}", err);
        std::process::exit(1);
    });

    let out_file = Path::new("target/workspace_docs.sfgn");
    if let Some(parent) = out_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(out_file, &doc_sfgn).is_ok() {
        println!("Saved generated doc.sfgn to: {}\n", out_file.display());
    }

    let total_child_edges: usize = doc.nodes().map(|n| n.children().count()).sum();
    let total_rel_edges: usize = doc.nodes().map(|n| n.relatives().count()).sum();
    let total_portals: usize = doc.nodes().map(|n| n.external_portals().count()).sum();

    println!("============================================================");
    println!(
        "1. GENERATED doc.sfgn ({} nodes, {} > edges, {} >> links, {} external portals, {} bytes)",
        doc.len(),
        total_child_edges,
        total_rel_edges,
        total_portals,
        doc_sfgn.len()
    );
    println!("============================================================");
    println!("{}", doc_sfgn);

    println!("============================================================");
    println!("2. VISUAL DUAL-TOPOLOGY DOC TREE (> sections, >> anchors/portals)");
    println!("============================================================");
    println!("{}", render_ascii_tree(&doc));

    println!("============================================================");
    println!("3. MERMAID FLOWCHART (graph LR)");
    println!("============================================================");
    println!(
        "{}",
        render_mermaid_with_direction(&doc, MermaidDirection::LeftRight)
    );
}
