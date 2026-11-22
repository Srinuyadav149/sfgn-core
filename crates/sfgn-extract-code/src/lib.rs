pub mod error;
pub mod lang;
pub mod patch;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use sfgn_core::{
    EdgeTarget, IndentStyle, NodeHandle, SfgnBuilder, SfgnScalar, flat_code_portal,
    is_generated_sfgn_artifact,
};

pub use error::{CodeExtractError, Result};
pub use lang::LanguageKind;
pub use patch::{patch_node_source, splice_line_span};

use lang::{ExtractedFile, ExtractedSymbol, ImportBinding, extract_file_symbols};

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
    /// Depth 1 indentation style for emitted `.sfgn` files.
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

/// Deterministic `tree-sitter` codebase compiler emitting validated `code.sfgn` and lazy
/// per-file `sfgn-codes/<encoded>.sfgn` graphs.
#[derive(Debug, Clone, Default)]
pub struct CodeExtractor {
    options: CodeExtractOptions,
}

impl CodeExtractor {
    pub fn new(options: CodeExtractOptions) -> Self {
        Self { options }
    }

    /// Lazily compiles a single workspace source file (`rel_file_path`, e.g. `"src/main.rs"`)
    /// into its standalone flat `.sfgn` graph (`"kind": "code_file"`), resolving intra-file calls
    /// to local `NodeHandle`s and cross-file imports / external `mod` declarations to unhydrated
    /// `<"sfgn-codes/<encoded>.sfgn">` portals without parsing target files.
    pub fn extract_file(
        &self,
        workspace_root: impl AsRef<Path>,
        rel_file_path: impl AsRef<Path>,
    ) -> Result<String> {
        let root_path = workspace_root.as_ref();
        let canon_root = fs::canonicalize(root_path).map_err(|source| CodeExtractError::Io {
            path: root_path.to_path_buf(),
            source,
        })?;

        let rel_posix = normalize_workspace_rel_path(rel_file_path.as_ref())?;
        let abs_file = canon_root.join(&rel_posix);

        let metadata = fs::metadata(&abs_file).map_err(|source| CodeExtractError::Io {
            path: abs_file.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(CodeExtractError::Io {
                path: abs_file,
                source: io::Error::new(io::ErrorKind::InvalidInput, "target is not a regular file"),
            });
        }
        if metadata.len() > self.options.max_file_bytes {
            return Err(CodeExtractError::Parse {
                path: abs_file,
                language: "unknown",
                reason: format!(
                    "file size {} bytes exceeds max_file_bytes ({})",
                    metadata.len(),
                    self.options.max_file_bytes
                ),
            });
        }

        let ext = Path::new(&rel_posix)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang = LanguageKind::from_extension(ext).ok_or_else(|| CodeExtractError::Parse {
            path: abs_file.clone(),
            language: "unknown",
            reason: format!("unsupported source file extension '.{}'", ext),
        })?;
        if !lang.is_enabled() {
            return Err(CodeExtractError::Parse {
                path: abs_file,
                language: lang.tag(),
                reason: format!("language feature '{}' is not enabled", lang.tag()),
            });
        }

        let source = fs::read_to_string(&abs_file).map_err(|source| CodeExtractError::Io {
            path: abs_file.clone(),
            source,
        })?;

        let extracted = extract_file_symbols(&abs_file, lang, &source)?;
        let import_portals =
            resolve_import_portals(&canon_root, &rel_posix, lang, &extracted.import_bindings);

        let mut builder = SfgnBuilder::new().with_indent(self.options.indent);
        builder.set_root_prop("kind", "code_file");
        builder.set_root_prop("source_file", rel_posix.as_str());
        builder.set_root_prop("language", extracted.language.tag());
        if let Some(doc) = &extracted.doc {
            builder.set_root_prop("doc", doc.as_str());
        }

        let mut local_symbol_index: HashMap<String, Vec<NodeHandle>> = HashMap::new();
        let mut pending_edges: Vec<PendingEdges> = Vec::new();
        let mut used_external_portals: BTreeSet<String> = BTreeSet::new();

        for sym in &extracted.symbols {
            self.register_single_file_symbol(
                &mut builder,
                NodeHandle::ROOT,
                None,
                &canon_root,
                &rel_posix,
                extracted.language,
                sym,
                &mut local_symbol_index,
                &mut pending_edges,
                &mut used_external_portals,
            );
        }

        // Resolve each symbol's calls and type references to local nodes first, falling back
        // to resolved external `<"sfgn-codes/...">` portals for imported symbols.
        for edge_group in pending_edges {
            let mut local_targets: BTreeSet<NodeHandle> = BTreeSet::new();
            let mut external_targets: BTreeSet<String> = BTreeSet::new();

            for target_name in &edge_group.targets {
                if let Some(handles) = local_symbol_index.get(target_name) {
                    for &target_handle in handles {
                        if target_handle != edge_group.from {
                            local_targets.insert(target_handle);
                        }
                    }
                    continue;
                }

                // Check direct import name (`run_lsblk` / `SfgnBuilder`) or owner type (`SfgnBuilder::new` -> `SfgnBuilder`)
                let lookup_key = target_name
                    .split_once("::")
                    .map(|(owner, _)| owner)
                    .unwrap_or(target_name.as_str());

                if let Some(portals) = import_portals.get(lookup_key) {
                    for portal in portals {
                        external_targets.insert(portal.clone());
                    }
                }
            }

            for target_handle in local_targets {
                builder.add_relative(edge_group.from, target_handle);
            }
            for portal in external_targets {
                used_external_portals.insert(portal.clone());
                builder.add_relative(edge_group.from, EdgeTarget::external_root(portal));
            }
        }

        // Attach any file-level imported modules not already linked by a child symbol onto `>:`
        let mut root_portals: BTreeSet<String> = BTreeSet::new();
        for portals in import_portals.values() {
            for portal in portals {
                if !used_external_portals.contains(portal) {
                    root_portals.insert(portal.clone());
                }
            }
        }
        for portal in root_portals {
            builder.add_relative(NodeHandle::ROOT, EdgeTarget::external_root(portal));
        }

        Ok(builder.build()?)
    }

    /// Walks `root_dir`, parses enabled source languages via `tree-sitter`, resolves cross-symbol
    /// `>>` edges, and compiles canonical monolithic `code.sfgn`.
    pub fn extract(&self, root_dir: impl AsRef<Path>) -> Result<String> {
        let root_path = root_dir.as_ref();
        let canon_root = fs::canonicalize(root_path).map_err(|source| CodeExtractError::Io {
            path: root_path.to_path_buf(),
            source,
        })?;

        let filter_root = canon_root.clone();
        let mut walker = WalkBuilder::new(&canon_root);
        walker
            .hidden(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(move |entry| {
                if entry.file_name() == ".git" {
                    return false;
                }
                if let Ok(rel) = entry.path().strip_prefix(&filter_root) {
                    if is_generated_sfgn_artifact(rel) {
                        return false;
                    }
                }
                true
            });

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
            if is_generated_sfgn_artifact(rel_path) {
                continue;
            }
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
    fn register_single_file_symbol(
        &self,
        builder: &mut SfgnBuilder,
        parent: NodeHandle,
        enclosing_type_name: Option<&str>,
        canon_root: &Path,
        source_file: &str,
        lang: LanguageKind,
        sym: &ExtractedSymbol,
        symbol_index: &mut HashMap<String, Vec<NodeHandle>>,
        pending_edges: &mut Vec<PendingEdges>,
        used_external_portals: &mut BTreeSet<String>,
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

        // External Rust `mod foo;` declaration resolves directly to `sfgn-codes/.../foo.rs.sfgn`
        if sym.is_external_mod && lang == LanguageKind::Rust {
            if let Some(portal) = resolve_rust_external_mod(canon_root, source_file, &sym.name) {
                used_external_portals.insert(portal.clone());
                builder.add_relative(handle, EdgeTarget::external_root(portal));
            }
        }

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
            self.register_single_file_symbol(
                builder,
                handle,
                next_owner_type,
                canon_root,
                source_file,
                lang,
                child,
                symbol_index,
                pending_edges,
                used_external_portals,
            );
        }
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

/// Convenience function compiling `root_dir` into monolithic `code.sfgn` using default options.
pub fn extract_code_to_sfgn(root_dir: impl AsRef<Path>) -> Result<String> {
    CodeExtractor::default().extract(root_dir)
}

/// Convenience function lazily compiling a single workspace-relative file (`rel_file_path`)
/// into its flat `sfgn-codes/<encoded>.sfgn` document string.
pub fn extract_code_file_to_sfgn(
    workspace_root: impl AsRef<Path>,
    rel_file_path: impl AsRef<Path>,
) -> Result<String> {
    CodeExtractor::default().extract_file(workspace_root, rel_file_path)
}

// ============================================================================
// Zero-Parse Import-to-Portal Resolution Helpers
// ============================================================================

fn normalize_workspace_rel_path(rel_path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for comp in rel_path.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(os) => parts.push(os.to_string_lossy().into_owned()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CodeExtractError::Io {
                    path: rel_path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "rel_file_path must be a relative path inside the workspace root",
                    ),
                });
            }
        }
    }
    if parts.is_empty() {
        return Err(CodeExtractError::Io {
            path: rel_path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "rel_file_path cannot be empty"),
        });
    }
    Ok(parts.join("/"))
}

