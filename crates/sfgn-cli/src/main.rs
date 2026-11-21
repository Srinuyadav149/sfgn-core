use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use sfgn_core::{Document, MermaidDirection, render_ascii_tree, render_mermaid_with_direction};
use sfgn_extract_code::{CodeExtractOptions, CodeExtractor, patch_node_source};
use sfgn_extract_docs::{DocsExtractOptions, DocsExtractor, compile_ebnf_to_gbnf};
use sfgn_extract_fs::FsExtractor;

const HELP_TEXT: &str = "\
sfgn - Simple Flat Graph Notation (SFGN v1.0) workspace compiler & tape inspector

USAGE:
    sfgn <COMMAND> [OPTIONS]

COMMANDS:
    compile <ROOT_DIR> [--out-dir <DIR>] [--no-bodies] [--public-only] [--no-media]
        Compile a workspace into federated fs.sfgn, code.sfgn, and doc.sfgn graphs
        (default output directory: <ROOT_DIR>/.sfgn).

    check <FILE.sfgn>
        Validate an SFGN v1.0 file through Pass 1 (scanner) and Pass 2 (topology).

    slice <FILE.sfgn> <NODE_PATH> [--follow-portals]
        Extract the exact zero-copy UTF-8 slice for <NODE_PATH> (e.g. \">:\",
        \"src\">\"builder.rs\", \"struct SfgnBuilder\"). With --follow-portals,
        also resolves and prints any linked <\"other.sfgn\">[SubPath] slices.

    grep <FILE.sfgn> <QUERY> [--case-sensitive]
        Zero-copy linear scan across node headers, properties, and multiline blocks.

    viz <FILE.sfgn> [--mermaid] [--td | --lr] [--out <OUTPUT_FILE>]
        Render an SFGN graph as an ASCII dual-topology tree (default) or Mermaid
        flowchart (--mermaid, defaulting to --lr).

    patch <WORKSPACE_ROOT> <CODE_SFGN> <NODE_PATH> (--body <CODE> | --body-file <FILE>)
        Splice an updated code payload back into the original source file on disk
        using the node's \"source_file\" and \"line_span\" properties.

    gbnf <INPUT.ebnf> [--out <OUTPUT.gbnf>]
        Transpile an EBNF grammar specification into a llama.cpp GBNF grammar.
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print!("{}", HELP_TEXT);
        return;
    }
    if matches!(args[0].as_str(), "-V" | "--version") {
        println!("sfgn {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let result = match args[0].as_str() {
        "compile" => cmd_compile(&args[1..]),
        "check" => cmd_check(&args[1..]),
        "slice" => cmd_slice(&args[1..]),
        "grep" => cmd_grep(&args[1..]),
        "viz" => cmd_viz(&args[1..]),
        "patch" => cmd_patch(&args[1..]),
        "gbnf" => cmd_gbnf(&args[1..]),
        other => Err(format!(
            "unknown subcommand '{}'\n\nRun `sfgn --help` for usage.",
            other
        )),
    };

    if let Err(err) = result {
        eprintln!("error: {}", err);
        process::exit(1);
    }
}

