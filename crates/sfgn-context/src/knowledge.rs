use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use sfgn_core::{
    CODE_GRAPH_DIR, DOC_GRAPH_DIR, Document, FS_GRAPH_FILE, FlatArtifactDomain, NodeView, Scalar,
    Value, classify_workspace_file, flat_code_artifact_rel, flat_doc_artifact_rel,
    is_generated_sfgn_artifact,
};
use sfgn_extract_code::{CodeExtractOptions, CodeExtractor, patch_node_source};
use sfgn_extract_docs::{DocsExtractOptions, DocsExtractor, classify_media_extension};
use sfgn_extract_fs::FsExtractor;

use crate::memory::{MEMORY_GRAPH_DIR, MemoryStore};
use crate::node::{GraphNode, RevealLevel};

/// Structured lookup or execution error with an actionable recovery hint for `History`.
#[derive(Debug, Clone)]
pub struct InspectError {
    pub message: String,
    pub hint: Option<String>,
}

impl InspectError {
    pub fn new(message: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            message: message.into(),
            hint,
        }
    }
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " (Hint: {})", h)?;
        }
        Ok(())
    }
}

/// Lazy SFGN workspace graph engine over `fs.sfgn`, `sfgn-codes/`, `sfgn-docs/`, and `sfgn-memories/`.
#[derive(Debug, Clone)]
pub struct WorkspaceGraph {
    root: PathBuf,
}

impl WorkspaceGraph {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            root: workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Ensures `fs.sfgn` exists at the workspace root (compiling it eagerly if missing).
    pub fn ensure_fs_graph(&self) -> Result<PathBuf, String> {
        let fs_path = self.root.join(FS_GRAPH_FILE);
        if !fs_path.is_file() {
            self.refresh_fs_graph()?;
        }
        Ok(fs_path)
    }

    /// Re-compiles `fs.sfgn` from the current directory tree.
    pub fn refresh_fs_graph(&self) -> Result<(), String> {
        let rendered = FsExtractor::default()
            .extract(&self.root)
            .map_err(|e| format!("failed to extract {}: {}", FS_GRAPH_FILE, e))?;
        let fs_path = self.root.join(FS_GRAPH_FILE);
        fs::write(&fs_path, &rendered)
            .map_err(|e| format!("failed to write '{}': {}", fs_path.display(), e))?;
        Ok(())
    }

    /// Returns the Level 0 root directory node (`workspace>:`).
    pub fn workspace_root_node(&self) -> Result<GraphNode, InspectError> {
        self.inspect_directory(".")
    }

    /// Universal read-only graph traversal primitive (`inspect_node(address)`), enriched with
    /// bidirectional reverse portals (`sfgn-memories/` and `sfgn-docs/` -> code).
    pub fn inspect_node(&self, raw_address: &str) -> Result<GraphNode, InspectError> {
        let mut node = self.inspect_node_inner(raw_address)?;
        attach_reverse_portals(&self.root, &mut node);
        Ok(node)
    }