fn resolve_import_portals(
    canon_root: &Path,
    source_rel_posix: &str,
    lang: LanguageKind,
    bindings: &[ImportBinding],
) -> HashMap<String, BTreeSet<String>> {
    let mut resolved: HashMap<String, BTreeSet<String>> = HashMap::new();
    if bindings.is_empty() {
        return resolved;
    }

    match lang {
        LanguageKind::Rust => {
            let crate_ctx = RustCrateContext::discover(canon_root, source_rel_posix);
            for binding in bindings {
                if let Some(target_rel) = crate_ctx.resolve_use_segments(
                    canon_root,
                    source_rel_posix,
                    &binding.path_segments,
                ) {
                    if target_rel != source_rel_posix {
                        let portal = flat_code_portal(Path::new(&target_rel));
                        resolved
                            .entry(binding.local_name.clone())
                            .or_default()
                            .insert(portal);
                    }
                }
            }
        }
        LanguageKind::Python => {
            for binding in bindings {
                if let Some(target_rel) =
                    resolve_python_import(canon_root, source_rel_posix, &binding.path_segments)
                {
                    if target_rel != source_rel_posix {
                        let portal = flat_code_portal(Path::new(&target_rel));
                        resolved
                            .entry(binding.local_name.clone())
                            .or_default()
                            .insert(portal);
                    }
                }
            }
        }
        LanguageKind::TypeScript | LanguageKind::Tsx | LanguageKind::JavaScript => {
            for binding in bindings {
                if let Some(spec) = &binding.module_specifier {
                    if let Some(target_rel) =
                        resolve_ts_relative_import(canon_root, source_rel_posix, spec)
                    {
                        if target_rel != source_rel_posix {
                            let portal = flat_code_portal(Path::new(&target_rel));
                            resolved
                                .entry(binding.local_name.clone())
                                .or_default()
                                .insert(portal);
                        }
                    }
                }
            }
        }
    }

    resolved
}