fn cmd_compile(args: &[String]) -> Result<(), String> {
    let mut root_dir: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut include_bodies = true;
    let mut include_private = true;
    let mut include_media_proxies = true;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out-dir" | "-o" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--out-dir requires a directory path".to_owned())?;
                out_dir = Some(PathBuf::from(val));
            }
            "--no-bodies" => include_bodies = false,
            "--public-only" => include_private = false,
            "--no-media" => include_media_proxies = false,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn compile`", flag));
            }
            positional => {
                if root_dir.is_some() {
                    return Err(format!("unexpected positional argument '{}'", positional));
                }
                root_dir = Some(PathBuf::from(positional));
            }
        }
        i += 1;
    }

    let root = root_dir.unwrap_or_else(|| PathBuf::from("."));
    let target_dir = out_dir.unwrap_or_else(|| root.join(".sfgn"));
    fs::create_dir_all(&target_dir).map_err(|e| {
        format!(
            "failed to create output directory '{}': {}",
            target_dir.display(),
            e
        )
    })?;

    // 1. fs.sfgn
    let fs_sfgn = FsExtractor::default()
        .extract(&root)
        .map_err(|e| format!("fs extraction failed: {}", e))?;
    let fs_path = target_dir.join("fs.sfgn");
    fs::write(&fs_path, &fs_sfgn)
        .map_err(|e| format!("failed to write '{}': {}", fs_path.display(), e))?;

    // 2. code.sfgn
    let code_sfgn = CodeExtractor::new(CodeExtractOptions {
        include_bodies,
        include_private,
        ..CodeExtractOptions::default()
    })
    .extract(&root)
    .map_err(|e| format!("code extraction failed: {}", e))?;
    let code_path = target_dir.join("code.sfgn");
    fs::write(&code_path, &code_sfgn)
        .map_err(|e| format!("failed to write '{}': {}", code_path.display(), e))?;

    // 3. doc.sfgn (federated with fs.sfgn & code.sfgn portals)
    let doc_sfgn = DocsExtractor::new(DocsExtractOptions {
        include_media_proxies,
        fs_portal_file: Some("fs.sfgn".to_owned()),
        code_portal_file: Some("code.sfgn".to_owned()),
        ..DocsExtractOptions::default()
    })
    .extract(&root)
    .map_err(|e| format!("docs extraction failed: {}", e))?;
    let doc_path = target_dir.join("doc.sfgn");
    fs::write(&doc_path, &doc_sfgn)
        .map_err(|e| format!("failed to write '{}': {}", doc_path.display(), e))?;

    println!(
        "Compiled SFGN workspace graphs into {}:",
        target_dir.display()
    );
    print_graph_summary("fs.sfgn", &fs_sfgn)?;
    print_graph_summary("code.sfgn", &code_sfgn)?;
    print_graph_summary("doc.sfgn", &doc_sfgn)?;

    Ok(())
}

fn print_graph_summary(label: &str, raw_sfgn: &str) -> Result<(), String> {
    let doc = Document::parse(raw_sfgn)
        .map_err(|e| format!("validation failed on generated {}: {}", label, e))?;
    let child_edges: usize = doc.nodes().map(|n| n.children().count()).sum();
    let rel_edges: usize = doc.nodes().map(|n| n.relatives().count()).sum();
    let portals: usize = doc.nodes().map(|n| n.external_portals().count()).sum();
    println!(
        "  {:<10} {:>4} nodes | {:>4} > edges | {:>4} >> edges | {:>3} portals | {:>7} bytes",
        label,
        doc.len(),
        child_edges,
        rel_edges,
        portals,
        raw_sfgn.len()
    );
    Ok(())
}

fn cmd_check(args: &[String]) -> Result<(), String> {
    let file_arg = args
        .first()
        .ok_or_else(|| "usage: sfgn check <FILE.sfgn>".to_owned())?;
    let path = Path::new(file_arg);
    let bytes =
        fs::read(path).map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;

    let doc =
        Document::parse_bytes(&bytes).map_err(|diag| format!("{}: {}", path.display(), diag))?;

    let child_edges: usize = doc.nodes().map(|n| n.children().count()).sum();
    let rel_edges: usize = doc.nodes().map(|n| n.relatives().count()).sum();
    let portals: usize = doc.nodes().map(|n| n.external_portals().count()).sum();

    println!(
        "OK: {} ({} nodes, {} > edges, {} >> edges, {} external portals, {} bytes)",
        path.display(),
        doc.len(),
        child_edges,
        rel_edges,
        portals,
        bytes.len()
    );
    Ok(())
}

fn cmd_slice(args: &[String]) -> Result<(), String> {
    let mut file_path: Option<PathBuf> = None;
    let mut node_path: Option<String> = None;
    let mut follow_portals = false;

    for arg in args {
        match arg.as_str() {
            "--follow-portals" => follow_portals = true,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn slice`", flag));
            }
            val => {
                if file_path.is_none() {
                    file_path = Some(PathBuf::from(val));
                } else if node_path.is_none() {
                    node_path = Some(val.to_owned());
                } else {
                    return Err(format!("unexpected argument '{}'", val));
                }
            }
        }
    }

    let path = file_path
        .ok_or_else(|| "usage: sfgn slice <FILE.sfgn> <NODE_PATH> [--follow-portals]".to_owned())?;
    let query_path = node_path
        .ok_or_else(|| "usage: sfgn slice <FILE.sfgn> <NODE_PATH> [--follow-portals]".to_owned())?;

    let bytes =
        fs::read(&path).map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
    let doc =
        Document::parse_bytes(&bytes).map_err(|diag| format!("{}: {}", path.display(), diag))?;

    let node = doc
        .node_by_path(&query_path)
        .map_err(|diag| format!("invalid NodePath '{}': {}", query_path, diag))?
        .ok_or_else(|| format!("node '{}' not found in {}", query_path, path.display()))?;

    print!("{}", node.raw_slice());
    if !node.raw_slice().ends_with('\n') {
        println!();
    }

    if follow_portals {
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        for portal in node.external_portals() {
            let target_file = base_dir.join(portal.file_path);
            let Ok(target_bytes) = fs::read(&target_file) else {
                eprintln!(
                    "warning: could not open portal target '{}'",
                    target_file.display()
                );
                continue;
            };
            let target_doc = Document::parse_bytes(&target_bytes)
                .map_err(|diag| format!("{}: {}", target_file.display(), diag))?;

            let resolved_slice = match portal.sub_path {
                Some(sub) => target_doc
                    .node_by_canonical_path(sub)
                    .map(|n| n.raw_slice()),
                None => Some(target_doc.root().raw_slice()),
            };

            if let Some(slice) = resolved_slice {
                let label = match portal.sub_path {
                    Some(sub) => format!("<\"{}\">{}", portal.file_path, sub),
                    None => format!("<\"{}\">", portal.file_path),
                };
                println!("\n# --- Resolved Portal: {} ---", label);
                print!("{}", slice);
                if !slice.ends_with('\n') {
                    println!();
                }
            }
        }
    }

    Ok(())
}