    fn inspect_node_inner(&self, raw_address: &str) -> Result<GraphNode, InspectError> {
        let trimmed = raw_address.trim().trim_end_matches(">:");
        if trimmed.is_empty() || trimmed == "." || trimmed == "workspace" {
            return self.inspect_directory(".");
        }

        // Catch tool addresses passed to `inspect_node` by mistake
        if trimmed == "tools" || trimmed.starts_with("tools/") {
            let tool_target = if trimmed == "tools" {
                "tools/"
            } else {
                trimmed
            };
            return Err(InspectError::new(
                format!("'{}' is a tool node, not a data node", raw_address),
                Some(format!(
                    "Call {{\"action\":\"inspect_tool\",\"tool\":\"{}\"}} instead.",
                    tool_target
                )),
            ));
        }

        if trimmed == MEMORY_GRAPH_DIR || trimmed.starts_with("sfgn-memories/") {
            if let Some(rest) = trimmed.strip_prefix("sfgn-memories/")
                && let Some((cat, entry_id)) = rest.split_once("::")
            {
                return crate::portals::inspect_memory_entry(&self.root, cat, entry_id)
                    .map_err(|msg| InspectError::new(msg, None));
            }

            let mem = MemoryStore::new(&self.root);
            return mem.inspect(trimmed).map_err(|msg| {
                InspectError::new(
                    msg,
                    Some(
                        "sfgn-memories/ has no entries yet. Call {\"action\":\"inspect_tool\",\"tool\":\"tools/memory/\"} to unlock record_memory."
                            .to_owned(),
                    ),
                )
            });
        }

        // Symbol or section address (`file::symbol_or_section`)
        if let Some((file_part, sub_part)) = trimmed.split_once("::") {
            let clean_file =
                normalize_rel_posix(file_part).map_err(|e| InspectError::new(e, None))?;
            let sub = sub_part.trim();
            if sub.is_empty() {
                return Err(InspectError::new(
                    format!(
                        "Empty symbol/section target after '::' in '{}'",
                        raw_address
                    ),
                    Some(format!(
                        "Call {{\"action\":\"inspect_node\",\"address\":\"{}\"}} first to list its '>' children.",
                        clean_file
                    )),
                ));
            }
            return self.inspect_file_member(&clean_file, sub);
        }

        // Explicit folder (`src/`)
        if trimmed.ends_with('/') {
            let clean_dir = normalize_rel_posix(trimmed.trim_end_matches('/'))
                .map_err(|e| InspectError::new(e, None))?;
            return self.inspect_directory(&clean_dir);
        }

        let clean_rel = normalize_rel_posix(trimmed).map_err(|e| InspectError::new(e, None))?;
        let abs_target = self.root.join(&clean_rel);

        if abs_target.is_dir() {
            return self.inspect_directory(&clean_rel);
        }

        if abs_target.is_file() {
            return self.inspect_file_outline(&clean_rel);
        }

        let suggestions = self.suggest_paths(&clean_rel);
        let hint = if suggestions.is_empty() {
            Some(
                "List a parent folder ending with '/' (e.g. \"src/\") to see valid files."
                    .to_owned(),
            )
        } else {
            Some(format!(
                "Path not found. Call inspect_node with one of: {}",
                suggestions.join(", ")
            ))
        };
        Err(InspectError::new(
            format!("Node '{}' not found in workspace", raw_address),
            hint,
        ))
    }

    /// Patches a code symbol on disk via `sfgn-extract-code::patch_node_source` and immediately
    /// refreshes the cached `sfgn-codes/<encoded>.sfgn` graph.
    pub fn patch_symbol(
        &self,
        rel_file: &str,
        symbol_query: &str,
        new_code: &str,
    ) -> Result<String, InspectError> {
        let clean_rel = normalize_rel_posix(rel_file).map_err(|e| InspectError::new(e, None))?;
        let artifact_abs = self
            .ensure_hydrated_file(Path::new(&clean_rel), false)
            .map_err(|e| InspectError::new(e, None))?
            .ok_or_else(|| {
                InspectError::new(
                    format!("'{}' is not a supported source code file", clean_rel),
                    None,
                )
            })?;

        let raw = fs::read_to_string(&artifact_abs)
            .map_err(|e| InspectError::new(format!("failed to read graph: {}", e), None))?;
        let doc = Document::parse(&raw)
            .map_err(|e| InspectError::new(format!("invalid SFGN graph: {}", e), None))?;

        let matched = resolve_symbol_in_doc(&doc, symbol_query).ok_or_else(|| {
            let available = list_doc_child_labels(&doc, &clean_rel);
            InspectError::new(
                format!("Symbol '{}' not found in '{}'", symbol_query, clean_rel),
                Some(format!(
                    "Use one of these exact symbol names: {}",
                    available.join(", ")
                )),
            )
        })?;

        patch_node_source(&self.root, matched, new_code).map_err(|e| {
            InspectError::new(
                format!("Failed to splice patch into '{}': {}", clean_rel, e),
                Some(
                    "Ensure `code` contains the complete replacement item (signature + body)."
                        .to_owned(),
                ),
            )
        })?;

        let _ = self
            .ensure_hydrated_file(Path::new(&clean_rel), true)
            .map_err(|e| InspectError::new(e, None))?;

        Ok(format!(
            "Patched {}::{} and refreshed {}",
            clean_rel,
            symbol_query,
            flat_code_artifact_rel(Path::new(&clean_rel)).display()
        ))
    }

