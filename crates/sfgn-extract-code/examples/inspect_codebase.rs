use std::env;
use std::fs;
use std::path::Path;

use sfgn_core::{Document, MermaidDirection, render_ascii_tree, render_mermaid_with_direction};
use sfgn_extract_code::{CodeExtractOptions, CodeExtractor, extract_code_to_sfgn};

fn main() {
    let mut target = ".".to_owned();
    let mut print_bodies = false;

    for arg in env::args().skip(1) {
        if arg == "--bodies" {
            print_bodies = true;
        } else {
            target = arg;
        }
    }

    let target_path = Path::new(&target);

    // 1. Compile the full `code.sfgn` (including verbatim `"""rust` bodies) and validate via Pass 1 + 2
    let full_code_sfgn = extract_code_to_sfgn(target_path).unwrap_or_else(|err| {
        eprintln!("Failed to extract SFGN code graph: {}", err);
        std::process::exit(1);
    });

    let full_doc = Document::parse(&full_code_sfgn).unwrap_or_else(|err| {
        eprintln!("Validation failed on generated code.sfgn: {}", err);
        std::process::exit(1);
    });

    let out_file = Path::new("target/workspace_code.sfgn");
    if let Some(parent) = out_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(out_file, &full_code_sfgn).is_ok() {
        println!(
            "Saved full code.sfgn (with verbatim code blocks) to: {}\n",
            out_file.display()
        );
    }

    let total_child_edges: usize = full_doc.nodes().map(|n| n.children().count()).sum();
    let total_rel_edges: usize = full_doc.nodes().map(|n| n.relatives().count()).sum();

    // 2. For terminal readability, omit `"""` code bodies unless `--bodies` is passed
    let display_sfgn = if print_bodies {
        full_code_sfgn.clone()
    } else {
        CodeExtractor::new(CodeExtractOptions {
            include_bodies: false,
            ..CodeExtractOptions::default()
        })
        .extract(target_path)
        .unwrap_or_else(|err| {
            eprintln!("Failed to extract structural code.sfgn: {}", err);
            std::process::exit(1);
        })
    };

    println!("============================================================");
    println!(
        "1. GENERATED code.sfgn ({} nodes, {} > edges, {} >> call/type/import edges, {} bytes full)",
        full_doc.len(),
        total_child_edges,
        total_rel_edges,
        full_code_sfgn.len()
    );
    if !print_bodies {
        println!("   (showing structural view; pass `--bodies` to print inline \"\"\"rust blocks)");
    }
    println!("============================================================");
    println!("{}", display_sfgn);

    println!("============================================================");
    println!("2. VISUAL DUAL-TOPOLOGY AST & SYMBOL TREE (> containment, >> calls/types)");
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
