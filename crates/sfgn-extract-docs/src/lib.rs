pub mod error;
pub mod gbnf;
pub mod markdown;
pub mod media;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use sfgn_core::{EdgeTarget, IndentStyle, NodeHandle, SfgnBuilder, SfgnScalar};

pub use error::{DocsExtractError, Result};
pub use gbnf::{compile_ebnf_to_gbnf, normalize_gbnf_rule_name};
pub use markdown::slugify_heading;
pub use media::{MediaCategory, MediaProxyMeta, classify_media_extension, inspect_media_file};

use markdown::{ExtractedMarkdown, parse_markdown};

/// Configuration options for the `sfgn-extract-docs` Markdown & binary media proxy compiler.
#[derive(Debug, Clone)]
pub struct DocsExtractOptions {
    /// Optional explicit root label stored in root metadata (defaults to directory name).
    pub root_name: Option<String>,
    /// Whether to include binary media proxy vertices (images, audio, video, PDFs, archives).
    pub include_media_proxies: bool,
    /// Optional filesystem graph filename (e.g., `Some("fs.sfgn".into())`) used when Markdown
    /// links reference workspace files (e.g., `src/builder.rs`, `Cargo.toml`, `crates/.../lib.rs`).
    pub fs_portal_file: Option<String>,
    /// Optional code AST graph filename (e.g., `Some("code.sfgn".into())`) used when Markdown
    /// links include a `#Symbol` fragment on a source file (e.g., `src/builder.rs#SfgnBuilder`).
    pub code_portal_file: Option<String>,
    /// Maximum Markdown file size in bytes to parse (default `4 MiB`).
    pub max_markdown_bytes: u64,
    /// Depth 1 indentation style for emitted `doc.sfgn`.
    pub indent: IndentStyle,
}

