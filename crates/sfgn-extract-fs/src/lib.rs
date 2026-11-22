pub mod error;
pub mod git;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use sfgn_core::{
    EdgeTarget, FlatArtifactDomain, IndentStyle, NodeHandle, SfgnBuilder, SfgnValue,
    classify_workspace_file, flat_code_portal, flat_doc_portal, is_generated_sfgn_artifact,
};

pub use error::{FsExtractError, Result};
use git::{GitMetadata, normalize_lexical_path, path_to_posix};

/// Configuration options for the `sfgn-extract-fs` filesystem and Git graph compiler.
#[derive(Debug, Clone)]
pub struct FsExtractOptions {
    /// Optional explicit root label stored in root metadata (defaults to directory name).
    pub root_name: Option<String>,
    /// Whether to follow symbolic links during traversal (default `false` so symlinks emit `>>` edges).
    pub follow_symlinks: bool,
    /// Optional maximum directory traversal depth.
    pub max_depth: Option<usize>,
    /// Number of recent Git commits inspected for `last_commit` and co-modification `>>` edges.
    pub recent_commits_limit: usize,
    /// Minimum shared commit count required to emit a co-modification `>>` edge between two files.
    pub co_change_min_commits: usize,
    /// Maximum files touched in a single commit before skipping bulk/formatting commits for co-change analysis.
    pub co_change_max_files_per_commit: usize,
    /// Maximum co-modification `>>` edges emitted per file.
    pub max_co_changes_per_file: usize,
    /// Depth 1 indentation style for emitted `fs.sfgn`.
    pub indent: IndentStyle,
}

impl Default for FsExtractOptions {
    fn default() -> Self {
        Self {
            root_name: None,
            follow_symlinks: false,
            max_depth: None,
            recent_commits_limit: 32,
            co_change_min_commits: 2,
            co_change_max_files_per_commit: 25,
            max_co_changes_per_file: 5,
            indent: IndentStyle::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Debug, Clone)]
struct ScannedEntry {
    abs_path: PathBuf,
    name: String,
    kind: EntryKind,
    size_bytes: u64,
    lines: u64,
    ext: Option<String>,
}

/// Deterministic `.gitignore`-aware filesystem and Git compiler emitting validated `fs.sfgn`.
#[derive(Debug, Clone, Default)]
pub struct FsExtractor {
    options: FsExtractOptions,
}

impl FsExtractor {
    pub fn new(options: FsExtractOptions) -> Self {
        Self { options }
    }

    /// Walks `root_dir`, collects Git metadata, and compiles the directory tree into canonical `fs.sfgn`.
    pub fn extract(&self, root_dir: impl AsRef<Path>) -> Result<String> {
        let root_path = root_dir.as_ref();
        let canon_root = fs::canonicalize(root_path).map_err(|source| FsExtractError::Io {
            path: root_path.to_path_buf(),
            source,
        })?;

        let git_meta = GitMetadata::collect(
            &canon_root,
            self.options.recent_commits_limit,
            self.options.co_change_min_commits,
            self.options.co_change_max_files_per_commit,
            self.options.max_co_changes_per_file,
        );

        let filter_root = canon_root.clone();
        let mut walker = WalkBuilder::new(&canon_root);
        walker
            .hidden(false)
            .require_git(false)
            .follow_links(self.options.follow_symlinks)
            .max_depth(self.options.max_depth)
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

        let mut entries_by_rel: BTreeMap<String, ScannedEntry> = BTreeMap::new();

        for result in walker.build() {
            let dir_entry = result?;
            let path = dir_entry.path();
            if path == canon_root {
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

            let metadata = fs::symlink_metadata(path).map_err(|source| FsExtractError::Io {
                path: path.to_path_buf(),
                source,
            })?;

            let file_type = metadata.file_type();
            let kind = if file_type.is_symlink() {
                EntryKind::Symlink
            } else if file_type.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            };

            let name = rel_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel_posix.clone());

            let ext = if kind == EntryKind::File {
                rel_path
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
            } else {
                None
            };

            let size_bytes = metadata.len();
            let lines = if kind == EntryKind::File {
                count_text_lines(path, size_bytes)
            } else {
                0
            };

            entries_by_rel.insert(
                rel_posix,
                ScannedEntry {
                    abs_path: path.to_path_buf(),
                    name,
                    kind,
                    size_bytes,
                    lines,
                    ext,
                },
            );
        }

        let mut builder = SfgnBuilder::new().with_indent(self.options.indent);
        let root_label = self.options.root_name.clone().unwrap_or_else(|| {
            canon_root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "root".to_owned())
        });

        let total_dirs = entries_by_rel
            .values()
            .filter(|e| e.kind == EntryKind::Directory)
            .count() as u64;
        let total_files = entries_by_rel
            .values()
            .filter(|e| e.kind != EntryKind::Directory)
            .count() as u64;

        builder.set_root_prop("kind", "filesystem");
        builder.set_root_prop("root_name", root_label);
        builder.set_root_prop("total_dirs", total_dirs);
        builder.set_root_prop("total_files", total_files);
        if let Some(head) = &git_meta.head_commit {
            builder.set_root_prop("git_head", head.as_str());
        }

