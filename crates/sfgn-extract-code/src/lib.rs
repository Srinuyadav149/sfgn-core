pub mod error;
pub mod lang;
pub mod patch;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use sfgn_core::{IndentStyle, NodeHandle, SfgnBuilder, SfgnScalar};

pub use error::{CodeExtractError, Result};
pub use lang::LanguageKind;
pub use patch::{patch_node_source, splice_line_span};

use lang::{ExtractedFile, ExtractedSymbol, extract_file_symbols};

/// Configuration options for the `sfgn-extract-code` source code graph compiler.
#[derive(Debug, Clone)]
pub struct CodeExtractOptions {
    /// Optional explicit root label stored in root metadata (defaults to directory name).
    pub root_name: Option<String>,
    /// Whether to embed verbatim `"""<lang>` implementation bodies on compiled nodes (default `true`).
    pub include_bodies: bool,
    /// Whether to include private / non-exported items (default `true`).
    pub include_private: bool,
    /// Maximum source file size in bytes to parse (default `2 MiB`).
    pub max_file_bytes: u64,
    /// Depth 1 indentation style for emitted `code.sfgn`.
    pub indent: IndentStyle,
}

impl Default for CodeExtractOptions {
    fn default() -> Self {
        Self {
            root_name: None,
            include_bodies: true,
            include_private: true,
            max_file_bytes: 2 * 1024 * 1024,
            indent: IndentStyle::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingEdges {
    from: NodeHandle,
    targets: Vec<String>,
}

/// Deterministic `tree-sitter` codebase compiler emitting validated `code.sfgn` graphs.
#[derive(Debug, Clone, Default)]
pub struct CodeExtractor {
    options: CodeExtractOptions,
}

impl CodeExtractor {
    pub fn new(options: CodeExtractOptions) -> Self {
        Self { options }
    }

    /// Walks `root_dir`, parses enabled source languages via `tree-sitter`, resolves cross-symbol
    /// `>>` edges, and compiles canonical `code.sfgn`.
    pub fn extract(&self, root_dir: impl AsRef<Path>) -> Result<String> {
        let root_path = root_dir.as_ref();
        let canon_root = fs::canonicalize(root_path).map_err(|source| CodeExtractError::Io {
            path: root_path.to_path_buf(),
            source,
        })?;

        let mut walker = WalkBuilder::new(&canon_root);
        walker
            .hidden(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|entry| entry.file_name() != ".git");

        let mut parsed_files: BTreeMap<String, (PathBuf, String, ExtractedFile)> = BTreeMap::new();
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
            if !metadata.is_file() || metadata.len() > self.options.max_file_bytes {
                continue;
            }

            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            let Some(lang) = LanguageKind::from_extension(ext) else {
                continue;
            };
            if !lang.is_enabled() {
                continue;
            }

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

            let source = fs::read_to_string(path).map_err(|source| CodeExtractError::Io {
                path: path.to_path_buf(),
                source,
            })?;

            let extracted = extract_file_symbols(path, lang, &source)?;

            // Record all ancestor directories that contain at least one compiled source file
            let mut dir_cursor = rel_posix.as_str();
            while let Some((parent, _)) = dir_cursor.rsplit_once('/') {
                active_dirs.insert(parent.to_owned());
                dir_cursor = parent;
            }

            parsed_files.insert(rel_posix, (path.to_path_buf(), file_name, extracted));
        }

        let mut builder = SfgnBuilder::new().with_indent(self.options.indent);
        let root_label = self.options.root_name.clone().unwrap_or_else(|| {
            canon_root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "codebase".to_owned())
        });

        builder.set_root_prop("kind", "codebase");
        builder.set_root_prop("root_name", root_label);
        builder.set_root_prop("total_files", parsed_files.len() as u64);

        // 1. Create directory nodes in lexical order
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

        // 2. Create file nodes and nested lexical symbol trees
        let mut symbol_index: HashMap<String, Vec<NodeHandle>> = HashMap::new();
        let mut pending_edges: Vec<PendingEdges> = Vec::new();

        for (rel_posix, (_abs_path, file_name, extracted)) in &parsed_files {
            let parent = match rel_posix.rsplit_once('/') {
                Some((p, _)) => dir_handles.get(p).copied().unwrap_or(NodeHandle::ROOT),
                None => NodeHandle::ROOT,
            };

            let file_handle = builder.add_node(parent, file_name);
            builder.add_prop(file_handle, "kind", "file");
            builder.add_prop(file_handle, "language", extracted.language.tag());
            builder.add_prop(file_handle, "source_file", rel_posix.as_str());
            if let Some(doc) = &extracted.doc {
                builder.add_prop(file_handle, "doc", doc.as_str());
            }

            if let Some(stem) = Path::new(file_name).file_stem().and_then(|s| s.to_str()) {
                symbol_index
                    .entry(stem.to_owned())
                    .or_default()
                    .push(file_handle);
            }

            if !extracted.imports.is_empty() {
                pending_edges.push(PendingEdges {
                    from: file_handle,
                    targets: extracted.imports.clone(),
                });
            }

            for sym in &extracted.symbols {
                self.register_symbol(
                    &mut builder,
                    file_handle,
                    None,
                    rel_posix,
                    extracted.language,
                    sym,
                    &mut symbol_index,
                    &mut pending_edges,
                );
            }
        }

        // 3. Resolve `>>` associative edges across calls, trait/type references, and imports
        for edge_group in pending_edges {
            let mut linked_handles: BTreeSet<NodeHandle> = BTreeSet::new();
            for target_name in &edge_group.targets {
                if let Some(handles) = symbol_index.get(target_name) {
                    for &target_handle in handles {
                        if target_handle != edge_group.from {
                            linked_handles.insert(target_handle);
                        }
                    }
                }
            }
            for target_handle in linked_handles {
                builder.add_relative(edge_group.from, target_handle);
            }
        }

        Ok(builder.build()?)
    }

    #[allow(clippy::too_many_arguments)]
    fn register_symbol(
        &self,
        builder: &mut SfgnBuilder,
        parent: NodeHandle,
        enclosing_type_name: Option<&str>,
        source_file: &str,
        lang: LanguageKind,
        sym: &ExtractedSymbol,
        symbol_index: &mut HashMap<String, Vec<NodeHandle>>,
        pending_edges: &mut Vec<PendingEdges>,
    ) {
        if !self.options.include_private && sym.visibility == "private" {
            return;
        }

        let header_label = sym.prefixed_name();
        let handle = builder.add_node(parent, &header_label);
        builder.add_prop(handle, "kind", sym.kind);
        builder.add_prop(handle, "language", lang.tag());
        builder.add_prop(handle, "source_file", source_file);
        builder.add_prop(
            handle,
            "line_span",
            vec![
                SfgnScalar::from(sym.line_span[0]),
                SfgnScalar::from(sym.line_span[1]),
            ],
        );
        builder.add_prop(handle, "visibility", sym.visibility.as_str());
        builder.add_prop(handle, "signature", sym.signature.as_str());

        if let Some(doc) = &sym.doc {
            builder.add_prop(handle, "doc", doc.as_str());
        }

        if self.options.include_bodies {
            builder.add_multiline(handle, "code", Some(lang.tag()), &sym.code);
        }

        // Index under bare symbol name, type-prefixed header label, and `Type::method` when applicable
        symbol_index
            .entry(sym.name.clone())
            .or_default()
            .push(handle);
        symbol_index.entry(header_label).or_default().push(handle);
        if sym.kind == "method" {
            if let Some(owner_ty) = enclosing_type_name {
                symbol_index
                    .entry(format!("{}::{}", owner_ty, sym.name))
                    .or_default()
                    .push(handle);
            }
        }

        let mut combined_targets = sym.calls.clone();
        combined_targets.extend(sym.type_refs.iter().cloned());
        if !combined_targets.is_empty() {
            pending_edges.push(PendingEdges {
                from: handle,
                targets: combined_targets,
            });
        }

        let next_owner_type = match sym.kind {
            "struct" | "enum" | "trait" | "class" | "interface" => Some(sym.name.as_str()),
            "impl" => sym
                .name
                .strip_prefix("impl ")
                .and_then(|rest| rest.rsplit(" for ").next()),
            _ => None,
        };

        for child in &sym.children {
            self.register_symbol(
                builder,
                handle,
                next_owner_type,
                source_file,
                lang,
                child,
                symbol_index,
                pending_edges,
            );
        }
    }
}

/// Convenience function compiling `root_dir` into `code.sfgn` using default `CodeExtractOptions`.
pub fn extract_code_to_sfgn(root_dir: impl AsRef<Path>) -> Result<String> {
    CodeExtractor::default().extract(root_dir)
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