impl Default for DocsExtractOptions {
    fn default() -> Self {
        Self {
            root_name: None,
            include_media_proxies: true,
            fs_portal_file: None,
            code_portal_file: None,
            max_markdown_bytes: 4 * 1024 * 1024,
            indent: IndentStyle::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingDocLink {
    from: NodeHandle,
    source_file_posix: String,
    raw_target: String,
}

/// Deterministic Markdown outline/table compiler and binary media proxy scanner emitting `doc.sfgn`.
#[derive(Debug, Clone, Default)]
pub struct DocsExtractor {
    options: DocsExtractOptions,
}

impl DocsExtractor {
    pub fn new(options: DocsExtractOptions) -> Self {
        Self { options }
    }

    /// Walks `root_dir`, compiles Markdown files (`H1 > H2 > H3`, prose, fenced code, CSV tables,
    /// and internal/cross-doc links) plus binary media proxies into canonical `doc.sfgn`.
    pub fn extract(&self, root_dir: impl AsRef<Path>) -> Result<String> {
        let root_path = root_dir.as_ref();
        let canon_root = fs::canonicalize(root_path).map_err(|source| DocsExtractError::Io {
            path: root_path.to_path_buf(),
            source,
        })?;

        let mut walker = WalkBuilder::new(&canon_root);
        walker
            .hidden(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|entry| entry.file_name() != ".git");

        let mut md_files: BTreeMap<String, (String, ExtractedMarkdown)> = BTreeMap::new();
        let mut media_files: BTreeMap<String, (String, MediaProxyMeta)> = BTreeMap::new();
        let mut active_dirs: BTreeSet<String> = BTreeSet::new();

        for result in walker.build() {
            let entry = result?;
            let path = entry.path();
            if path == canon_root {
                continue;
            }

            let Ok(metadata) = fs::metadata(path) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }

            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            let Ok(rel_path) = path.strip_prefix(&canon_root) else {
                continue;
            };
            let rel_posix = path_to_posix(rel_path);
            if rel_posix.is_empty() {
                continue;
            }

            let file_name = rel_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel_posix.clone());

            if is_markdown_ext(ext) && metadata.len() <= self.options.max_markdown_bytes {
                let raw = fs::read_to_string(path).map_err(|source| DocsExtractError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                let parsed = parse_markdown(&raw);
                record_ancestor_dirs(&rel_posix, &mut active_dirs);
                md_files.insert(rel_posix, (file_name, parsed));
            } else if self.options.include_media_proxies {
                if let Some(proxy) = inspect_media_file(path, metadata.len()) {
                    record_ancestor_dirs(&rel_posix, &mut active_dirs);
                    media_files.insert(rel_posix, (file_name, proxy));
                }
            }
        }

        let mut builder = SfgnBuilder::new().with_indent(self.options.indent);
        let root_label = self.options.root_name.clone().unwrap_or_else(|| {
            canon_root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "docs".to_owned())
        });

        builder.set_root_prop("kind", "documentation");
        builder.set_root_prop("root_name", root_label);
        builder.set_root_prop("total_docs", md_files.len() as u64);
        builder.set_root_prop("total_media", media_files.len() as u64);

        // 1. Register directory vertices
        let mut dir_handles: HashMap<String, NodeHandle> =
            HashMap::with_capacity(active_dirs.len());
        for dir_rel in &active_dirs {
            let parent = match dir_rel.rsplit_once('/') {
                Some((p, _)) => dir_handles.get(p).copied().unwrap_or(NodeHandle::ROOT),
                None => NodeHandle::ROOT,
            };
            let dir_name = dir_rel.rsplit('/').next().unwrap_or(dir_rel.as_str());
            let handle = builder.add_node(parent, dir_name);
            builder.add_prop(handle, "kind", "directory");
            builder.add_prop(handle, "path", dir_rel.as_str());
            dir_handles.insert(dir_rel.clone(), handle);
        }

        // 2. Register Markdown document and nested heading vertices
        let mut file_handles: HashMap<String, NodeHandle> = HashMap::with_capacity(md_files.len());
        let mut anchor_handles: HashMap<(String, String), NodeHandle> = HashMap::new();
        let mut pending_links: Vec<PendingDocLink> = Vec::new();

        for (rel_posix, (file_name, md)) in &md_files {
            let parent = match rel_posix.rsplit_once('/') {
                Some((p, _)) => dir_handles.get(p).copied().unwrap_or(NodeHandle::ROOT),
                None => NodeHandle::ROOT,
            };

            let file_handle = builder.add_node(parent, file_name);
            builder.add_prop(file_handle, "kind", "document");
            builder.add_prop(file_handle, "path", rel_posix.as_str());
            builder.add_prop(file_handle, "sections", md.sections.len() as u64);

            if !md.preamble.is_empty() {
                builder.add_multiline(file_handle, "preamble", Some("md"), &md.preamble);
            }
            for raw_target in &md.preamble_links {
                pending_links.push(PendingDocLink {
                    from: file_handle,
                    source_file_posix: rel_posix.clone(),
                    raw_target: raw_target.clone(),
                });
            }

            file_handles.insert(rel_posix.clone(), file_handle);

            // Heading level stack `(level, NodeHandle)` builds `H1 > H2 > H3` containment
            let mut heading_stack: Vec<(u8, NodeHandle)> = Vec::new();

            for sec in &md.sections {
                while let Some(&(top_lvl, _)) = heading_stack.last() {
                    if top_lvl >= sec.level {
                        heading_stack.pop();
                    } else {
                        break;
                    }
                }
                let sec_parent = heading_stack.last().map(|&(_, h)| h).unwrap_or(file_handle);

                let sec_label = if sec.title.is_empty() {
                    format!("Section H{}", sec.level)
                } else {
                    sec.title.clone()
                };

                let sec_handle = builder.add_node(sec_parent, &sec_label);
                builder.add_prop(sec_handle, "kind", "section");
                builder.add_prop(sec_handle, "level", sec.level as u64);
                builder.add_prop(sec_handle, "slug", sec.slug.as_str());
                builder.add_prop(sec_handle, "source_file", rel_posix.as_str());

                if !sec.content.is_empty() {
                    builder.add_multiline(sec_handle, "content", Some("md"), &sec.content);
                }

                for (idx, tbl) in sec.tables.iter().enumerate() {
                    if sec.tables.len() == 1 {
                        builder.add_multiline(sec_handle, "table", Some("csv"), &tbl.csv);
                        builder.add_prop(
                            sec_handle,
                            "table_shape",
                            vec![SfgnScalar::from(tbl.rows), SfgnScalar::from(tbl.cols)],
                        );
                    } else {
                        let key = format!("table_{}", idx);
                        let shape_key = format!("table_{}_shape", idx);
                        builder.add_multiline(sec_handle, &key, Some("csv"), &tbl.csv);
                        builder.add_prop(
                            sec_handle,
                            &shape_key,
                            vec![SfgnScalar::from(tbl.rows), SfgnScalar::from(tbl.cols)],
                        );
                    }
                }

                for (idx, cb) in sec.code_blocks.iter().enumerate() {
                    let tag = cb.language.as_deref();
                    if sec.code_blocks.len() == 1 {
                        builder.add_multiline(sec_handle, "code", tag, &cb.code);
                    } else {
                        let key = format!("code_{}", idx);
                        builder.add_multiline(sec_handle, &key, tag, &cb.code);
                    }
                }

                if !sec.slug.is_empty() {
                    anchor_handles
                        .entry((rel_posix.clone(), sec.slug.clone()))
                        .or_insert(sec_handle);
                }

                for raw_target in &sec.links {
                    pending_links.push(PendingDocLink {
                        from: sec_handle,
                        source_file_posix: rel_posix.clone(),
                        raw_target: raw_target.clone(),
                    });
                }

                heading_stack.push((sec.level, sec_handle));
            }
        }

        // 3. Register binary media proxy vertices
        let mut media_handles: HashMap<String, NodeHandle> =
            HashMap::with_capacity(media_files.len());
        for (rel_posix, (file_name, proxy)) in &media_files {
            let parent = match rel_posix.rsplit_once('/') {
                Some((p, _)) => dir_handles.get(p).copied().unwrap_or(NodeHandle::ROOT),
                None => NodeHandle::ROOT,
            };

            let handle = builder.add_node(parent, file_name);
            builder.add_prop(handle, "kind", "media_proxy");
            builder.add_prop(handle, "media_type", proxy.category.as_str());
            builder.add_prop(handle, "mime_type", proxy.mime_type);
            builder.add_prop(handle, "path", rel_posix.as_str());
            builder.add_prop(handle, "size_bytes", proxy.size_bytes);

            if let Some(w) = proxy.width {
                builder.add_prop(handle, "width", w as u64);
            }
            if let Some(h) = proxy.height {
                builder.add_prop(handle, "height", h as u64);
            }
            if let Some(sr) = proxy.sample_rate_hz {
                builder.add_prop(handle, "sample_rate_hz", sr as u64);
            }
            if let Some(ch) = proxy.channels {
                builder.add_prop(handle, "channels", ch as u64);
            }
            if let Some(dur) = proxy.duration_secs {
                builder.add_prop(handle, "duration_secs", dur);
            }
            if let Some(detail) = &proxy.detail {
                builder.add_prop(handle, "detail", detail.as_str());
            }

            media_handles.insert(rel_posix.clone(), handle);
        }

        // 4. Resolve `>>` cross-references (same-file `#anchor`, cross-doc `.md`, media assets, `fs.sfgn` / `code.sfgn` portals)
        let mut seen_local_edges: BTreeSet<(NodeHandle, NodeHandle)> = BTreeSet::new();
        let mut seen_external_edges: BTreeSet<(NodeHandle, String, Option<String>)> =
            BTreeSet::new();

        for link in pending_links {
            if let Some(anchor) = link.raw_target.strip_prefix('#') {
                let slug = slugify_heading(anchor);
                if let Some(&target_h) = anchor_handles.get(&(link.source_file_posix.clone(), slug))
                {
                    if target_h != link.from && seen_local_edges.insert((link.from, target_h)) {
                        builder.add_relative(link.from, target_h);
                    }
                }
                continue;
            }

            let (path_part, anchor_part) = match link.raw_target.split_once('#') {
                Some((p, a)) => (p, Some(slugify_heading(a))),
                None => (link.raw_target.as_str(), None),
            };
            if path_part.is_empty() {
                continue;
            }

            let resolved_rel = resolve_relative_posix(&link.source_file_posix, path_part);

            // Check local Markdown section or document
            if let Some(slug) = &anchor_part {
                if let Some(&sec_h) = anchor_handles.get(&(resolved_rel.clone(), slug.clone())) {
                    if sec_h != link.from && seen_local_edges.insert((link.from, sec_h)) {
                        builder.add_relative(link.from, sec_h);
                    }
                    continue;
                }
            }
            if let Some(&doc_h) = file_handles.get(&resolved_rel) {
                if doc_h != link.from && seen_local_edges.insert((link.from, doc_h)) {
                    builder.add_relative(link.from, doc_h);
                }
                continue;
            }

            // Check local binary media proxy vertex
            if let Some(&media_h) = media_handles.get(&resolved_rel) {
                if media_h != link.from && seen_local_edges.insert((link.from, media_h)) {
                    builder.add_relative(link.from, media_h);
                }
                continue;
            }

            // External `.sfgn` portal
            if resolved_rel.ends_with(".sfgn") {
                if seen_external_edges.insert((link.from, resolved_rel.clone(), None)) {
                    builder.add_relative(link.from, EdgeTarget::external_root(resolved_rel));
                }
                continue;
            }

            // Code AST symbol portal (`file.rs#Symbol` -> `<"code.sfgn">"dir">"file.rs">"Symbol"`)
            if let Some(code_sfgn) = &self.options.code_portal_file {
                if let Some((_, raw_sym)) = link.raw_target.split_once('#') {
                    if !is_markdown_path(&resolved_rel) {
                        let file_node_path = posix_path_to_sfgn_node_path(&resolved_rel);
                        let sym_escaped = raw_sym.trim().replace('\\', "\\\\").replace('"', "\\\"");
                        if !file_node_path.is_empty() && !sym_escaped.is_empty() {
                            let sub_path = format!("{}>\"{}\"", file_node_path, sym_escaped);
                            if seen_external_edges.insert((
                                link.from,
                                code_sfgn.clone(),
                                Some(sub_path.clone()),
                            )) {
                                builder.add_relative(
                                    link.from,
                                    EdgeTarget::external_node(code_sfgn.clone(), sub_path),
                                );
                            }
                            continue;
                        }
                    }
                }
            }

            // Filesystem graph portal (`dir/file.ext` -> `<"fs.sfgn">"dir">"file.ext"`)
            if let Some(fs_sfgn) = &self.options.fs_portal_file {
                let sub_path = posix_path_to_sfgn_node_path(&resolved_rel);
                if !sub_path.is_empty()
                    && seen_external_edges.insert((
                        link.from,
                        fs_sfgn.clone(),
                        Some(sub_path.clone()),
                    ))
                {
                    builder.add_relative(
                        link.from,
                        EdgeTarget::external_node(fs_sfgn.clone(), sub_path),
                    );
                }
            }
        }

        Ok(builder.build()?)
    }
}

/// Convenience function compiling `root_dir` into `doc.sfgn` using default `DocsExtractOptions`.
pub fn extract_docs_to_sfgn(root_dir: impl AsRef<Path>) -> Result<String> {
    DocsExtractor::default().extract(root_dir)
}

fn is_markdown_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx")
}

fn is_markdown_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(is_markdown_ext)
        .unwrap_or(false)
}

fn record_ancestor_dirs(rel_posix: &str, active_dirs: &mut BTreeSet<String>) {
    let mut cursor = rel_posix;
    while let Some((parent, _)) = cursor.rsplit_once('/') {
        active_dirs.insert(parent.to_owned());
        cursor = parent;
    }
}

fn resolve_relative_posix(from_file_posix: &str, target: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !target.starts_with('/') {
        if let Some((parent_dir, _)) = from_file_posix.rsplit_once('/') {
            for seg in parent_dir.split('/') {
                if !seg.is_empty() {
                    parts.push(seg);
                }
            }
        }
    }

    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }

    parts.join("/")
}

fn posix_path_to_sfgn_node_path(posix_path: &str) -> String {
    posix_path
        .split('/')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let escaped = seg.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{}\"", escaped)
        })
        .collect::<Vec<_>>()
        .join(">")
}

fn path_to_posix(path: &Path) -> String {
    let mut parts = Vec::new();
    for comp in path.components() {
        if let std::path::Component::Normal(os) = comp {
            parts.push(os.to_string_lossy());
        }
    }
    parts.join("/")
}