fn cmd_grep(args: &[String]) -> Result<(), String> {
    let mut file_path: Option<PathBuf> = None;
    let mut pattern: Option<String> = None;
    let mut case_sensitive = false;

    for arg in args {
        match arg.as_str() {
            "--case-sensitive" | "-s" => case_sensitive = true,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn grep`", flag));
            }
            val => {
                if file_path.is_none() {
                    file_path = Some(PathBuf::from(val));
                } else if pattern.is_none() {
                    pattern = Some(val.to_owned());
                } else {
                    return Err(format!("unexpected argument '{}'", val));
                }
            }
        }
    }

    let path = file_path
        .ok_or_else(|| "usage: sfgn grep <FILE.sfgn> <QUERY> [--case-sensitive]".to_owned())?;
    let query = pattern
        .ok_or_else(|| "usage: sfgn grep <FILE.sfgn> <QUERY> [--case-sensitive]".to_owned())?;

    let bytes =
        fs::read(&path).map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
    let doc =
        Document::parse_bytes(&bytes).map_err(|diag| format!("{}: {}", path.display(), diag))?;

    let needle = if case_sensitive {
        query.clone()
    } else {
        query.to_ascii_lowercase()
    };

    let matches_query = |haystack: &str| -> bool {
        if case_sensitive {
            haystack.contains(&needle)
        } else {
            haystack.to_ascii_lowercase().contains(&needle)
        }
    };

    let mut match_count = 0usize;
    for node in doc.nodes() {
        let header_display = if node.is_root() {
            ">:".to_owned()
        } else {
            node.canonical_path()
                .map(|p| p.to_string())
                .unwrap_or_else(|| node.raw_header_path().to_owned())
        };

        let mut hit_reasons: Vec<String> = Vec::new();
        if matches_query(&header_display) {
            hit_reasons.push("header".to_owned());
        }
        for prop in node.properties() {
            let key = prop.key_name();
            if matches_query(&key) || matches_query(prop.raw_value()) {
                hit_reasons.push(format!("prop \"{}\"", key));
            }
        }

        if !hit_reasons.is_empty() {
            match_count += 1;
            println!("{}  [matched: {}]", header_display, hit_reasons.join(", "));
        }
    }

    if match_count == 0 {
        println!("No matching nodes found for '{}'.", query);
    }
    Ok(())
}

fn cmd_viz(args: &[String]) -> Result<(), String> {
    let mut file_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut use_mermaid = false;
    let mut direction = MermaidDirection::LeftRight;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mermaid" => use_mermaid = true,
            "--td" => {
                use_mermaid = true;
                direction = MermaidDirection::TopDown;
            }
            "--lr" => {
                use_mermaid = true;
                direction = MermaidDirection::LeftRight;
            }
            "--out" | "-o" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--out requires an output file path".to_owned())?;
                out_path = Some(PathBuf::from(val));
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn viz`", flag));
            }
            val => {
                if file_path.is_some() {
                    return Err(format!("unexpected positional argument '{}'", val));
                }
                file_path = Some(PathBuf::from(val));
            }
        }
        i += 1;
    }

    let path = file_path.ok_or_else(|| {
        "usage: sfgn viz <FILE.sfgn> [--mermaid] [--td | --lr] [--out <OUTPUT_FILE>]".to_owned()
    })?;
    let bytes =
        fs::read(&path).map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
    let doc =
        Document::parse_bytes(&bytes).map_err(|diag| format!("{}: {}", path.display(), diag))?;

    let rendered = if use_mermaid {
        render_mermaid_with_direction(&doc, direction)
    } else {
        render_ascii_tree(&doc)
    };

    if let Some(out) = out_path {
        if let Some(parent) = out.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&out, &rendered)
            .map_err(|e| format!("failed to write '{}': {}", out.display(), e))?;
        println!("Wrote visualization to {}", out.display());
    } else {
        print!("{}", rendered);
    }

    Ok(())
}