    /// Creates a new file on disk, refreshes `fs.sfgn`, and hydrates its `.sfgn` graph if applicable.
    pub fn create_file(&self, rel_file: &str, content: &str) -> Result<String, InspectError> {
        let clean_rel = normalize_rel_posix(rel_file).map_err(|e| InspectError::new(e, None))?;
        let abs_path = self.root.join(&clean_rel);
        if abs_path.exists() {
            return Err(InspectError::new(
                format!("File '{}' already exists", clean_rel),
                Some(format!(
                    "Inspect \"{}\" and use `patch_symbol` instead of `create_file`.",
                    clean_rel
                )),
            ));
        }

        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                InspectError::new(format!("failed to create parent directory: {}", e), None)
            })?;
        }
        fs::write(&abs_path, content).map_err(|e| {
            InspectError::new(format!("failed to write '{}': {}", clean_rel, e), None)
        })?;

        let _ = self.refresh_fs_graph();
        let _ = self.ensure_hydrated_file(Path::new(&clean_rel), true);
        Ok(format!("Created '{}'", clean_rel))
    }

    /// Lists entries inside a directory (`"."` or `"src"`), appending `/` to directory children.
    pub fn list_dir(&self, raw_dir: &str) -> Result<Vec<String>, InspectError> {
        let trimmed = raw_dir.trim().trim_end_matches('/');
        let rel = if trimmed.is_empty() || trimmed == "." {
            String::new()
        } else {
            normalize_rel_posix(trimmed).map_err(|e| InspectError::new(e, None))?
        };

        let abs_dir = if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(&rel)
        };

        if !abs_dir.is_dir() {
            let suggestions = self.suggest_paths(&rel);
            let hint = if suggestions.is_empty() {
                None
            } else {
                Some(format!("Did you mean: {}?", suggestions.join(", ")))
            };
            return Err(InspectError::new(
                format!("Directory '{}' not found", raw_dir),
                hint,
            ));
        }

        let entries = fs::read_dir(&abs_dir)
            .map_err(|e| InspectError::new(format!("failed to read directory: {}", e), None))?;

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str == ".git"
                || name_str == "target"
                || name_str == "sfgn-logs"
                || name_str == MEMORY_GRAPH_DIR
            {
                continue;
            }
            let child_rel = if rel.is_empty() {
                PathBuf::from(name_str.as_ref())
            } else {
                Path::new(&rel).join(name_str.as_ref())
            };
            if is_generated_sfgn_artifact(&child_rel) {
                continue;
            }

            let display = child_rel.to_string_lossy().replace('\\', "/");
            if entry.path().is_dir() {
                dirs.push(format!("{}/", display));
            } else {
                files.push(display);
            }
        }

        dirs.sort();
        files.sort();
        dirs.extend(files);
        Ok(dirs)
    }

    /// Substring search across non-ignored workspace text files.
    pub fn search_text(
        &self,
        query: &str,
        path_prefix: Option<&str>,
    ) -> Result<Vec<String>, InspectError> {
        let q = query.trim();
        if q.is_empty() {
            return Err(InspectError::new("Search query cannot be empty", None));
        }

        let base_rel = match path_prefix
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != ".")
        {
            Some(p) => normalize_rel_posix(p.trim_end_matches('/'))
                .map_err(|e| InspectError::new(e, None))?,
            None => String::new(),
        };

        let start_path = if base_rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(&base_rel)
        };
        if !start_path.exists() {
            return Err(InspectError::new(
                format!("Search path_prefix '{}' does not exist", base_rel),
                Some("Pass `null` for `path_prefix` to search the entire workspace.".to_owned()),
            ));
        }

        let mut hits = Vec::new();
        self.walk_text_files(&start_path, &mut |rel_posix, text| {
            if hits.len() >= 30 {
                return;
            }
            for (idx, line) in text.lines().enumerate() {
                if line.contains(q) {
                    hits.push(format!("{}:{} | {}", rel_posix, idx + 1, line.trim()));
                    if hits.len() >= 30 {
                        break;
                    }
                }
            }
        });

        Ok(hits)
    }

    /// Searches section titles and prose across Markdown files in the workspace.
    pub fn search_docs(&self, query: &str) -> Result<Vec<String>, InspectError> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return Err(InspectError::new("Doc search query cannot be empty", None));
        }

        let mut hits = Vec::new();
        self.walk_text_files(&self.root, &mut |rel_posix, text| {
            if hits.len() >= 20 {
                return;
            }
            let lower_rel = rel_posix.to_ascii_lowercase();
            if !(lower_rel.ends_with(".md") || lower_rel.ends_with(".markdown")) {
                return;
            }
            let mut current_heading = "Top".to_owned();
            for (idx, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                if let Some(h) = trimmed.strip_prefix('#') {
                    current_heading = h.trim_start_matches('#').trim().to_owned();
                }
                if trimmed.to_ascii_lowercase().contains(&q) {
                    hits.push(format!(
                        "{}::{} (line {}) | {}",
                        rel_posix,
                        current_heading,
                        idx + 1,
                        trimmed
                    ));
                    if hits.len() >= 20 {
                        break;
                    }
                }
            }
        });

        Ok(hits)
    }

    // ------------------------------------------------------------------------
    // Internal Projection & Hydration Helpers
    // ------------------------------------------------------------------------

    fn inspect_directory(&self, rel_dir: &str) -> Result<GraphNode, InspectError> {
        let _ = self.ensure_fs_graph();
        let children = self.list_dir(rel_dir)?;
        let addr = if rel_dir == "." || rel_dir.is_empty() {
            "workspace".to_owned()
        } else {
            format!("{}/", rel_dir.trim_end_matches('/'))
        };

        let mut node = GraphNode::new(addr, RevealLevel::Level0Index, "folder");
        node.children = children;
        Ok(node)
    }

    fn inspect_file_outline(&self, clean_rel: &str) -> Result<GraphNode, InspectError> {
        let rel_path = Path::new(clean_rel);
        match classify_workspace_file(rel_path) {
            FlatArtifactDomain::Code => {
                let artifact_abs = self
                    .ensure_hydrated_file(rel_path, false)
                    .map_err(|e| InspectError::new(e, None))?
                    .unwrap();
                let raw = fs::read_to_string(&artifact_abs).map_err(|e| {
                    InspectError::new(format!("failed to read artifact: {}", e), None)
                })?;
                let doc = Document::parse(&raw)
                    .map_err(|e| InspectError::new(format!("invalid code SFGN: {}", e), None))?;

                let mut node = GraphNode::new(clean_rel, RevealLevel::Level1Outline, "file");
                if let Some(lang) = extract_str_prop(doc.root(), "language") {
                    node.add_str_prop("language", lang);
                }
                if let Some(doc_str) = extract_str_prop(doc.root(), "doc") {
                    node.add_str_prop("summary", doc_str);
                }

                let mut child_entries = Vec::new();
                for child in doc.root().children() {
                    let label = node_display_label(child);
                    let sig = extract_str_prop(child, "signature").unwrap_or_default();
                    if sig.is_empty() {
                        child_entries.push(format!("{}::{}", clean_rel, label));
                    } else {
                        child_entries.push(format!("{}::{} — {}", clean_rel, label, sig));
                    }
                    for grand in child.children() {
                        let g_label = grand
                            .canonical_path()
                            .and_then(|p| p.segments().last())
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| node_display_label(grand));
                        let g_sig = extract_str_prop(grand, "signature").unwrap_or_default();
                        if g_sig.is_empty() {
                            child_entries.push(format!("{}::{}", clean_rel, g_label));
                        } else {
                            child_entries.push(format!("{}::{} — {}", clean_rel, g_label, g_sig));
                        }
                    }
                }
                node.children = child_entries;
                node.relatives = collect_node_relatives(doc.root());
                Ok(node)
            }
            FlatArtifactDomain::DocOrMedia => {
                let artifact_abs = self
                    .ensure_hydrated_file(rel_path, false)
                    .map_err(|e| InspectError::new(e, None))?
                    .unwrap();
                let raw = fs::read_to_string(&artifact_abs).map_err(|e| {
                    InspectError::new(format!("failed to read artifact: {}", e), None)
                })?;
                let doc = Document::parse(&raw)
                    .map_err(|e| InspectError::new(format!("invalid doc SFGN: {}", e), None))?;

                let root_kind =
                    extract_str_prop(doc.root(), "kind").unwrap_or_else(|| "document".to_owned());
                if root_kind == "media_proxy" {
                    let mut node =
                        GraphNode::new(clean_rel, RevealLevel::Level2Leaf, "media_proxy");
                    for prop in doc.root().properties() {
                        let k = prop.key_name();
                        if k != "kind" && k != "source_file" {
                            node.add_str_prop(k.as_ref(), prop.raw_value().trim_matches('"'));
                        }
                    }
                    return Ok(node);
                }

                let mut node = GraphNode::new(clean_rel, RevealLevel::Level1Outline, "document");
                let mut sections = Vec::new();
                for n in doc.nodes() {
                    if n.is_root() {
                        continue;
                    }
                    let title = n
                        .canonical_path()
                        .and_then(|p| p.segments().last())
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| node_display_label(n));
                    sections.push(format!("{}::{}", clean_rel, title));
                }
                node.children = sections;
                node.relatives = collect_node_relatives(doc.root());
                Ok(node)
            }
            FlatArtifactDomain::FsOnly => {
                let abs_path = self.root.join(rel_path);
                let text = fs::read_to_string(&abs_path).map_err(|e| {
                    InspectError::new(format!("failed to read '{}': {}", clean_rel, e), None)
                })?;
                let ext = rel_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("text");
                let line_count = text.lines().count();
                let mut node = GraphNode::new(clean_rel, RevealLevel::Level2Leaf, "file");
                node.add_num_prop("lines", line_count.to_string());
                let capped = cap_payload(&text, 12_000);
                node.set_payload(Some(ext), capped);
                Ok(node)
            }
        }
    }

    fn inspect_file_member(
        &self,
        clean_rel: &str,
        member_query: &str,
    ) -> Result<GraphNode, InspectError> {
        let rel_path = Path::new(clean_rel);
        match classify_workspace_file(rel_path) {
            FlatArtifactDomain::Code => {
                let artifact_abs = self
                    .ensure_hydrated_file(rel_path, false)
                    .map_err(|e| InspectError::new(e, None))?
                    .unwrap();
                let raw = fs::read_to_string(&artifact_abs).map_err(|e| {
                    InspectError::new(format!("failed to read artifact: {}", e), None)
                })?;
                let doc = Document::parse(&raw)
                    .map_err(|e| InspectError::new(format!("invalid code SFGN: {}", e), None))?;

                let matched = resolve_symbol_in_doc(&doc, member_query).ok_or_else(|| {
                    let available = list_doc_child_labels(&doc, clean_rel);
                    InspectError::new(
                        format!("Symbol '{}' not found in '{}'", member_query, clean_rel),
                        Some(format!(
                            "Call inspect_node with one of these exact addresses: {}",
                            available.join(", ")
                        )),
                    )
                })?;

                let label = matched
                    .canonical_path()
                    .and_then(|p| p.segments().last())
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| node_display_label(matched));
                let kind = extract_str_prop(matched, "kind").unwrap_or_else(|| "symbol".to_owned());
                let mut node = GraphNode::new(
                    format!("{}::{}", clean_rel, label),
                    RevealLevel::Level2Leaf,
                    &kind,
                );

                if let Some(sig) = extract_str_prop(matched, "signature") {
                    node.add_str_prop("signature", sig);
                }
                if let Some(span_prop) = matched.property("line_span") {
                    node.add_raw_json_prop("lines", span_prop.raw_value());
                }
                if let Some(doc_str) = extract_str_prop(matched, "doc") {
                    node.add_str_prop("doc", doc_str);
                }

                node.children = matched
                    .children()
                    .map(|c| {
                        let c_name = c
                            .canonical_path()
                            .and_then(|p| p.segments().last())
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| node_display_label(c));
                        format!("{}::{}", clean_rel, c_name)
                    })
                    .collect();
                node.relatives = collect_node_relatives(matched);

                if let Some(code_prop) = matched.property("code")
                    && let Ok(Value::Multiline(ml)) = code_prop.decode()
                {
                    node.set_payload(ml.tag, ml.decode_text().into_owned());
                }

                Ok(node)
            }
            FlatArtifactDomain::DocOrMedia => {
                let artifact_abs = self
                    .ensure_hydrated_file(rel_path, false)
                    .map_err(|e| InspectError::new(e, None))?
                    .unwrap();
                let raw = fs::read_to_string(&artifact_abs).map_err(|e| {
                    InspectError::new(format!("failed to read artifact: {}", e), None)
                })?;
                let doc = Document::parse(&raw)
                    .map_err(|e| InspectError::new(format!("invalid doc SFGN: {}", e), None))?;

                let matched = resolve_section_in_doc(&doc, member_query).ok_or_else(|| {
                    let available = list_doc_child_labels(&doc, clean_rel);
                    InspectError::new(
                        format!("Section '{}' not found in '{}'", member_query, clean_rel),
                        Some(format!(
                            "Call inspect_node with one of these exact addresses: {}",
                            available.join(", ")
                        )),
                    )
                })?;

                let label = matched
                    .canonical_path()
                    .and_then(|p| p.segments().last())
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| node_display_label(matched));
                let mut node = GraphNode::new(
                    format!("{}::{}", clean_rel, label),
                    RevealLevel::Level2Leaf,
                    "section",
                );
                node.relatives = collect_node_relatives(matched);

                let mut combined_body = String::new();
                for prop in matched.properties() {
                    if let Ok(Value::Multiline(ml)) = prop.decode() {
                        if !combined_body.is_empty() {
                            combined_body.push_str("\n\n");
                        }
                        combined_body.push_str(&ml.decode_text());
                    }
                }
                if !combined_body.is_empty() {
                    node.set_payload(Some("md"), cap_payload(&combined_body, 12_000));
                }
                Ok(node)
            }
            FlatArtifactDomain::FsOnly => Err(InspectError::new(
                format!(
                    "'{}' is a plain text/config file without child symbols",
                    clean_rel
                ),
                Some(format!(
                    "Call {{\"action\":\"inspect_node\",\"address\":\"{}\"}} without '::'.",
                    clean_rel
                )),
            )),
        }
    }

    fn ensure_hydrated_file(
        &self,
        rel_source: &Path,
        force: bool,
    ) -> Result<Option<PathBuf>, String> {
        let ext = rel_source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let domain = match classify_workspace_file(rel_source) {
            FlatArtifactDomain::Code => FlatArtifactDomain::Code,
            FlatArtifactDomain::DocOrMedia => FlatArtifactDomain::DocOrMedia,
            FlatArtifactDomain::FsOnly => {
                if classify_media_extension(ext).is_some() {
                    FlatArtifactDomain::DocOrMedia
                } else {
                    return Ok(None);
                }
            }
        };

        let artifact_rel = match domain {
            FlatArtifactDomain::Code => flat_code_artifact_rel(rel_source),
            FlatArtifactDomain::DocOrMedia => flat_doc_artifact_rel(rel_source),
            FlatArtifactDomain::FsOnly => unreachable!(),
        };

        let abs_source = self.root.join(rel_source);
        let abs_artifact = self.root.join(&artifact_rel);

        if !force && abs_artifact.is_file() {
            if let (Ok(src_meta), Ok(art_meta)) =
                (fs::metadata(&abs_source), fs::metadata(&abs_artifact))
            {
                if let (Ok(src_mtime), Ok(art_mtime)) = (src_meta.modified(), art_meta.modified())
                    && art_mtime >= src_mtime
                {
                    return Ok(Some(abs_artifact));
                }
            }
        }

        let rendered = match domain {
            FlatArtifactDomain::Code => CodeExtractor::new(CodeExtractOptions::default())
                .extract_file(&self.root, rel_source)
                .map_err(|e| {
                    format!(
                        "code hydration failed for '{}': {}",
                        rel_source.display(),
                        e
                    )
                })?,
            FlatArtifactDomain::DocOrMedia => DocsExtractor::new(DocsExtractOptions::default())
                .extract_file(&self.root, rel_source)
                .map_err(|e| {
                    format!("doc hydration failed for '{}': {}", rel_source.display(), e)
                })?,
            FlatArtifactDomain::FsOnly => unreachable!(),
        };

        if let Some(parent) = abs_artifact.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create '{}': {}", parent.display(), e))?;
        }
        fs::write(&abs_artifact, &rendered)
            .map_err(|e| format!("failed to write '{}': {}", abs_artifact.display(), e))?;

        Ok(Some(abs_artifact))
    }

    fn suggest_paths(&self, missing_rel: &str) -> Vec<String> {
        let needle = Path::new(missing_rel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(missing_rel)
            .to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        self.walk_workspace_paths(&self.root, &mut |rel_posix, is_dir| {
            if matches.len() >= 5 {
                return;
            }
            if rel_posix.to_ascii_lowercase().contains(&needle) {
                if is_dir {
                    matches.push(format!("{}/", rel_posix));
                } else {
                    matches.push(rel_posix.to_owned());
                }
            }
        });
        matches
    }

    fn walk_workspace_paths(&self, dir: &Path, cb: &mut dyn FnMut(&str, bool)) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "target" || name == "sfgn-logs" || name == MEMORY_GRAPH_DIR
            {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&self.root) else {
                continue;
            };
            if is_generated_sfgn_artifact(rel) {
                continue;
            }
            let rel_posix = rel.to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                cb(&rel_posix, true);
                self.walk_workspace_paths(&path, cb);
            } else if path.is_file() {
                cb(&rel_posix, false);
            }
        }
    }

    fn walk_text_files(&self, dir: &Path, cb: &mut dyn FnMut(&str, &str)) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "target" || name == "sfgn-logs" || name == MEMORY_GRAPH_DIR
            {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&self.root) else {
                continue;
            };
            if is_generated_sfgn_artifact(rel) {
                continue;
            }
            if path.is_dir() {
                self.walk_text_files(&path, cb);
            } else if path.is_file()
                && let Ok(meta) = fs::metadata(&path)
                && meta.len() <= 512 * 1024
                && let Ok(text) = fs::read_to_string(&path)
            {
                let rel_posix = rel.to_string_lossy().replace('\\', "/");
                cb(&rel_posix, &text);
            }
        }
    }
}