fn resolve_rust_external_mod(
    canon_root: &Path,
    source_rel_posix: &str,
    mod_name: &str,
) -> Option<String> {
    let parent_dir = match source_rel_posix.rsplit_once('/') {
        Some((p, _)) => PathBuf::from(p),
        None => PathBuf::new(),
    };
    let file_stem = Path::new(source_rel_posix)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let mod_base = if matches!(file_stem, "lib" | "main" | "mod") {
        parent_dir
    } else {
        parent_dir.join(file_stem)
    };

    let cand_file = mod_base.join(format!("{}.rs", mod_name));
    if canon_root.join(&cand_file).is_file() {
        return Some(flat_code_portal(&cand_file));
    }

    let cand_dir_mod = mod_base.join(mod_name).join("mod.rs");
    if canon_root.join(&cand_dir_mod).is_file() {
        return Some(flat_code_portal(&cand_dir_mod));
    }

    None
}

struct RustCrateContext {
    crate_rel_dir: PathBuf,
    own_crate_ident: Option<String>,
    workspace_crates: HashMap<String, PathBuf>,
}

impl RustCrateContext {
    fn discover(canon_root: &Path, source_rel_posix: &str) -> Self {
        let mut crate_rel_dir = PathBuf::new();
        let mut cursor = Path::new(source_rel_posix).parent();
        while let Some(dir) = cursor {
            if canon_root.join(dir).join("Cargo.toml").is_file() {
                crate_rel_dir = dir.to_path_buf();
                break;
            }
            cursor = dir.parent();
        }

        let own_crate_ident =
            read_cargo_package_ident(&canon_root.join(&crate_rel_dir).join("Cargo.toml"));

        let mut workspace_crates = HashMap::new();
        if let Some(ident) = &own_crate_ident {
            workspace_crates.insert(ident.clone(), crate_rel_dir.clone());
        }
        // Check `crates/*` convention at workspace root for local workspace member crates
        if let Ok(entries) = fs::read_dir(canon_root.join("crates")) {
            for entry in entries.flatten() {
                let member_abs = entry.path();
                let manifest = member_abs.join("Cargo.toml");
                if manifest.is_file() {
                    if let (Some(ident), Ok(rel_member)) = (
                        read_cargo_package_ident(&manifest),
                        member_abs.strip_prefix(canon_root),
                    ) {
                        workspace_crates.insert(ident, rel_member.to_path_buf());
                    }
                }
            }
        }

        Self {
            crate_rel_dir,
            own_crate_ident,
            workspace_crates,
        }
    }

