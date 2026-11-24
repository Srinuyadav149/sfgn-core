use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use sfgn_core::{
    Document, NodeView, Scalar, Value, decode_flat_artifact_path, encode_flat_rel_path,
};

use crate::node::{GraphNode, RevealLevel};

/// Resolves reverse `>>` portals from `sfgn-memories/*.sfgn` and `sfgn-docs/*.sfgn`
/// pointing back to `rel_file` (and optionally `symbol`).
pub fn resolve_reverse_portals(
    workspace_root: &Path,
    rel_file: &str,
    symbol: Option<&str>,
) -> Vec<String> {
    let norm_file = normalize_rel(rel_file);
    let mut backlinks = BTreeSet::new();

    collect_memory_backlinks(workspace_root, &norm_file, symbol, &mut backlinks);
    collect_doc_backlinks(workspace_root, &norm_file, &mut backlinks);

    backlinks.into_iter().collect()
}

/// Resolves `inspect_node("sfgn-memories/<category>::<entry_id>")` into a Level 2 leaf node.
pub fn inspect_memory_entry(
    workspace_root: &Path,
    category: &str,
    entry_id: &str,
) -> Result<GraphNode, String> {
    let clean_cat = category.trim();
    let clean_id = entry_id.trim();
    let shard_path = workspace_root
        .join("sfgn-memories")
        .join(format!("{}.sfgn", clean_cat));

    let raw = fs::read_to_string(&shard_path).map_err(|_| {
        format!(
            "Memory category '{}' not found in sfgn-memories/",
            clean_cat
        )
    })?;
    let doc = Document::parse(&raw)
        .map_err(|e| format!("Invalid memory shard '{}.sfgn': {}", clean_cat, e))?;

    let mut available_ids = Vec::new();
    for node in doc.nodes() {
        if node.is_root() {
            continue;
        }
        let id = node_label(node);
        if id == clean_id {
            let mut leaf = GraphNode::new(
                format!("sfgn-memories/{}::{}", clean_cat, id),
                RevealLevel::Level2Leaf,
                "memory_entry",
            );
            leaf.add_str_prop("category", clean_cat);
            if let Some(et) = extract_str_prop(node, "entry_type") {
                leaf.add_str_prop("entry_type", et);
            }
            if let Some(af) = extract_str_prop(node, "anchor_file") {
                leaf.add_str_prop("anchor_file", &af);
                let target = match extract_str_prop(node, "anchor_symbol") {
                    Some(sym) if !sym.is_empty() => format!("{}::{}", af, sym),
                    _ => af,
                };
                leaf.relatives.push(target);
            }
            if let Some(asym) = extract_str_prop(node, "anchor_symbol") {
                leaf.add_str_prop("anchor_symbol", asym);
            }
            if let Some(note_text) = extract_str_prop(node, "note") {
                leaf.set_payload(Some("text"), note_text);
            }
            return Ok(leaf);
        }
        available_ids.push(format!("sfgn-memories/{}::{}", clean_cat, id));
    }

    Err(format!(
        "Memory entry '{}' not found in sfgn-memories/{}.sfgn. Available entries: [{}]",
        clean_id,
        clean_cat,
        available_ids.join(", ")
    ))
}

#[derive(Debug, Clone)]
struct ParsedMemoryEntry {
    id: String,
    anchor_file: Option<String>,
    anchor_symbol: Option<String>,
}

fn collect_memory_backlinks(
    workspace_root: &Path,
    norm_file: &str,
    symbol: Option<&str>,
    out: &mut BTreeSet<String>,
) {
    let mem_dir = workspace_root.join("sfgn-memories");
    let Ok(read_dir) = fs::read_dir(&mem_dir) else {
        return;
    };

    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sfgn") {
            continue;
        }
        let Some(category) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };

        for entry in parse_memory_shard_entries(&raw) {
            let Some(ref anchored) = entry.anchor_file else {
                continue;
            };
            if normalize_rel(anchored) != norm_file {
                continue;
            }
            let symbol_matches = match (symbol, entry.anchor_symbol.as_deref()) {
                (None, _) | (Some(_), None) => true,
                (Some(req_sym), Some(anc_sym)) => {
                    strip_kind_prefix(req_sym) == strip_kind_prefix(anc_sym)
                }
            };
            if symbol_matches {
                out.insert(format!("sfgn-memories/{}::{}", category, entry.id));
            }
        }
    }
}

fn collect_doc_backlinks(workspace_root: &Path, norm_file: &str, out: &mut BTreeSet<String>) {
    let docs_dir = workspace_root.join("sfgn-docs");
    let Ok(read_dir) = fs::read_dir(&docs_dir) else {
        return;
    };

    let code_shard_portal = format!(
        "sfgn-codes/{}.sfgn",
        encode_flat_rel_path(Path::new(norm_file))
    );

    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sfgn") {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };

        if raw.contains(&code_shard_portal) || raw.contains(norm_file) {
            let artifact_rel = format!("sfgn-docs/{}", file_name);
            if let Some(doc_path) = decode_flat_artifact_path(&artifact_rel) {
                out.insert(doc_path.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn parse_memory_shard_entries(raw: &str) -> Vec<ParsedMemoryEntry> {
    let Ok(doc) = Document::parse(raw) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for node in doc.nodes() {
        if node.is_root() {
            continue;
        }
        entries.push(ParsedMemoryEntry {
            id: node_label(node),
            anchor_file: extract_str_prop(node, "anchor_file"),
            anchor_symbol: extract_str_prop(node, "anchor_symbol"),
        });
    }
    entries
}

fn node_label(node: NodeView<'_, '_>) -> String {
    node.canonical_path()
        .and_then(|p| p.segments().last())
        .map(|s| s.name.clone())
        .unwrap_or_else(|| node.raw_header_path().trim_matches('"').to_owned())
}

fn extract_str_prop(node: NodeView<'_, '_>, key: &str) -> Option<String> {
    let prop = node.property(key)?;
    match prop.decode().ok()? {
        Value::Scalar(Scalar::String(cow)) => Some(cow.into_owned()),
        _ => None,
    }
}

fn normalize_rel(p: &str) -> String {
    let s = p.trim().replace('\\', "/");
    s.trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn strip_kind_prefix(sym: &str) -> &str {
    const PREFIXES: &[&str] = &[
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "method ",
        "mod ",
        "type ",
        "const ",
        "static ",
        "macro ",
        "class ",
        "interface ",
    ];
    let trimmed = sym.trim();
    for prefix in PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    trimmed
}
