use std::path::{Component, Path, PathBuf};

/// Eagerly generated repository filesystem graph at the workspace root.
pub const FS_GRAPH_FILE: &str = "fs.sfgn";

/// Flat directory at the workspace root storing lazily hydrated code file graphs.
pub const CODE_GRAPH_DIR: &str = "sfgn-codes";

/// Flat directory at the workspace root storing lazily hydrated doc and media graphs.
pub const DOC_GRAPH_DIR: &str = "sfgn-docs";

/// Target SFGN artifact category for a workspace-relative source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatArtifactDomain {
    /// Hydrates into `sfgn-codes/<encoded>.sfgn`.
    Code,
    /// Hydrates into `sfgn-docs/<encoded>.sfgn` (Markdown or media proxy).
    DocOrMedia,
    /// Tracked only inside `fs.sfgn` (no separate hydrated graph).
    FsOnly,
}

/// Returns `true` if `rel_path` points to a generated SFGN root artifact
/// (`fs.sfgn`, `sfgn-codes/...`, or `sfgn-docs/...`) that extractors must skip.
pub fn is_generated_sfgn_artifact(rel_path: &Path) -> bool {
    let Some(first) = rel_path.components().find_map(|c| match c {
        Component::Normal(os) => os.to_str(),
        _ => None,
    }) else {
        return false;
    };

    matches!(first, FS_GRAPH_FILE | CODE_GRAPH_DIR | DOC_GRAPH_DIR)
}

/// Classifies a workspace-relative file path into its flat hydration domain.
pub fn classify_workspace_file(rel_path: &Path) -> FlatArtifactDomain {
    let Some(ext) = rel_path.extension().and_then(|e| e.to_str()) else {
        return FlatArtifactDomain::FsOnly;
    };

    match ext.to_ascii_lowercase().as_str() {
        "rs" | "py" | "ts" | "tsx" | "js" | "jsx" => FlatArtifactDomain::Code,
        "md" | "markdown" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "wav" | "pdf" | "svg" => {
            FlatArtifactDomain::DocOrMedia
        }
        _ => FlatArtifactDomain::FsOnly,
    }
}

/// Encodes a workspace-relative path into a flat filename stem using `:` as separator:
/// - `src/main.rs` -> `src:main.rs`
/// - `examples/no-modules/main.rs` -> `examples:no-modules:main.rs`
pub fn encode_flat_rel_path(rel_path: &Path) -> String {
    let normalized = normalize_rel_slashes(rel_path);
    normalized.replace('/', ":")
}

/// Decodes a flat `:`-delimited filename stem (without `.sfgn`) back to a relative `PathBuf`:
/// - `src:main.rs` -> `src/main.rs`
pub fn decode_flat_stem(encoded_stem: &str) -> PathBuf {
    let rel = encoded_stem.replace(':', "/");
    PathBuf::from(rel)
}

/// Decodes a workspace-relative flat artifact path (e.g. `sfgn-codes/src:main.rs.sfgn`
/// or `sfgn-docs/resources:releasing.md.sfgn`) back into its source `PathBuf`.
pub fn decode_flat_artifact_path(artifact_rel: &str) -> Option<PathBuf> {
    let norm = artifact_rel.replace('\\', "/");
    let after_dir = norm
        .strip_prefix("sfgn-codes/")
        .or_else(|| norm.strip_prefix("sfgn-docs/"))?;
    let stem = after_dir.strip_suffix(".sfgn")?;
    if stem.is_empty() {
        return None;
    }
    Some(decode_flat_stem(stem))
}

/// Returns the workspace-relative artifact path for a code file:
/// `src/main.rs` -> `sfgn-codes/src:main.rs.sfgn`
pub fn flat_code_artifact_rel(rel_source_path: &Path) -> PathBuf {
    PathBuf::from(CODE_GRAPH_DIR).join(format!("{}.sfgn", encode_flat_rel_path(rel_source_path)))
}

/// Returns the workspace-relative artifact path for a doc or media file:
/// `resources/releasing.md` -> `sfgn-docs/resources:releasing.md.sfgn`
pub fn flat_doc_artifact_rel(rel_source_path: &Path) -> PathBuf {
    PathBuf::from(DOC_GRAPH_DIR).join(format!("{}.sfgn", encode_flat_rel_path(rel_source_path)))
}

/// Returns the SFGN portal target string for a code file (`"sfgn-codes/src:main.rs.sfgn"`).
pub fn flat_code_portal(rel_source_path: &Path) -> String {
    format!(
        "{}/{}.sfgn",
        CODE_GRAPH_DIR,
        encode_flat_rel_path(rel_source_path)
    )
}

/// Returns the SFGN portal target string for a doc/media file (`"sfgn-docs/README.md.sfgn"`).
pub fn flat_doc_portal(rel_source_path: &Path) -> String {
    format!(
        "{}/{}.sfgn",
        DOC_GRAPH_DIR,
        encode_flat_rel_path(rel_source_path)
    )
}

/// Returns the target flat artifact path (`sfgn-codes/...` or `sfgn-docs/...`) if the
/// source file supports lazy graph hydration, or `None` for `FsOnly` files.
pub fn flat_artifact_for_source(rel_source_path: &Path) -> Option<PathBuf> {
    match classify_workspace_file(rel_source_path) {
        FlatArtifactDomain::Code => Some(flat_code_artifact_rel(rel_source_path)),
        FlatArtifactDomain::DocOrMedia => Some(flat_doc_artifact_rel(rel_source_path)),
        FlatArtifactDomain::FsOnly => None,
    }
}

fn normalize_rel_slashes(rel_path: &Path) -> String {
    let raw = rel_path.to_string_lossy().replace('\\', "/");
    let mut s = raw.as_str();
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest;
        } else {
            break;
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_flat_paths_roundtrip() {
        let rel = Path::new("./examples/no-modules/main.rs");
        let encoded = encode_flat_rel_path(rel);
        assert_eq!(encoded, "examples:no-modules:main.rs");
        assert_eq!(
            decode_flat_stem(&encoded),
            PathBuf::from("examples/no-modules/main.rs")
        );

        let portal = flat_code_portal(rel);
        assert_eq!(portal, "sfgn-codes/examples:no-modules:main.rs.sfgn");
        assert_eq!(
            decode_flat_artifact_path(&portal),
            Some(PathBuf::from("examples/no-modules/main.rs"))
        );
    }

    #[test]
    fn classifies_workspace_files_and_ignores_generated_artifacts() {
        assert_eq!(
            classify_workspace_file(Path::new("src/lib.rs")),
            FlatArtifactDomain::Code
        );
        assert_eq!(
            classify_workspace_file(Path::new("resources/releasing.md")),
            FlatArtifactDomain::DocOrMedia
        );
        assert_eq!(
            classify_workspace_file(Path::new("resources/banner.svg")),
            FlatArtifactDomain::DocOrMedia
        );
        assert_eq!(
            classify_workspace_file(Path::new("Cargo.toml")),
            FlatArtifactDomain::FsOnly
        );

        assert!(is_generated_sfgn_artifact(Path::new("fs.sfgn")));
        assert!(is_generated_sfgn_artifact(Path::new(
            "sfgn-codes/src:lib.rs.sfgn"
        )));
        assert!(is_generated_sfgn_artifact(Path::new(
            "sfgn-docs/README.md.sfgn"
        )));
        assert!(!is_generated_sfgn_artifact(Path::new("src/lib.rs")));
    }
}
