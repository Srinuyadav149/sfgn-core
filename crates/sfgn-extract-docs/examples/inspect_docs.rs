use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use sfgn_core::{
    Document, MermaidDirection, flat_doc_artifact_rel, render_ascii_tree,
    render_mermaid_with_direction,
};
use sfgn_extract_docs::extract_doc_file_to_sfgn;

fn main() {
    let positionals: Vec<String> = env::args().skip(1).collect();
    let (workspace_root, rel_file) = resolve_workspace_and_doc(&positionals);

    let doc_sfgn = extract_doc_file_to_sfgn(&workspace_root, &rel_file).unwrap_or_else(|err| {
        eprintln!(
            "Failed to extract SFGN document graph for '{}': {}",
            rel_file.display(),
            err
        );
        std::process::exit(1);
    });

    let doc = Document::parse(&doc_sfgn).unwrap_or_else(|err| {
        eprintln!("Validation failed on generated document .sfgn: {}", err);
        std::process::exit(1);
    });

    let artifact_rel = flat_doc_artifact_rel(&rel_file);
    let out_file = Path::new("target").join(&artifact_rel);
    if let Some(parent) = out_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(&out_file, &doc_sfgn).is_ok() {
        println!(
            "Saved single-document SFGN graph to: {}\n",
            out_file.display()
        );
    }

    let total_child_edges: usize = doc.nodes().map(|n| n.children().count()).sum();
    let total_rel_edges: usize = doc.nodes().map(|n| n.relatives().count()).sum();
    let total_portals: usize = doc.nodes().map(|n| n.external_portals().count()).sum();

    println!("============================================================");
    println!(
        "1. GENERATED {} ({} nodes, {} > edges, {} >> anchors, {} external portals, {} bytes)",
        artifact_rel.display(),
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

fn resolve_workspace_and_doc(positionals: &[String]) -> (PathBuf, PathBuf) {
    match positionals.len() {
        0 => {
            let root = PathBuf::from(".");
            let default_doc = if root.join("README.md").is_file() {
                PathBuf::from("README.md")
            } else {
                PathBuf::from("ARCHITECTURE.md")
            };
            (root, default_doc)
        }
        1 => {
            let arg = PathBuf::from(&positionals[0]);
            if arg.is_file() {
                (PathBuf::from("."), arg)
            } else {
                let default_doc = if arg.join("README.md").is_file() {
                    PathBuf::from("README.md")
                } else {
                    PathBuf::from("ARCHITECTURE.md")
                };
                (arg, default_doc)
            }
        }
        _ => (
            PathBuf::from(&positionals[0]),
            PathBuf::from(&positionals[1]),
        ),
    }
}