fn attach_reverse_portals(workspace_root: &Path, node: &mut GraphNode) {
    if !matches!(
        node.level,
        RevealLevel::Level1Outline | RevealLevel::Level2Leaf
    ) {
        return;
    }
    if node.address.starts_with(MEMORY_GRAPH_DIR) || node.address.starts_with("tools/") {
        return;
    }

    let (rel_file, sym_opt) = match node.address.split_once("::") {
        Some((file, sym)) => (file, Some(sym)),
        None => (node.address.as_str(), None),
    };

    for backlink in crate::portals::resolve_reverse_portals(workspace_root, rel_file, sym_opt) {
        if !node.relatives.contains(&backlink) {
            node.relatives.push(backlink);
        }
    }
}

fn resolve_symbol_in_doc<'doc, 'a>(
    doc: &'doc Document<'a>,
    raw_query: &str,
) -> Option<NodeView<'doc, 'a>> {
    let q = raw_query.trim().trim_matches('"');
    if q.is_empty() {
        return None;
    }

    if let Ok(Some(node)) = doc.node_by_path(raw_query) {
        return Some(node);
    }

    let method_leaf = q.rsplit_once("::").map(|(_, m)| m.trim());

    for node in doc.nodes() {
        if node.is_root() {
            continue;
        }
        if let Some(canon) = node.canonical_path()
            && let Some(last) = canon.segments().last()
            && last.name == q
        {
            return Some(node);
        }
    }

    for node in doc.nodes() {
        if node.is_root() {
            continue;
        }
        if let Some(canon) = node.canonical_path()
            && let Some(last) = canon.segments().last()
        {
            let bare = strip_symbol_kind_prefix(&last.name);
            if bare == q || method_leaf == Some(bare) {
                return Some(node);
            }
        }
    }

    None
}

