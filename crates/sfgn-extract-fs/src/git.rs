use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use gix::bstr::{BString, ByteSlice};

/// Aggregated Git repository metadata used to annotate `fs.sfgn` vertices and `>>` edges.
#[derive(Debug, Default)]
pub(crate) struct GitMetadata {
    pub in_repo: bool,
    pub head_commit: Option<String>,
    /// Repo-relative POSIX path -> working-tree / index status (`"modified"`, `"staged"`, `"untracked"`, `"renamed"`).
    pub file_status: HashMap<String, &'static str>,
    /// Repo-relative POSIX path -> short commit hash that most recently modified the file.
    pub last_commit: HashMap<String, String>,
    /// Repo-relative POSIX path -> sorted list of co-modified repo-relative POSIX file paths.
    pub co_changes: HashMap<String, Vec<String>>,
    /// Set of repo-relative POSIX directory paths registered as Git submodules.
    pub submodules: BTreeSet<String>,
    /// Prefix to strip from repo-relative paths when compiling a subdirectory inside a larger Git repo.
    pub subdir_prefix: Option<String>,
}

impl GitMetadata {
    /// Collects Git status, submodule paths, per-file last commits, and recent co-change pairs
    /// for `scan_root` using `gix`. Gracefully returns `in_repo = false` outside Git repositories.
    pub fn collect(
        scan_root: &Path,
        recent_commits_limit: usize,
        co_change_min_commits: usize,
        co_change_max_files_per_commit: usize,
        max_co_changes_per_file: usize,
    ) -> Self {
        let Ok(repo) = gix::discover(scan_root) else {
            return Self::default();
        };

        let Some(work_dir) = repo.workdir() else {
            return Self::default();
        };

        let canon_work = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let canon_scan =
            std::fs::canonicalize(scan_root).unwrap_or_else(|_| scan_root.to_path_buf());

        let subdir_prefix = canon_scan.strip_prefix(&canon_work).ok().and_then(|rel| {
            let s = path_to_posix(rel);
            if s.is_empty() {
                None
            } else {
                Some(format!("{}/", s))
            }
        });

        let mut meta = Self {
            in_repo: true,
            subdir_prefix,
            ..Self::default()
        };

        // 1. Submodules
        if let Ok(Some(submodules)) = repo.submodules() {
            for sub in submodules {
                if let Ok(sub_path) = sub.path() {
                    let posix = normalize_posix_str(&sub_path.to_str_lossy());
                    if !posix.is_empty() {
                        meta.submodules.insert(posix);
                    }
                }
            }
        }

        // 2. Working tree and staged index status
        if let Ok(status_platform) = repo.status(gix::progress::Discard) {
            if let Ok(iter) = status_platform.into_iter(Vec::<BString>::new()) {
                for item in iter.flatten() {
                    match item {
                        gix::status::Item::IndexWorktree(iw) => match iw {
                            gix::status::index_worktree::Item::Modification {
                                rela_path, ..
                            } => {
                                let p = normalize_posix_str(&rela_path.to_str_lossy());
                                meta.file_status.insert(p, "modified");
                            }
                            gix::status::index_worktree::Item::DirectoryContents {
                                entry, ..
                            } => {
                                let p = normalize_posix_str(&entry.rela_path.to_str_lossy());
                                meta.file_status.entry(p).or_insert("untracked");
                            }
                            gix::status::index_worktree::Item::Rewrite {
                                dirwalk_entry, ..
                            } => {
                                let p =
                                    normalize_posix_str(&dirwalk_entry.rela_path.to_str_lossy());
                                meta.file_status.insert(p, "renamed");
                            }
                        },
                        gix::status::Item::TreeIndex(ti) => {
                            let p = normalize_posix_str(&ti.location().to_str_lossy());
                            meta.file_status.entry(p).or_insert("staged");
                        }
                    }
                }
            }
        }

        // 3. Recent commit history: per-file `last_commit` and co-modification frequency matrix
        let Ok(head_id) = repo.head_id() else {
            return meta;
        };
        let full_head = head_id.to_hex().to_string();
        meta.head_commit = Some(short_hash(&full_head));

        let Ok(walk) = repo.rev_walk([head_id]).all() else {
            return meta;
        };

        let mut pair_counts: HashMap<(String, String), usize> = HashMap::new();

        for info in walk.take(recent_commits_limit).flatten() {
            let Ok(commit) = repo.find_commit(info.id) else {
                continue;
            };
            let Ok(tree) = commit.tree() else {
                continue;
            };

            let parent_tree = commit
                .parent_ids()
                .next()
                .and_then(|pid| repo.find_commit(pid).ok())
                .and_then(|p| p.tree().ok());

            let Ok(changes) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
            else {
                continue;
            };

            let commit_short = short_hash(&info.id.to_hex().to_string());
            let mut touched_files: Vec<String> = Vec::with_capacity(changes.len());

            for change in changes {
                if change.entry_mode().is_tree() {
                    continue;
                }
                let path = normalize_posix_str(&change.location().to_str_lossy());
                if path.is_empty() {
                    continue;
                }
                meta.last_commit
                    .entry(path.clone())
                    .or_insert_with(|| commit_short.clone());
                touched_files.push(path);
            }

            touched_files.sort();
            touched_files.dedup();

            if touched_files.len() >= 2 && touched_files.len() <= co_change_max_files_per_commit {
                for i in 0..touched_files.len() {
                    for j in (i + 1)..touched_files.len() {
                        let key = (touched_files[i].clone(), touched_files[j].clone());
                        *pair_counts.entry(key).or_insert(0) += 1;
                    }
                }
            }
        }

        // Build adjacency list of strongly co-modified files sorted by co-change frequency
        let mut adj: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        for ((a, b), count) in pair_counts {
            if count >= co_change_min_commits {
                adj.entry(a.clone()).or_default().push((count, b.clone()));
                adj.entry(b).or_default().push((count, a));
            }
        }

        for (file, mut neighbors) in adj {
            neighbors.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
            neighbors.truncate(max_co_changes_per_file);
            meta.co_changes
                .insert(file, neighbors.into_iter().map(|(_, path)| path).collect());
        }

        meta
    }

