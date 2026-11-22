use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process;

use sfgn_core::{
    CODE_GRAPH_DIR, DOC_GRAPH_DIR, Document, FS_GRAPH_FILE, FlatArtifactDomain, MermaidDirection,
    classify_workspace_file, decode_flat_artifact_path, flat_code_artifact_rel,
    flat_doc_artifact_rel, is_generated_sfgn_artifact, render_ascii_tree,
    render_mermaid_with_direction,
};
use sfgn_extract_code::{CodeExtractOptions, CodeExtractor, patch_node_source};
use sfgn_extract_docs::{
    DocsExtractOptions, DocsExtractor, classify_media_extension, compile_ebnf_to_gbnf,
};
use sfgn_extract_fs::FsExtractor;

const HELP_TEXT: &str = "\
sfgn - Simple Flat Graph Notation (SFGN v1.0) workspace compiler & tape inspector

USAGE:
    sfgn <COMMAND> [OPTIONS]

COMMANDS:
    compile <ROOT_DIR> [--out-dir <DIR>] [--eager-all] [--no-bodies] [--public-only] [--no-media]
        Compile a workspace into eager fs.sfgn and flat sfgn-codes/ + sfgn-docs/
        directories at the workspace root (default output directory: <ROOT_DIR>).
        With --eager-all, pre-populates all per-file .sfgn graphs upfront.

    hydrate <ROOT_DIR> <REL_PATH> [--force] [--print] [--no-bodies] [--public-only] [--no-media]
        Lazily compile a single workspace file into sfgn-codes/<path>.sfgn or
        sfgn-docs/<path>.sfgn (using ':' as path separator), skipping work if
        the cached artifact is newer than the source file.

    check <FILE.sfgn>
        Validate an SFGN v1.0 file through Pass 1 (scanner) and Pass 2 (topology).

    slice <FILE.sfgn> <NODE_PATH> [--follow-portals] [--root <WORKSPACE_ROOT>]
        Extract the exact zero-copy UTF-8 slice for <NODE_PATH> (e.g. \">:\",
        \"src\">\"builder.rs\", \"fn main\"). With --follow-portals, auto-hydrates
        and prints any linked <\"sfgn-codes/...\">, <\"sfgn-docs/...\">, or <\"fs.sfgn\"> slices.

    grep <FILE.sfgn> <QUERY> [--case-sensitive]
        Zero-copy linear scan across node headers, properties, and multiline blocks.

    viz <FILE.sfgn> [--mermaid] [--td | --lr] [--out <OUTPUT_FILE>]
        Render an SFGN graph as an ASCII dual-topology tree (default) or Mermaid
        flowchart (--mermaid, defaulting to --lr).

    patch <WORKSPACE_ROOT> <CODE_SFGN> <NODE_PATH> (--body <CODE> | --body-file <FILE>)
        Splice an updated code payload back into the original source file on disk
        using the node's \"source_file\" and \"line_span\" properties, and refresh
        the cached sfgn-codes/ graph.

    gbnf <INPUT.ebnf> [--out <OUTPUT.gbnf>]
        Transpile an EBNF grammar specification into a llama.cpp GBNF grammar.
";

#[derive(Debug, Clone, Copy)]
struct HydrateConfig {
    include_bodies: bool,
    include_private: bool,
    include_media_proxies: bool,
    force: bool,
}

impl Default for HydrateConfig {
    fn default() -> Self {
        Self {
            include_bodies: true,
            include_private: true,
            include_media_proxies: true,
            force: false,
        }
    }
}

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
        "hydrate" => cmd_hydrate(&args[1..]),
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
    let mut eager_all = false;
    let mut cfg = HydrateConfig {
        force: true,
        ..HydrateConfig::default()
    };

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
            "--eager-all" => eager_all = true,
            "--no-bodies" => cfg.include_bodies = false,
            "--public-only" => cfg.include_private = false,
            "--no-media" => cfg.include_media_proxies = false,
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
    let target_dir = out_dir.unwrap_or_else(|| root.clone());
    fs::create_dir_all(&target_dir).map_err(|e| {
        format!(
            "failed to create output directory '{}': {}",
            target_dir.display(),
            e
        )
    })?;

    let codes_dir = target_dir.join(CODE_GRAPH_DIR);
    let docs_dir = target_dir.join(DOC_GRAPH_DIR);
    fs::create_dir_all(&codes_dir).map_err(|e| {
        format!(
            "failed to create code graph directory '{}': {}",
            codes_dir.display(),
            e
        )
    })?;
    fs::create_dir_all(&docs_dir).map_err(|e| {
        format!(
            "failed to create doc graph directory '{}': {}",
            docs_dir.display(),
            e
        )
    })?;

    // 1. Eager root `fs.sfgn`
    let fs_sfgn = FsExtractor::default()
        .extract(&root)
        .map_err(|e| format!("fs extraction failed: {}", e))?;
    let fs_path = target_dir.join(FS_GRAPH_FILE);
    fs::write(&fs_path, &fs_sfgn)
        .map_err(|e| format!("failed to write '{}': {}", fs_path.display(), e))?;

    println!(
        "Initialized SFGN workspace graphs in {}:",
        target_dir.display()
    );
    print_graph_summary(FS_GRAPH_FILE, &fs_sfgn)?;

    if eager_all {
        let mut hydrated_codes = 0usize;
        let mut hydrated_docs = 0usize;
        let mut rel_files = Vec::new();
        collect_workspace_rel_files(&root, &root, &mut rel_files)?;
        rel_files.sort();

        for rel_file in rel_files {
            if let Some((artifact_abs, _)) =
                ensure_hydrated_artifact(&root, &target_dir, &rel_file, cfg)?
            {
                if artifact_abs.starts_with(&codes_dir) {
                    hydrated_codes += 1;
                } else {
                    hydrated_docs += 1;
                }
            }
        }
        println!(
            "  {:<10} {} hydrated files in {}/",
            "codes", hydrated_codes, CODE_GRAPH_DIR
        );
        println!(
            "  {:<10} {} hydrated files in {}/",
            "docs", hydrated_docs, DOC_GRAPH_DIR
        );
    } else {
        println!(
            "  {:<10} ready for lazy hydration ({}/ and {}/)",
            "lazy", CODE_GRAPH_DIR, DOC_GRAPH_DIR
        );
    }

    Ok(())
}