fn resolve_section_in_doc<'doc, 'a>(
    doc: &'doc Document<'a>,
    raw_query: &str,
) -> Option<NodeView<'doc, 'a>> {
    let q = raw_query.trim().trim_matches('"');
    let q_lower = q.to_ascii_lowercase();

    for node in doc.nodes() {
        if node.is_root() {
            continue;
        }
        if let Some(canon) = node.canonical_path()
            && let Some(last) = canon.segments().last()
            && (last.name == q || last.name.to_ascii_lowercase() == q_lower)
        {
            return Some(node);
        }
        if let Some(slug) = extract_str_prop(node, "slug")
            && slug == q_lower
        {
            return Some(node);
        }
    }
    None
}

fn list_doc_child_labels(doc: &Document<'_>, clean_rel: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for node in doc.nodes() {
        if node.is_root() {
            continue;
        }
        if let Some(canon) = node.canonical_path()
            && let Some(last) = canon.segments().last()
        {
            labels.push(format!("{}::{}", clean_rel, last.name));
        }
    }
    labels
}

fn collect_node_relatives(node: NodeView<'_, '_>) -> Vec<String> {
    let mut rels = BTreeSet::new();
    for r in node.relatives() {
        rels.insert(node_display_label(r));
    }
    for portal in node.external_portals() {
        if let Some(decoded) = sfgn_core::decode_flat_artifact_path(portal.file_path) {
            let clean = decoded.to_string_lossy().replace('\\', "/");
            if let Some(sub) = portal.sub_path
                && let Some(last) = sub.segments().last()
            {
                rels.insert(format!("{}::{}", clean, last.name));
            } else {
                rels.insert(clean);
            }
        } else {
            rels.insert(portal.file_path.to_owned());
        }
    }
    rels.into_iter().collect()
}