    fn resolve_use_segments(
        &self,
        canon_root: &Path,
        source_rel_posix: &str,
        segments: &[String],
    ) -> Option<String> {
        let first = segments.first()?.as_str();
        let rest = &segments[1..];

        if first == "crate" {
            let src_root = self.crate_rel_dir.join("src");
            return resolve_rust_mod_tree(canon_root, &src_root, rest);
        }

        if first == "super" || first == "self" {
            let mut base_dir = match source_rel_posix.rsplit_once('/') {
                Some((p, _)) => PathBuf::from(p),
                None => PathBuf::new(),
            };
            let mut idx = 0;
            while idx < segments.len() {
                match segments[idx].as_str() {
                    "self" => idx += 1,
                    "super" => {
                        base_dir.pop();
                        idx += 1;
                    }
                    _ => break,
                }
            }
            return resolve_rust_mod_tree(canon_root, &base_dir, &segments[idx..]);
        }

        // External-looking root segment matching this crate or a workspace member crate
        if let Some(member_rel_dir) = self.workspace_crates.get(first) {
            let src_root = member_rel_dir.join("src");
            if let Some(found) = resolve_rust_mod_tree(canon_root, &src_root, rest) {
                return Some(found);
            }
            let lib_rs = src_root.join("lib.rs");
            if canon_root.join(&lib_rs).is_file() {
                return Some(path_to_posix(&lib_rs));
            }
        } else if self.own_crate_ident.as_deref() == Some(first) {
            let lib_rs = self.crate_rel_dir.join("src/lib.rs");
            if canon_root.join(&lib_rs).is_file() {
                return Some(path_to_posix(&lib_rs));
            }
        }

        None
    }
}

