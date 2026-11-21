use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sfgn_core::{
    CanonicalNodePath, Document, ExternalPortal, NodeId, ParseOptions, Result, Value,
    serialize_document,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// RAII temporary directory on the real OS filesystem that cleans up on `Drop`.
#[derive(Debug)]
pub struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    pub fn new(prefix: &str) -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("{}_{}_{}_{}", prefix, pid, nanos, seq));
        fs::create_dir_all(&dir)?;
        Ok(Self { root: dir })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, rel_path: impl AsRef<Path>) -> PathBuf {
        self.root.join(rel_path)
    }

    /// Writes UTF-8 SFGN content to a relative path on disk, creating parent directories if needed.
    pub fn write_sfgn(&self, rel_path: impl AsRef<Path>, content: &str) -> io::Result<PathBuf> {
        self.write_bytes(rel_path, content.as_bytes())
    }

    /// Writes raw bytes to a relative path on disk, creating parent directories if needed.
    pub fn write_bytes(&self, rel_path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<PathBuf> {
        let full_path = self.path(rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full_path, bytes)?;
        Ok(full_path)
    }

    /// Reads raw bytes from a relative path on disk.
    pub fn read_bytes(&self, rel_path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        fs::read(self.path(rel_path))
    }

    /// Reads a UTF-8 string from a relative path on disk.
    pub fn read_string(&self, rel_path: impl AsRef<Path>) -> io::Result<String> {
        fs::read_to_string(self.path(rel_path))
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Multi-file federated `.sfgn` loader resolving `<"relative/path.sfgn">` subgraph portals
/// across real filesystem directories (Spec § 3.4, § 6.3).
#[derive(Debug, Default)]
pub struct FsPortalResolver {
    buffers: HashMap<PathBuf, Arc<Vec<u8>>>,
    options: ParseOptions,
}

impl FsPortalResolver {
    pub fn new(options: ParseOptions) -> Self {
        Self {
            buffers: HashMap::new(),
            options,
        }
    }

    /// Loads and caches the raw bytes of an `.sfgn` file from disk as a shared buffer.
    /// Returning `Arc<Vec<u8>>` decouples `Document<'a>` lifetimes from `&mut self`.
    pub fn load_buffer(&mut self, path: &Path) -> io::Result<Arc<Vec<u8>>> {
        let canonical_key = fs::canonicalize(path)?;
        if let Some(existing) = self.buffers.get(&canonical_key) {
            return Ok(Arc::clone(existing));
        }
        let bytes = Arc::new(fs::read(&canonical_key)?);
        self.buffers.insert(canonical_key, Arc::clone(&bytes));
        Ok(bytes)
    }

    /// Parses a loaded byte buffer using this resolver's `ParseOptions`.
    pub fn parse_buffer<'a>(&self, bytes: &'a [u8]) -> Result<Document<'a>> {
        Document::parse_bytes_with_options(bytes, self.options)
    }

    /// Resolves an external portal `<"portal_file">[sub_path]` relative to `base_file` on disk,
    /// returning the raw zero-copy UTF-8 node slice from the target `.sfgn` file.
    pub fn resolve_portal_slice(
        &mut self,
        base_file: &Path,
        portal_file: &str,
        sub_path: Option<&CanonicalNodePath>,
    ) -> io::Result<Result<Option<String>>> {
        let base_dir = base_file.parent().unwrap_or_else(|| Path::new("."));
        let target_path = base_dir.join(portal_file);
        let target_bytes = self.load_buffer(&target_path)?;
        let parsed = match self.parse_buffer(&target_bytes) {
            Ok(doc) => doc,
            Err(err) => return Ok(Err(err)),
        };

        let slice = match sub_path {
            None => parsed.node_slice(NodeId::ROOT).map(str::to_owned),
            Some(path) => parsed
                .node_by_canonical_path(path)
                .map(|node| node.raw_slice().to_owned()),
        };

        Ok(Ok(slice))
    }
}

/// Verifies semantic and structural equivalence between two parsed `Document`s across a
/// serialization -> disk write -> disk read -> deserialization round-trip.
pub fn assert_documents_semantically_equal(left: &Document<'_>, right: &Document<'_>) {
    assert_eq!(
        left.len(),
        right.len(),
        "node count mismatch across round-trip"
    );

    for (l_node, r_node) in left.nodes().zip(right.nodes()) {
        assert_eq!(l_node.id(), r_node.id());
        assert_eq!(l_node.canonical_path(), r_node.canonical_path());

        let l_children: Vec<Option<CanonicalNodePath>> = l_node
            .children()
            .map(|c| c.canonical_path().cloned())
            .collect();
        let r_children: Vec<Option<CanonicalNodePath>> = r_node
            .children()
            .map(|c| c.canonical_path().cloned())
            .collect();
        assert_eq!(l_children, r_children);

        let l_rels: Vec<Option<CanonicalNodePath>> = l_node
            .relatives()
            .map(|r| r.canonical_path().cloned())
            .collect();
        let r_rels: Vec<Option<CanonicalNodePath>> = r_node
            .relatives()
            .map(|r| r.canonical_path().cloned())
            .collect();
        assert_eq!(l_rels, r_rels);

        let l_portals: Vec<ExternalPortal<'_>> = l_node.external_portals().collect();
        let r_portals: Vec<ExternalPortal<'_>> = r_node.external_portals().collect();
        assert_eq!(l_portals, r_portals);

        let l_props: Vec<(String, Vec<usize>, Value<'_>)> = l_node
            .properties()
            .map(|p| {
                (
                    p.key_name().into_owned(),
                    p.key_indices(),
                    p.decode().expect("left prop decodes"),
                )
            })
            .collect();
        let r_props: Vec<(String, Vec<usize>, Value<'_>)> = r_node
            .properties()
            .map(|p| {
                (
                    p.key_name().into_owned(),
                    p.key_indices(),
                    p.decode().expect("right prop decodes"),
                )
            })
            .collect();
        assert_eq!(l_props, r_props);
    }

    let left_canonical = serialize_document(left).expect("left serializes");
    let right_canonical = serialize_document(right).expect("right serializes");
    assert_eq!(left_canonical, right_canonical);
}