fn node_display_label(node: NodeView<'_, '_>) -> String {
    node.canonical_path()
        .and_then(|p| p.segments().last())
        .map(|s| s.name.clone())
        .unwrap_or_else(|| node.raw_header_path().trim_matches('"').to_owned())
}

fn strip_symbol_kind_prefix(label: &str) -> &str {
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
    for prefix in PREFIXES {
        if let Some(rest) = label.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    label
}

fn normalize_rel_posix(raw: &str) -> Result<String, String> {
    let p = Path::new(raw.trim());
    let mut parts = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(os) => parts.push(os.to_string_lossy().into_owned()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Path '{}' must be relative to the workspace root",
                    raw
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err("Target path cannot be empty".to_owned());
    }
    Ok(parts.join("/"))
}

fn extract_str_prop(node: NodeView<'_, '_>, key: &str) -> Option<String> {
    let prop = node.property(key)?;
    match prop.decode().ok()? {
        Value::Scalar(Scalar::String(cow)) => Some(cow.into_owned()),
        _ => None,
    }
}

fn cap_payload(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n... [truncated {} bytes — inspect a specific symbol/section for full text]",
        &text[..end],
        text.len() - end
    )
}

#[allow(dead_code)]
fn _keep_graph_dir_consts_referenced() {
    let _ = (CODE_GRAPH_DIR, DOC_GRAPH_DIR);
}
