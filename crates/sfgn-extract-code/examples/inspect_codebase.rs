use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use sfgn_core::{
    Document, MermaidDirection, flat_code_artifact_rel, render_ascii_tree,
    render_mermaid_with_direction,
};
use sfgn_extract_code::{CodeExtractOptions, CodeExtractor, extract_code_file_to_sfgn};

fn main() {
    let mut positionals: Vec<String> = Vec::new();
    let mut print_bodies = false;

    for arg in env::args().skip(1) {
        if arg == "--bodies" {
            print_bodies = true;
        } else {
            positionals.push(arg);
        }
    }

    let (workspace_root, rel_file) = resolve_workspace_and_file(&positionals);

    // 1. Lazily compile only the selected source file into its flat `sfgn-codes/<path>.sfgn` graph
    let full_code_sfgn =
        extract_code_file_to_sfgn(&workspace_root, &rel_file).unwrap_or_else(|err| {
            eprintln!(
                "Failed to extract SFGN code graph for '{}': {}",
                rel_file.display(),
                err
            );
            std::process::exit(1);
        });

    let full_doc = Document::parse(&full_code_sfgn).unwrap_or_else(|err| {
        eprintln!("Validation failed on generated code_file .sfgn: {}", err);
        std::process::exit(1);
    });

    let artifact_rel = flat_code_artifact_rel(&rel_file);
    let out_file = Path::new("target").join(&artifact_rel);
    if let Some(parent) = out_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(&out_file, &full_code_sfgn).is_ok() {
        println!(
            "Saved single-file code graph (with verbatim code blocks) to: {}\n",
            out_file.display()
        );
    }

    let total_child_edges: usize = full_doc.nodes().map(|n| n.children().count()).sum();
    let total_rel_edges: usize = full_doc.nodes().map(|n| n.relatives().count()).sum();
    let total_portals: usize = full_doc.nodes().map(|n| n.external_portals().count()).sum();

    // 2. For terminal readability, omit `"""` code bodies unless `--bodies` is passed
    let display_sfgn = if print_bodies {
        full_code_sfgn.clone()
    } else {
        CodeExtractor::new(CodeExtractOptions {
            include_bodies: false,
            ..CodeExtractOptions::default()
        })
        .extract_file(&workspace_root, &rel_file)
        .unwrap_or_else(|err| {
            eprintln!("Failed to extract structural code_file .sfgn: {}", err);
            std::process::exit(1);
        })
    };

    println!("============================================================");
    println!(
        "1. GENERATED {} ({} nodes, {} > edges, {} >> local edges, {} portals, {} bytes full)",
        artifact_rel.display(),
        full_doc.len(),
        total_child_edges,
        total_rel_edges,
        total_portals,
        full_code_sfgn.len()
    );
    if !print_bodies {
        println!("   (showing structural view; pass `--bodies` to print inline \"\"\"rust blocks)");
    }
    println!("============================================================");
    println!("{}", display_sfgn);

    println!("============================================================");
    println!("2. VISUAL DUAL-TOPOLOGY SYMBOL TREE (> containment, >> calls/portals)");
    println!("============================================================");
    println!("{}", render_ascii_tree(&full_doc));

    println!("============================================================");
    println!("3. MERMAID FLOWCHART (graph LR - paste into Mermaid Live / GitHub)");
    println!("============================================================");
    println!(
        "{}",
        render_mermaid_with_direction(&full_doc, MermaidDirection::LeftRight)
    );
}

fn resolve_workspace_and_file(positionals: &[String]) -> (PathBuf, PathBuf) {
    match positionals.len() {
        0 => {
            let root = PathBuf::from(".");
            let default_file = if root.join("src/lib.rs").is_file() {
                PathBuf::from("src/lib.rs")
            } else {
                PathBuf::from("src/main.rs")
            };
            (root, default_file)
        }
        1 => {
            let arg = PathBuf::from(&positionals[0]);
            if arg.is_file() {
                (PathBuf::from("."), arg)
            } else {
                let default_file = if arg.join("src/lib.rs").is_file() {
                    PathBuf::from("src/lib.rs")
                } else {
                    PathBuf::from("src/main.rs")
                };
                (arg, default_file)
            }
        }
        _ => (
            PathBuf::from(&positionals[0]),
            PathBuf::from(&positionals[1]),
        ),
    }
}
