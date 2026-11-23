use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sfgn_core::{
    Document, EdgeTarget, FlatArtifactDomain, NodeHandle, Scalar, SfgnBuilder, Value,
    classify_workspace_file, flat_code_portal, flat_doc_portal,
};

use crate::node::{GraphNode, RevealLevel};

pub const MEMORY_GRAPH_DIR: &str = "sfgn-memories";
pub const MEMORY_INDEX_FILE: &str = "index.sfgn";

/// A single persisted memory note inside `sfgn-memories/<category>.sfgn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    pub id: String,
    pub category: String,
    pub entry_type: String,
    pub note: String,
    pub anchor_file: Option<String>,
    pub anchor_symbol: Option<String>,
}

/// Persistent cross-session memory graph store backed by `sfgn-memories/`.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    workspace_root: PathBuf,
}

impl MemoryStore {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.workspace_root.join(MEMORY_GRAPH_DIR)
    }

    /// Returns the Level 0 `sfgn-memories>:` root node listing available category shards.
    pub fn root_node(&self) -> GraphNode {
        let entries = self.load_all_entries();
        let mut categories: BTreeSet<String> = BTreeSet::new();
        for e in &entries {
            categories.insert(format!("{}/{}", MEMORY_GRAPH_DIR, e.category));
        }

        let mut root = GraphNode::new(MEMORY_GRAPH_DIR, RevealLevel::Level0Index, "folder");
        root.add_num_prop("total_entries", entries.len().to_string());
        root.children = categories.into_iter().collect();
        root
    }

    /// Inspects `"sfgn-memories/"` (Level 0) or `"sfgn-memories/<category>"` (Level 2 shard).
    pub fn inspect(&self, raw_address: &str) -> Result<GraphNode, String> {
        let trimmed = raw_address.trim().trim_end_matches(">:");
        let sub = trimmed
            .strip_prefix(MEMORY_GRAPH_DIR)
            .unwrap_or(trimmed)
            .trim_matches('/');

        if sub.is_empty() || sub == "index" || sub == "index.sfgn" {
            return Ok(self.root_node());
        }

        let category = sanitize_category(sub.strip_suffix(".sfgn").unwrap_or(sub));
        let shard_path = self.dir().join(format!("{}.sfgn", category));
        if !shard_path.is_file() {
            let existing: Vec<String> = self
                .list_categories()
                .into_iter()
                .map(|c| format!("{}/{}", MEMORY_GRAPH_DIR, c))
                .collect();
            return Err(format!(
                "Memory category '{}' has no entries yet. Existing shards: [{}]",
                category,
                existing.join(", ")
            ));
        }

        let raw = fs::read_to_string(&shard_path)
            .map_err(|e| format!("failed to read '{}': {}", shard_path.display(), e))?;
        let mut node = GraphNode::new(
            format!("{}/{}", MEMORY_GRAPH_DIR, category),
            RevealLevel::Level2Leaf,
            "memory_shard",
        );
        node.set_payload(Some("sfgn"), raw);
        Ok(node)
    }

    /// Appends a new [`MemoryEntry`] to `sfgn-memories/<category>.sfgn`, wires `>>` portals to
    /// `anchor_file` / `anchor_symbol`, and rebuilds `sfgn-memories/index.sfgn`.
    pub fn record(
        &self,
        category_raw: &str,
        entry_type_raw: &str,
        note: &str,
        anchor_file: Option<&str>,
        anchor_symbol: Option<&str>,
    ) -> Result<MemoryEntry, String> {
        let note_clean = note.trim();
        if note_clean.is_empty() {
            return Err("Memory `note` cannot be empty.".to_owned());
        }

        let category = sanitize_category(category_raw);
        let entry_type = sanitize_entry_type(entry_type_raw);
        let anchor_file_clean = anchor_file
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let anchor_symbol_clean = anchor_symbol
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);

        let mut existing = self.load_category_entries(&category);
        let id = format!("{}-{}", category, existing.len() + 1);
        let entry = MemoryEntry {
            id,
            category: category.clone(),
            entry_type,
            note: note_clean.to_owned(),
            anchor_file: anchor_file_clean,
            anchor_symbol: anchor_symbol_clean,
        };
        existing.push(entry.clone());

        fs::create_dir_all(self.dir())
            .map_err(|e| format!("failed to create '{}': {}", self.dir().display(), e))?;

        let shard_sfgn = build_category_sfgn(&category, &existing)?;
        let shard_path = self.dir().join(format!("{}.sfgn", category));
        fs::write(&shard_path, &shard_sfgn)
            .map_err(|e| format!("failed to write '{}': {}", shard_path.display(), e))?;

        self.rebuild_index()?;
        Ok(entry)
    }

    /// Searches all persisted memories by case-insensitive keyword overlap.
    pub fn recall(&self, query: &str) -> Vec<MemoryEntry> {
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();

        let all = self.load_all_entries();
        if tokens.is_empty() {
            return all;
        }

        all.into_iter()
            .filter(|entry| {
                let haystack = format!(
                    "{} {} {} {} {}",
                    entry.category,
                    entry.entry_type,
                    entry.note,
                    entry.anchor_file.as_deref().unwrap_or(""),
                    entry.anchor_symbol.as_deref().unwrap_or("")
                )
                .to_ascii_lowercase();
                tokens.iter().any(|tok| haystack.contains(tok))
            })
            .collect()
    }

    pub fn load_all_entries(&self) -> Vec<MemoryEntry> {
        let mut out = Vec::new();
        for cat in self.list_categories() {
            out.extend(self.load_category_entries(&cat));
        }
        out
    }

    fn list_categories(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.dir()) else {
            return Vec::new();
        };
        let mut cats = Vec::new();
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sfgn") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem != "index" {
                cats.push(stem.to_owned());
            }
        }
        cats.sort();
        cats
    }

    fn load_category_entries(&self, category: &str) -> Vec<MemoryEntry> {
        let path = self.dir().join(format!("{}.sfgn", category));
        let Ok(raw) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        let Ok(doc) = Document::parse(&raw) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        for node in doc.nodes() {
            if node.is_root() {
                continue;
            }
            let id = node
                .canonical_path()
                .and_then(|p| p.segments().last())
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "note".to_owned());

            let entry_type =
                extract_str_prop(node, "entry_type").unwrap_or_else(|| "summary".to_owned());
            let note = extract_str_prop(node, "note")
                .or_else(|| extract_multiline_prop(node, "note"))
                .unwrap_or_default();
            let anchor_file = extract_str_prop(node, "anchor_file");
            let anchor_symbol = extract_str_prop(node, "anchor_symbol");

            if !note.is_empty() {
                entries.push(MemoryEntry {
                    id,
                    category: category.to_owned(),
                    entry_type,
                    note,
                    anchor_file,
                    anchor_symbol,
                });
            }
        }
        entries
    }

    fn rebuild_index(&self) -> Result<(), String> {
        let entries = self.load_all_entries();
        let cats = self.list_categories();

        let mut builder = SfgnBuilder::new();
        builder.set_root_prop("kind", "memory_index");
        builder.set_root_prop("total_entries", entries.len() as u64);

        for cat in &cats {
            let portal = format!("{}/{}.sfgn", MEMORY_GRAPH_DIR, cat);
            builder.add_relative(NodeHandle::ROOT, EdgeTarget::external_root(portal));
        }

        let rendered = builder
            .build()
            .map_err(|e| format!("failed to build memory index: {}", e))?;
        let index_path = self.dir().join(MEMORY_INDEX_FILE);
        fs::write(&index_path, &rendered)
            .map_err(|e| format!("failed to write '{}': {}", index_path.display(), e))?;
        Ok(())
    }
}

