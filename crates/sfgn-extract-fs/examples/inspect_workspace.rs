use std::env;
use std::fs;
use std::path::Path;

use sfgn_core::{Document, render_ascii_tree, render_mermaid};
use sfgn_extract_fs::extract_fs_to_sfgn;

fn main() {
    let target = env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let target_path = Path::new(&target);

    let sfgn_doc_text = extract_fs_to_sfgn(target_path).unwrap_or_else(|err| {
        eprintln!("Failed to extract SFGN filesystem graph: {}", err);
        std::process::exit(1);
    });

    let doc = Document::parse(&sfgn_doc_text).unwrap_or_else(|err| {
        eprintln!("Validation failed on generated fs.sfgn: {}", err);
        std::process::exit(1);
    });

    // Save the generated file graph to `target/workspace_fs.sfgn` if `target/` exists
    let out_file = Path::new("target/workspace_fs.sfgn");
    if let Some(parent) = out_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(out_file, &sfgn_doc_text).is_ok() {
        println!("Saved generated SFGN document to: {}\n", out_file.display());
    }

    println!("============================================================");
    println!(
        "1. GENERATED fs.sfgn DOCUMENT ({} nodes, {} bytes)",
        doc.len(),
        sfgn_doc_text.len()
    );
    println!("============================================================");
    println!("{}", sfgn_doc_text);

    println!("============================================================");
    println!("2. VISUAL DUAL-TOPOLOGY TREE (> containment, >> relatives)");
    println!("============================================================");
    println!("{}", render_ascii_tree(&doc));

    println!("============================================================");
    println!("3. MERMAID FLOWCHART (paste into Mermaid Live / GitHub)");
    println!("============================================================");
    println!("{}", render_mermaid(&doc));
}