fn cmd_hydrate(args: &[String]) -> Result<(), String> {
    let mut positionals: Vec<String> = Vec::new();
    let mut print_slice = false;
    let mut cfg = HydrateConfig::default();

    for arg in args {
        match arg.as_str() {
            "--force" | "-f" => cfg.force = true,
            "--print" => print_slice = true,
            "--no-bodies" => cfg.include_bodies = false,
            "--public-only" => cfg.include_private = false,
            "--no-media" => cfg.include_media_proxies = false,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option '{}' for `sfgn hydrate`", flag));
            }
            val => positionals.push(val.to_owned()),
        }
    }

    if positionals.len() != 2 {
        return Err(
            "usage: sfgn hydrate <ROOT_DIR> <REL_PATH> [--force] [--print] [--no-bodies] [--public-only] [--no-media]"
                .to_owned(),
        );
    }

    let root = Path::new(&positionals[0]);
    let rel_path = Path::new(&positionals[1]);

    let Some((artifact_abs, recompiled)) = ensure_hydrated_artifact(root, root, rel_path, cfg)?
    else {
        return Err(format!(
            "file '{}' is tracked in {} only (not a code, Markdown, or media asset)",
            rel_path.display(),
            FS_GRAPH_FILE
        ));
    };

    let raw = fs::read_to_string(&artifact_abs)
        .map_err(|e| format!("failed to read '{}': {}", artifact_abs.display(), e))?;
    let status = if recompiled { "Hydrated" } else { "Up-to-date" };
    println!("{} {}:", status, artifact_abs.display());
    print_graph_summary(
        artifact_abs
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact.sfgn"),
        &raw,
    )?;

    if print_slice {
        println!();
        print!("{}", raw);
        if !raw.ends_with('\n') {
            println!();
        }
    }

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
    let mut explicit_root: Option<PathBuf> = None;
    let mut follow_portals = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--follow-portals" => follow_portals = true,
            "--root" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "--root requires a workspace directory path".to_owned())?;
                explicit_root = Some(PathBuf::from(val));
            }
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
        i += 1;
    }

    let path = file_path.ok_or_else(|| {
        "usage: sfgn slice <FILE.sfgn> <NODE_PATH> [--follow-portals] [--root <WORKSPACE_ROOT>]"
            .to_owned()
    })?;
    let query_path = node_path.ok_or_else(|| {
        "usage: sfgn slice <FILE.sfgn> <NODE_PATH> [--follow-portals] [--root <WORKSPACE_ROOT>]"
            .to_owned()
    })?;

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
        let ws_root = explicit_root.unwrap_or_else(|| detect_workspace_root(&path));
        for portal in node.external_portals() {
            // Auto-hydrate lazy flat portal targets (`sfgn-codes/...` or `sfgn-docs/...`) on demand
            if let Some(rel_src) = decode_flat_artifact_path(portal.file_path) {
                let _ = ensure_hydrated_artifact(
                    &ws_root,
                    &ws_root,
                    &rel_src,
                    HydrateConfig::default(),
                )?;
            }

            let target_file = ws_root.join(portal.file_path);
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

    // Refresh the flat `sfgn-codes/<encoded>.sfgn` file so updated line_spans are immediately live
    if code_sfgn_path.parent().and_then(|p| p.file_name()) == Some(OsStr::new(CODE_GRAPH_DIR)) {
        if let Some(file_name) = code_sfgn_path.file_name().and_then(|n| n.to_str()) {
            let artifact_rel = format!("{}/{}", CODE_GRAPH_DIR, file_name);
            if let Some(rel_src) = decode_flat_artifact_path(&artifact_rel) {
                let out_root = code_sfgn_path
                    .parent()
                    .and_then(|p| p.parent())
                    .unwrap_or(workspace_root);
                let _ = ensure_hydrated_artifact(
                    workspace_root,
                    out_root,
                    &rel_src,
                    HydrateConfig {
                        force: true,
                        ..HydrateConfig::default()
                    },
                )?;
            }
        }
    }

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

// ============================================================================
// Flat Workspace Root & Lazy Hydration Helpers
// ============================================================================

fn detect_workspace_root(sfgn_file: &Path) -> PathBuf {
    let resolved = fs::canonicalize(sfgn_file).unwrap_or_else(|_| sfgn_file.to_path_buf());
    let Some(parent) = resolved.parent() else {
        return PathBuf::from(".");
    };

    if matches!(
        parent.file_name().and_then(|n| n.to_str()),
        Some(CODE_GRAPH_DIR | DOC_GRAPH_DIR)
    ) {
        if let Some(ws_root) = parent.parent() {
            return ws_root.to_path_buf();
        }
    }

    for ancestor in parent.ancestors() {
        if ancestor.join(FS_GRAPH_FILE).is_file() {
            return ancestor.to_path_buf();
        }
    }

    parent.to_path_buf()
}

fn ensure_hydrated_artifact(
    workspace_root: &Path,
    out_root: &Path,
    rel_source: &Path,
    cfg: HydrateConfig,
) -> Result<Option<(PathBuf, bool)>, String> {
    let clean_rel = normalize_rel_path(rel_source)?;
    let ext = clean_rel.extension().and_then(|e| e.to_str()).unwrap_or("");

    let domain = match classify_workspace_file(&clean_rel) {
        FlatArtifactDomain::Code => FlatArtifactDomain::Code,
        FlatArtifactDomain::DocOrMedia => FlatArtifactDomain::DocOrMedia,
        FlatArtifactDomain::FsOnly => {
            if cfg.include_media_proxies && classify_media_extension(ext).is_some() {
                FlatArtifactDomain::DocOrMedia
            } else {
                return Ok(None);
            }
        }
    };

    let artifact_rel = match domain {
        FlatArtifactDomain::Code => flat_code_artifact_rel(&clean_rel),
        FlatArtifactDomain::DocOrMedia => flat_doc_artifact_rel(&clean_rel),
        FlatArtifactDomain::FsOnly => unreachable!(),
    };

    let abs_source = workspace_root.join(&clean_rel);
    let abs_artifact = out_root.join(&artifact_rel);

    if !cfg.force && abs_artifact.is_file() {
        if let (Ok(src_meta), Ok(art_meta)) =
            (fs::metadata(&abs_source), fs::metadata(&abs_artifact))
        {
            if let (Ok(src_mtime), Ok(art_mtime)) = (src_meta.modified(), art_meta.modified()) {
                if art_mtime >= src_mtime {
                    return Ok(Some((abs_artifact, false)));
                }
            }
        }
    }

    let rendered = match domain {
        FlatArtifactDomain::Code => CodeExtractor::new(CodeExtractOptions {
            include_bodies: cfg.include_bodies,
            include_private: cfg.include_private,
            ..CodeExtractOptions::default()
        })
        .extract_file(workspace_root, &clean_rel)
        .map_err(|e| format!("code hydration failed for '{}': {}", clean_rel.display(), e))?,
        FlatArtifactDomain::DocOrMedia => DocsExtractor::new(DocsExtractOptions {
            include_media_proxies: cfg.include_media_proxies,
            ..DocsExtractOptions::default()
        })
        .extract_file(workspace_root, &clean_rel)
        .map_err(|e| format!("doc hydration failed for '{}': {}", clean_rel.display(), e))?,
        FlatArtifactDomain::FsOnly => unreachable!(),
    };

    if let Some(parent) = abs_artifact.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {}", parent.display(), e))?;
    }
    fs::write(&abs_artifact, &rendered)
        .map_err(|e| format!("failed to write '{}': {}", abs_artifact.display(), e))?;

    Ok(Some((abs_artifact, true)))
}

fn normalize_rel_path(rel_path: &Path) -> Result<PathBuf, String> {
    let mut clean = PathBuf::new();
    for comp in rel_path.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(os) => clean.push(os),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "path '{}' must be relative to the workspace root",
                    rel_path.display()
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("source relative path cannot be empty".to_owned());
    }
    Ok(clean)
}

fn collect_workspace_rel_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(current)
        .map_err(|e| format!("failed to read directory '{}': {}", current.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("directory entry error: {}", e))?;
        let path = entry.path();
        let file_name = entry.file_name();
        if file_name == ".git" || file_name == "target" {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        if is_generated_sfgn_artifact(rel) {
            continue;
        }
        if path.is_dir() {
            collect_workspace_rel_files(root, &path, out)?;
        } else if path.is_file() {
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}