fn build_category_sfgn(category: &str, entries: &[MemoryEntry]) -> Result<String, String> {
    let mut builder = SfgnBuilder::new();
    builder.set_root_prop("kind", "memory_shard");
    builder.set_root_prop("category", category);
    builder.set_root_prop("count", entries.len() as u64);

    for entry in entries {
        let handle = builder.add_node(NodeHandle::ROOT, &entry.id);
        builder.add_prop(handle, "kind", "memory_entry");
        builder.add_prop(handle, "category", entry.category.as_str());
        builder.add_prop(handle, "entry_type", entry.entry_type.as_str());
        builder.add_prop(handle, "note", entry.note.as_str());

        if let Some(file_str) = &entry.anchor_file {
            builder.add_prop(handle, "anchor_file", file_str.as_str());
            let rel_path = Path::new(file_str);
            match classify_workspace_file(rel_path) {
                FlatArtifactDomain::Code => {
                    let portal = flat_code_portal(rel_path);
                    builder.add_relative(handle, EdgeTarget::external_root(portal));
                }
                FlatArtifactDomain::DocOrMedia => {
                    let portal = flat_doc_portal(rel_path);
                    builder.add_relative(handle, EdgeTarget::external_root(portal));
                }
                FlatArtifactDomain::FsOnly => {}
            }
        }
        if let Some(sym_str) = &entry.anchor_symbol {
            builder.add_prop(handle, "anchor_symbol", sym_str.as_str());
        }
    }

    builder
        .build()
        .map_err(|e| format!("failed to compile memory shard '{}': {}", category, e))
}

fn sanitize_category(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match cleaned.trim_matches('_') {
        "" => "notes".to_owned(),
        "architecture" | "invariants" | "bugs" | "notes" => cleaned,
        other => other.to_owned(),
    }
}

fn sanitize_entry_type(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "rule" => "rule".to_owned(),
        "insight" => "insight".to_owned(),
        _ => "summary".to_owned(),
    }
}

fn extract_str_prop(node: sfgn_core::NodeView<'_, '_>, key: &str) -> Option<String> {
    let prop = node.property(key)?;
    match prop.decode().ok()? {
        Value::Scalar(Scalar::String(cow)) => Some(cow.into_owned()),
        _ => None,
    }
}

fn extract_multiline_prop(node: sfgn_core::NodeView<'_, '_>, key: &str) -> Option<String> {
    let prop = node.property(key)?;
    match prop.decode().ok()? {
        Value::Multiline(ml) => Some(ml.decode_text().into_owned()),
        _ => None,
    }
}
