use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use sfgn_core::{decode_flat_artifact_path, encode_flat_rel_path};

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

#[derive(Debug, Clone)]
struct ParsedMemoryEntry {
    id: String,
    entry_type: String,
    note: String,
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
    let mut entries = Vec::new();
    let mut current: Option<ParsedMemoryEntry> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') && trimmed.ends_with("\":") && !trimmed.contains(": ") {
            if let Some(prev) = current.take() {
                entries.push(prev);
            }
            let id = trimmed.trim_end_matches(':').trim_matches('"').to_owned();
            current = Some(ParsedMemoryEntry {
                id,
                entry_type: "note".to_owned(),
                note: String::new(),
                anchor_file: None,
                anchor_symbol: None,
            });
            continue;
        }

        let Some(ref mut entry) = current else {
            continue;
        };
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().trim_matches('"');
            let val = v.trim().trim_matches('"');
            match key {
                "entry_type" => entry.entry_type = val.to_owned(),
                "note" => entry.note = val.replace("\\\"", "\"").replace("\\n", "\n"),
                "anchor_file" if val != "null" => entry.anchor_file = Some(val.to_owned()),
                "anchor_symbol" if val != "null" => entry.anchor_symbol = Some(val.to_owned()),
                _ => {}
            }
        }
    }

    if let Some(last) = current {
        entries.push(last);
    }
    entries
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