    /// Converts a scan-root-relative POSIX path into a repo-relative POSIX path.
    pub fn to_repo_relative(&self, scan_rel_posix: &str) -> String {
        match &self.subdir_prefix {
            Some(prefix) if !scan_rel_posix.is_empty() => format!("{}{}", prefix, scan_rel_posix),
            Some(prefix) => prefix.trim_end_matches('/').to_owned(),
            None => scan_rel_posix.to_owned(),
        }
    }

    pub fn status_for(&self, scan_rel_posix: &str) -> &'static str {
        if !self.in_repo {
            return "untracked_repo";
        }
        let repo_rel = self.to_repo_relative(scan_rel_posix);
        if let Some(&status) = self.file_status.get(&repo_rel) {
            return status;
        }

        // `gix` collapses untracked directories by default; check ancestor directory entries.
        let mut prefix = repo_rel.as_str();
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            if let Some(&status) = self.file_status.get(parent) {
                return status;
            }
            prefix = parent;
        }

        "clean"
    }

    pub fn last_commit_for(&self, scan_rel_posix: &str) -> Option<&str> {
        if !self.in_repo {
            return None;
        }
        let repo_rel = self.to_repo_relative(scan_rel_posix);
        self.last_commit.get(&repo_rel).map(String::as_str)
    }

    pub fn is_submodule(&self, scan_rel_posix: &str) -> bool {
        if !self.in_repo {
            return false;
        }
        let repo_rel = self.to_repo_relative(scan_rel_posix);
        self.submodules.contains(&repo_rel)
    }

    /// Returns scan-root-relative POSIX paths of files frequently co-modified with `scan_rel_posix`.
    pub fn co_changes_for(&self, scan_rel_posix: &str) -> Vec<String> {
        if !self.in_repo {
            return Vec::new();
        }
        let repo_rel = self.to_repo_relative(scan_rel_posix);
        let Some(peers) = self.co_changes.get(&repo_rel) else {
            return Vec::new();
        };

        peers
            .iter()
            .filter_map(|peer_repo_rel| match &self.subdir_prefix {
                Some(prefix) => peer_repo_rel.strip_prefix(prefix).map(ToOwned::to_owned),
                None => Some(peer_repo_rel.clone()),
            })
            .collect()
    }
}

pub(crate) fn path_to_posix(path: &Path) -> String {
    let mut parts = Vec::new();
    for comp in path.components() {
        if let std::path::Component::Normal(os) = comp {
            parts.push(os.to_string_lossy());
        }
    }
    parts.join("/")
}

pub(crate) fn normalize_posix_str(raw: &str) -> String {
    raw.replace('\\', "/").trim_matches('/').to_owned()
}

pub(crate) fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn short_hash(hex: &str) -> String {
    hex.chars().take(7).collect()
}