fn resolve_rust_mod_tree(
    canon_root: &Path,
    base_dir: &Path,
    segments: &[String],
) -> Option<String> {
    if segments.is_empty() {
        let lib_rs = base_dir.join("lib.rs");
        if canon_root.join(&lib_rs).is_file() {
            return Some(path_to_posix(&lib_rs));
        }
        return None;
    }

    // Walk longest matching module prefix first (`foo/bar/baz.rs` -> `foo/bar.rs` -> `foo.rs`)
    for end in (1..=segments.len()).rev() {
        let mut dir = base_dir.to_path_buf();
        for seg in &segments[..(end - 1)] {
            dir.push(seg);
        }
        let leaf = &segments[end - 1];

        let file_cand = dir.join(format!("{}.rs", leaf));
        if canon_root.join(&file_cand).is_file() {
            return Some(path_to_posix(&file_cand));
        }

        let mod_cand = dir.join(leaf).join("mod.rs");
        if canon_root.join(&mod_cand).is_file() {
            return Some(path_to_posix(&mod_cand));
        }
    }

    // Fallback to `lib.rs` if item is defined or re-exported directly at crate root
    let lib_rs = base_dir.join("lib.rs");
    if canon_root.join(&lib_rs).is_file() {
        return Some(path_to_posix(&lib_rs));
    }

    None
}

fn read_cargo_package_ident(manifest_path: &Path) -> Option<String> {
    let text = fs::read_to_string(manifest_path).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = trimmed.strip_prefix("name") {
                let rest = rest.trim_start();
                if let Some(val) = rest.strip_prefix('=') {
                    let pkg = val.trim().trim_matches(['"', '\'']).replace('-', "_");
                    if !pkg.is_empty() {
                        return Some(pkg);
                    }
                }
            }
        }
    }
    None
}

fn resolve_python_import(
    canon_root: &Path,
    source_rel_posix: &str,
    segments: &[String],
) -> Option<String> {
    if segments.is_empty() {
        return None;
    }

    let (mut base_dir, mod_segs) = if segments[0].starts_with('.') {
        let dots = segments[0].len();
        let mut dir = match source_rel_posix.rsplit_once('/') {
            Some((p, _)) => PathBuf::from(p),
            None => PathBuf::new(),
        };
        for _ in 1..dots {
            dir.pop();
        }
        (dir, &segments[1..])
    } else {
        (PathBuf::new(), segments)
    };

    if mod_segs.is_empty() {
        return None;
    }

    for end in (1..=mod_segs.len()).rev() {
        let mut dir = base_dir.clone();
        for seg in &mod_segs[..(end - 1)] {
            dir.push(seg);
        }
        let leaf = &mod_segs[end - 1];

        let py_file = dir.join(format!("{}.py", leaf));
        if canon_root.join(&py_file).is_file() {
            return Some(path_to_posix(&py_file));
        }

        let pkg_init = dir.join(leaf).join("__init__.py");
        if canon_root.join(&pkg_init).is_file() {
            return Some(path_to_posix(&pkg_init));
        }
    }

    base_dir.clear();
    None
}

fn resolve_ts_relative_import(
    canon_root: &Path,
    source_rel_posix: &str,
    specifier: &str,
) -> Option<String> {
    if !specifier.starts_with('.') {
        return None;
    }

    let mut base_dir = match source_rel_posix.rsplit_once('/') {
        Some((p, _)) => PathBuf::from(p),
        None => PathBuf::new(),
    };
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                base_dir.pop();
            }
            seg => base_dir.push(seg),
        }
    }

    // Handle ESM `.js`/`.jsx` import extensions pointing to `.ts`/`.tsx` sources on disk
    let stem_path = match base_dir.extension().and_then(|e| e.to_str()) {
        Some("js" | "jsx" | "mjs" | "cjs") => base_dir.with_extension(""),
        _ => base_dir.clone(),
    };

    let candidates = [
        base_dir.clone(),
        stem_path.with_extension("ts"),
        stem_path.with_extension("tsx"),
        stem_path.with_extension("js"),
        stem_path.with_extension("jsx"),
        stem_path.join("index.ts"),
        stem_path.join("index.tsx"),
        stem_path.join("index.js"),
        stem_path.join("index.jsx"),
    ];

    for cand in candidates {
        if canon_root.join(&cand).is_file() {
            return Some(path_to_posix(&cand));
        }
    }

    None
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