        // Register vertices in BTreeMap lexical order so every parent directory is created before its children.
        let mut handle_by_rel: HashMap<String, NodeHandle> =
            HashMap::with_capacity(entries_by_rel.len());

        for (rel_posix, entry) in &entries_by_rel {
            let parent_handle = match rel_posix.rsplit_once('/') {
                Some((parent_rel, _)) => handle_by_rel
                    .get(parent_rel)
                    .copied()
                    .unwrap_or(NodeHandle::ROOT),
                None => NodeHandle::ROOT,
            };

            let handle = builder.add_node(parent_handle, &entry.name);
            handle_by_rel.insert(rel_posix.clone(), handle);

            let is_submodule =
                entry.kind == EntryKind::Directory && git_meta.is_submodule(rel_posix);

            let kind_str = match entry.kind {
                EntryKind::Directory => "directory",
                EntryKind::File => "file",
                EntryKind::Symlink => "symlink",
            };
            builder.add_prop(handle, "kind", kind_str);
            builder.add_prop(handle, "path", rel_posix.as_str());

            if is_submodule {
                builder.add_prop(handle, "git_submodule", true);
                builder.add_relative(NodeHandle::ROOT, handle);
            }

            if entry.kind != EntryKind::Directory {
                builder.add_prop(handle, "size_bytes", entry.size_bytes);
                builder.add_prop(handle, "lines", entry.lines);
                match &entry.ext {
                    Some(ext) => builder.add_prop(handle, "ext", ext.as_str()),
                    None => builder.add_prop(handle, "ext", SfgnValue::Null),
                }
                builder.add_prop(handle, "git_status", git_meta.status_for(rel_posix));
                match git_meta.last_commit_for(rel_posix) {
                    Some(commit) => builder.add_prop(handle, "last_commit", commit),
                    None => builder.add_prop(handle, "last_commit", SfgnValue::Null),
                }
            }
        }

        // Wire `>>` relative edges: symlinks, recent Git co-modifications, and lazy flat portals.
        for (rel_posix, entry) in &entries_by_rel {
            let Some(&from_handle) = handle_by_rel.get(rel_posix) else {
                continue;
            };

            if entry.kind == EntryKind::Symlink {
                if let Some(target_handle) =
                    resolve_symlink_target(&canon_root, &entry.abs_path, &handle_by_rel)
                {
                    if target_handle != from_handle {
                        builder.add_relative(from_handle, target_handle);
                    }
                }
            }

            if entry.kind == EntryKind::File {
                for peer_rel in git_meta.co_changes_for(rel_posix) {
                    if let Some(&peer_handle) = handle_by_rel.get(&peer_rel) {
                        if peer_handle != from_handle {
                            builder.add_relative(from_handle, peer_handle);
                        }
                    }
                }
                let rel_path = Path::new(rel_posix);
                match classify_workspace_file(rel_path) {
                    FlatArtifactDomain::Code => {
                        let portal = flat_code_portal(rel_path);
                        builder.add_relative(from_handle, EdgeTarget::external_root(portal));
                    }
                    FlatArtifactDomain::DocOrMedia => {
                        let portal = flat_doc_portal(rel_path);
                        builder.add_relative(from_handle, EdgeTarget::external_root(portal));
                    }
                    FlatArtifactDomain::FsOnly => {}
                }
            }
        }

        Ok(builder.build()?)
    }
}

/// Convenience function compiling `root_dir` into `fs.sfgn` using default `FsExtractOptions`.
pub fn extract_fs_to_sfgn(root_dir: impl AsRef<Path>) -> Result<String> {
    FsExtractor::default().extract(root_dir)
}

fn resolve_symlink_target(
    canon_root: &Path,
    symlink_path: &Path,
    handle_by_rel: &HashMap<String, NodeHandle>,
) -> Option<NodeHandle> {
    let raw_target = fs::read_link(symlink_path).ok()?;
    let joined = if raw_target.is_absolute() {
        raw_target
    } else {
        symlink_path.parent()?.join(raw_target)
    };

    let resolved = fs::canonicalize(&joined).unwrap_or_else(|_| normalize_lexical_path(&joined));
    let rel = resolved.strip_prefix(canon_root).ok()?;
    let rel_posix = path_to_posix(rel);
    handle_by_rel.get(&rel_posix).copied()
}

/// Counts text lines for valid UTF-8 non-binary files; returns `0` for binary files or files > 16 MiB.
fn count_text_lines(path: &Path, size_bytes: u64) -> u64 {
    const MAX_LINE_SCAN_BYTES: u64 = 16 * 1024 * 1024;
    if size_bytes == 0 || size_bytes > MAX_LINE_SCAN_BYTES {
        return 0;
    }

    let Ok(mut file) = fs::File::open(path) else {
        return 0;
    };
    let mut buf = Vec::with_capacity(size_bytes as usize);
    if file.read_to_end(&mut buf).is_err() {
        return 0;
    }

    if buf.contains(&0x00) || std::str::from_utf8(&buf).is_err() {
        return 0;
    }

    let newlines = buf.iter().filter(|&&b| b == b'\n').count() as u64;
    if buf.ends_with(b"\n") {
        newlines
    } else {
        newlines + 1
    }
}