fn cmd_patch(args: &[String]) -> Result<(), String> {
    let mut positionals: Vec<String> = Vec::new();
    let mut body_inline: Option<String> = None;
    let mut body_file: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--body" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--body requires replacement source text".to_owned())?;
                body_inline = Some(val.clone());
            }
            "--body-file" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--body-file requires a file path".to_owned())?;
                body_file = Some(PathBuf::from(val));
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn patch`", flag));
            }
            val => positionals.push(val.to_owned()),
        }
        i += 1;
    }

    if positionals.len() != 3 {
        return Err(
            "usage: sfgn patch <WORKSPACE_ROOT> <CODE_SFGN> <NODE_PATH> (--body <CODE> | --body-file <FILE>)"
                .to_owned(),
        );
    }

    let new_body = match (body_inline, body_file) {
        (Some(text), None) => text,
        (None, Some(path)) => fs::read_to_string(&path)
            .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?,
        _ => {
            return Err(
                "specify exactly one of `--body <CODE>` or `--body-file <FILE>`".to_owned(),
            );
        }
    };

    let workspace_root = Path::new(&positionals[0]);
    let code_sfgn_path = Path::new(&positionals[1]);
    let node_path = &positionals[2];

    let bytes = fs::read(code_sfgn_path)
        .map_err(|e| format!("failed to read '{}': {}", code_sfgn_path.display(), e))?;
    let doc = Document::parse_bytes(&bytes)
        .map_err(|diag| format!("{}: {}", code_sfgn_path.display(), diag))?;

    let node = doc
        .node_by_path(node_path)
        .map_err(|diag| format!("invalid NodePath '{}': {}", node_path, diag))?
        .ok_or_else(|| {
            format!(
                "node '{}' not found in {}",
                node_path,
                code_sfgn_path.display()
            )
        })?;

    patch_node_source(workspace_root, node, &new_body)
        .map_err(|e| format!("patch failed: {}", e))?;

    println!("Patched {} via {}", node_path, code_sfgn_path.display());
    Ok(())
}

fn cmd_gbnf(args: &[String]) -> Result<(), String> {
    let mut input_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" | "-o" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--out requires an output file path".to_owned())?;
                out_path = Some(PathBuf::from(val));
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn gbnf`", flag));
            }
            val => {
                if input_path.is_some() {
                    return Err(format!("unexpected positional argument '{}'", val));
                }
                input_path = Some(PathBuf::from(val));
            }
        }
        i += 1;
    }

    let path = input_path
        .ok_or_else(|| "usage: sfgn gbnf <INPUT.ebnf> [--out <OUTPUT.gbnf>]".to_owned())?;
    let ebnf = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read '{}': {}", path.display(), e))?;
    let gbnf = compile_ebnf_to_gbnf(&ebnf).map_err(|e| format!("{}", e))?;

    if let Some(out) = out_path {
        if let Some(parent) = out.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&out, &gbnf)
            .map_err(|e| format!("failed to write '{}': {}", out.display(), e))?;
        println!("Compiled GBNF grammar to {}", out.display());
    } else {
        print!("{}", gbnf);
    }

    Ok(())
}
