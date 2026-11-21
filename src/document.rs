use std::borrow::Cow;
use std::collections::HashSet;

use crate::ParseOptions;
use crate::error::{Diagnostic, ErrorCode, LineCol, Result};
use crate::lexer::Lexer;
use crate::scanner::{scan_tape, scan_tape_bytes};
use crate::tape::{ByteRange, NodeEntry, NodeId, PropertyEntry, PropertyKind, StructuralTape};
use crate::topology::{
    CanonicalNodePath, RelationTarget, TopologyIndex, build_topology, parse_canonical_node_path,
};

/// A validated, zero-copy SFGN v1.0 document supporting `O(1)` raw node slicing
/// and progressive Level-of-Detail graph traversal (Spec § 6.3)[span_10](start_span)[span_10](end_span).
#[derive(Debug, Clone)]
pub struct Document<'a> {
    src: &'a str,
    tape: StructuralTape,
    topology: TopologyIndex,
}

impl<'a> Document<'a> {
    /// Parses and validates an SFGN UTF-8 string using default options (`OrphanPolicy::StrictError`).
    pub fn parse(src: &'a str) -> Result<Self> {
        Self::parse_with_options(src, ParseOptions::default())
    }

    /// Parses and validates an SFGN UTF-8 string with explicit `ParseOptions`.
    pub fn parse_with_options(src: &'a str, options: ParseOptions) -> Result<Self> {
        let tape = scan_tape(src)?;
        let topology = build_topology(src, &tape, options)?;
        Ok(Self {
            src,
            tape,
            topology,
        })
    }

    /// Parses and validates raw UTF-8 bytes (rejecting leading UTF-8 BOM with `E001`) using default options.
    pub fn parse_bytes(bytes: &'a [u8]) -> Result<Self> {
        Self::parse_bytes_with_options(bytes, ParseOptions::default())
    }

    /// Parses and validates raw UTF-8 bytes with explicit `ParseOptions`.
    pub fn parse_bytes_with_options(bytes: &'a [u8], options: ParseOptions) -> Result<Self> {
        let tape = scan_tape_bytes(bytes)?;
        // SAFETY: `scan_tape_bytes` already validated `bytes` as valid UTF-8 without BOM.
        let src = std::str::from_utf8(bytes).expect("scan_tape_bytes validated UTF-8");
        let topology = build_topology(src, &tape, options)?;
        Ok(Self {
            src,
            tape,
            topology,
        })
    }

    /// Returns the underlying SFGN UTF-8 source buffer.
    #[inline]
    pub fn source(&self) -> &'a str {
        self.src
    }

    /// Returns the Pass 1 `StructuralTape`.
    #[inline]
    pub fn tape(&self) -> &StructuralTape {
        &self.tape
    }

    /// Returns the Pass 2 `TopologyIndex`.
    #[inline]
    pub fn topology(&self) -> &TopologyIndex {
        &self.topology
    }

    /// Returns the total number of declared nodes in this document (including root `>:`).
    #[inline]
    pub fn len(&self) -> usize {
        self.tape.nodes.len()
    }

    /// Always returns `false` because every valid SFGN document has at least the root node `>:`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.tape.nodes.is_empty()
    }

    /// Returns a view of the mandatory file-root vertex `>:` (`NodeId::ROOT` / index `0`)[span_11](start_span)[span_11](end_span)[span_12](start_span)[span_12](end_span)[span_13](start_span)[span_13](end_span).
    #[inline]
    pub fn root(&self) -> NodeView<'_, 'a> {
        NodeView {
            doc: self,
            id: NodeId::ROOT,
        }
    }

    /// Returns a `NodeView` for `id`, or `None` if `id` is out of bounds.
    #[inline]
    pub fn node(&self, id: NodeId) -> Option<NodeView<'_, 'a>> {
        if id.0 < self.tape.nodes.len() {
            Some(NodeView { doc: self, id })
        } else {
            None
        }
    }

    /// Resolves a canonical `CanonicalNodePath` in `O(1)` average time and returns its `NodeView`.
    #[inline]
    pub fn node_by_canonical_path(&self, path: &CanonicalNodePath) -> Option<NodeView<'_, 'a>> {
        self.topology
            .lookup(path)
            .map(|id| NodeView { doc: self, id })
    }

    /// Parses an SFGN `NodePath` (or `">:""`) and resolves its `NodeView`.
    pub fn node_by_path(&self, raw_path: &str) -> Result<Option<NodeView<'_, 'a>>> {
        Ok(self
            .topology
            .lookup_str(raw_path)?
            .map(|id| NodeView { doc: self, id }))
    }

    /// Zero-copy `O(1)` raw UTF-8 byte-slice extraction for `id` (`&buffer[start_byte..end_byte]`)[span_14](start_span)[span_14](end_span)[span_15](start_span)[span_15](end_span).
    #[inline]
    pub fn node_slice(&self, id: NodeId) -> Option<&'a str> {
        self.tape
            .node(id)
            .map(|entry| entry.full_span.slice_str(self.src))
    }

    /// Resolves `raw_path` and extracts its raw contiguous UTF-8 node slice in `O(1)` lookup time[span_16](start_span)[span_16](end_span)[span_17](start_span)[span_17](end_span).
    pub fn node_slice_by_path(&self, raw_path: &str) -> Result<Option<&'a str>> {
        Ok(self
            .topology
            .lookup_str(raw_path)?
            .and_then(|id| self.node_slice(id)))
    }

    /// Iterates over all declared nodes in source declaration order (starting with `>:`).
    pub fn nodes(&self) -> impl Iterator<Item = NodeView<'_, 'a>> {
        (0..self.tape.nodes.len()).map(move |idx| NodeView {
            doc: self,
            id: NodeId(idx),
        })
    }
}

/// Lightweight handle for inspecting a single SFGN vertex and lazily traversing its neighborhood.
#[derive(Debug, Clone, Copy)]
pub struct NodeView<'doc, 'a> {
    doc: &'doc Document<'a>,
    id: NodeId,
}

impl<'doc, 'a> NodeView<'doc, 'a> {
    #[inline]
    fn entry(&self) -> &'doc NodeEntry {
        &self.doc.tape.nodes[self.id.0]
    }

    /// Returns this vertex's `NodeId`.
    #[inline]
    pub fn id(&self) -> NodeId {
        self.id
    }

    /// Returns `true` if this vertex is the file root `>:`.
    #[inline]
    pub fn is_root(&self) -> bool {
        self.id == NodeId::ROOT
    }

    /// Returns the raw header path slice (`">:"` for root, or `NodePath` excluding `:`).
    #[inline]
    pub fn raw_header_path(&self) -> &'a str {
        self.entry().path_span.slice_str(self.doc.src)
    }

    /// Returns the normalized canonical `NodePath` (`None` for the file root `>:`).
    #[inline]
    pub fn canonical_path(&self) -> Option<&'doc CanonicalNodePath> {
        self.doc.topology.canonical_path(self.id)
    }

    /// Returns the contiguous byte range `[start_byte..end_byte)` encapsulating this node[span_18](start_span)[span_18](end_span)[span_19](start_span)[span_19](end_span).
    #[inline]
    pub fn byte_range(&self) -> ByteRange {
        self.entry().full_span
    }

    /// Returns the exact zero-copy UTF-8 slice encapsulating this node[span_20](start_span)[span_20](end_span)[span_21](start_span)[span_21](end_span).
    #[inline]
    pub fn raw_slice(&self) -> &'a str {
        self.entry().full_span.slice_str(self.doc.src)
    }

    /// Returns the 1-based `line:col` position of this node's header.
    #[inline]
    pub fn header_loc(&self) -> LineCol {
        self.entry().header_loc
    }

    /// Returns this node's containment parent in the tree rooted at `>:` (`None` for `>:`).
    #[inline]
    pub fn parent(&self) -> Option<NodeView<'doc, 'a>> {
        self.doc.topology.parent_of(self.id).map(|pid| NodeView {
            doc: self.doc,
            id: pid,
        })
    }

    /// Returns all parsed `>` child relation targets declared on this node (including external portals).
    #[inline]
    pub fn child_targets(&self) -> &'doc [RelationTarget] {
        self.doc.topology.children(self.id)
    }

    /// Returns all parsed `>>` relative relation targets declared on this node (including external portals).
    #[inline]
    pub fn relative_targets(&self) -> &'doc [RelationTarget] {
        self.doc.topology.relatives(self.id)
    }

    /// Iterates over resolved local child vertices along the `>` containment axis
    /// (including adopted orphans on `>:` when `OrphanPolicy::AttachToRoot` is enabled).
    pub fn children(&self) -> impl Iterator<Item = NodeView<'doc, 'a>> + 'doc {
        let doc = self.doc;
        doc.topology
            .local_child_ids(self.id)
            .iter()
            .copied()
            .map(move |cid| NodeView { doc, id: cid })
    }

    /// Iterates over resolved local relative vertices along the `>>` associative axis.
    pub fn relatives(&self) -> impl Iterator<Item = NodeView<'doc, 'a>> + 'doc {
        let doc = self.doc;
        doc.topology
            .relatives(self.id)
            .iter()
            .filter_map(move |target| match target {
                RelationTarget::Local {
                    node_id: Some(rid), ..
                } => Some(NodeView { doc, id: *rid }),
                _ => None,
            })
    }

    /// Iterates over the deduplicated union of local `>` children and `>>` relatives,
    /// yielding this vertex's complete local neighborhood (Spec § 3.3)[span_22](start_span)[span_22](end_span).
    pub fn neighbors(&self) -> impl Iterator<Item = NodeView<'doc, 'a>> + 'doc {
        let mut seen = HashSet::new();
        self.children()
            .chain(self.relatives())
            .filter(move |node| seen.insert(node.id()))
    }

    /// Iterates over all external subgraph portal targets (`<"File.sfgn">`) referenced via `>` or `>>`[span_23](start_span)[span_23](end_span).
    pub fn external_portals(&self) -> impl Iterator<Item = ExternalPortal<'doc>> + 'doc {
        let child_iter = self.child_targets().iter().filter_map(|t| match t {
            RelationTarget::External {
                file_path,
                sub_path,
                ..
            } => Some(ExternalPortal {
                axis: RelationAxis::Child,
                file_path: file_path.as_str(),
                sub_path: sub_path.as_ref(),
            }),
            RelationTarget::Local { .. } => None,
        });

        let rel_iter = self.relative_targets().iter().filter_map(|t| match t {
            RelationTarget::External {
                file_path,
                sub_path,
                ..
            } => Some(ExternalPortal {
                axis: RelationAxis::Relative,
                file_path: file_path.as_str(),
                sub_path: sub_path.as_ref(),
            }),
            RelationTarget::Local { .. } => None,
        });

        child_iter.chain(rel_iter)
    }

    /// Iterates over all properties owned by this node in physical declaration order.
    pub fn properties(&self) -> impl Iterator<Item = PropertyView<'a>> + 'doc {
        let src = self.doc.src;
        self.doc
            .tape
            .node_properties(self.entry())
            .iter()
            .copied()
            .map(move |entry| PropertyView { src, entry })
    }

    /// Iterates over all properties whose unescaped base key name matches `key_name`,
    /// preserving physical declaration order for sequential repeated keys (Spec § 4.3)[span_24](start_span)[span_24](end_span).
    pub fn properties_named<'k>(
        &self,
        key_name: &'k str,
    ) -> impl Iterator<Item = PropertyView<'a>> + 'k
    where
        'doc: 'k,
    {
        self.properties()
            .filter(move |prop| prop.key_name() == key_name)
    }

    /// Returns the first property whose unescaped base key name matches `key_name`.
    pub fn property(&self, key_name: &str) -> Option<PropertyView<'a>> {
        self.properties_named(key_name).next()
    }
}

/// Direction of an edge (`>` child vs. `>>` relative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationAxis {
    Child,
    Relative,
}

/// Borrowed view of an external subgraph portal `<"File.sfgn">[NodePath]` (Spec § 3.4)[span_25](start_span)[span_25](end_span).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalPortal<'a> {
    pub axis: RelationAxis,
    pub file_path: &'a str,
    pub sub_path: Option<&'a CanonicalNodePath>,
}

/// Parsed property key consisting of its unescaped name and zero or more ordinal indices (`#i#j`)[span_26](start_span)[span_26](end_span).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyKey<'a> {
    pub name: Cow<'a, str>,
    pub indices: Vec<usize>,
}

/// Zero-copy handle to a single property entry on the Property Tape.
#[derive(Debug, Clone, Copy)]
pub struct PropertyView<'a> {
    src: &'a str,
    entry: PropertyEntry,
}

impl<'a> PropertyView<'a> {
    /// Returns the raw property key slice including quotes and any `#index` chain.
    #[inline]
    pub fn raw_key(&self) -> &'a str {
        self.entry.key_span.slice_str(self.src)
    }

    /// Parses the property key into its unescaped base name and ordinal index chain (`#i#j`)[span_27](start_span)[span_27](end_span).
    pub fn parsed_key(&self) -> PropertyKey<'a> {
        parse_property_key(self.raw_key())
    }

    /// Returns the unescaped property key name (zero-copy when no escapes are present).
    pub fn key_name(&self) -> Cow<'a, str> {
        self.parsed_key().name
    }

    /// Returns the ordinal index chain on this property key (empty if unindexed).
    pub fn key_indices(&self) -> Vec<usize> {
        self.parsed_key().indices
    }

    /// Returns the syntactic value discriminant (`Scalar`, `FlatCollection`, or `MultilineBlock`).
    #[inline]
    pub fn kind(&self) -> PropertyKind {
        self.entry.kind
    }

    /// Returns the raw value payload slice from the source buffer.
    #[inline]
    pub fn raw_value(&self) -> &'a str {
        self.entry.val_span.slice_str(self.src)
    }

    /// Returns the optional multiline format tag (e.g., `Some("rust")`, `Some("csv")`).
    #[inline]
    pub fn format_tag(&self) -> Option<&'a str> {
        self.entry.tag_span.map(|s| s.slice_str(self.src))
    }

    /// Returns the 1-based `line:col` location of this property entry.
    #[inline]
    pub fn loc(&self) -> LineCol {
        self.entry.loc
    }

    /// Lazily decodes this property's value into a typed `Value<'a>`.
    pub fn decode(&self) -> Result<Value<'a>> {
        let raw = self.raw_value();
        match self.entry.kind {
            PropertyKind::Scalar => Ok(Value::Scalar(decode_scalar(
                raw,
                self.entry.loc,
                self.entry.val_span.start,
            )?)),
            PropertyKind::FlatCollection => Ok(Value::FlatCollection(decode_flat_collection(
                raw,
                self.entry.loc,
                self.entry.val_span.start,
            )?)),
            PropertyKind::MultilineBlock => Ok(Value::Multiline(MultilinePayload {
                tag: self.format_tag(),
                raw,
            })),
        }
    }
}

/// Decoded SFGN property value (Spec § 4)[span_28](start_span)[span_28](end_span).
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Scalar(Scalar<'a>),
    FlatCollection(Vec<Scalar<'a>>),
    Multiline(MultilinePayload<'a>),
}

/// Decoded SFGN atomic scalar value (Spec § 4.1)[span_29](start_span)[span_29](end_span).
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar<'a> {
    String(Cow<'a, str>),
    Number(Number<'a>),
    Bool(bool),
    Null,
}

impl<'a> Scalar<'a> {
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    #[inline]
    pub fn as_number(&self) -> Option<Number<'a>> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    #[inline]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

/// Zero-copy SFGN numeric literal wrapper offering lossless raw access and typed conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Number<'a> {
    raw: &'a str,
}

impl<'a> Number<'a> {
    #[inline]
    pub const fn new(raw: &'a str) -> Self {
        Self { raw }
    }

    /// Returns the exact numeric token slice from the SFGN source.
    #[inline]
    pub const fn raw(&self) -> &'a str {
        self.raw
    }

    #[inline]
    pub fn as_i64(&self) -> Option<i64> {
        self.raw.parse::<i64>().ok()
    }

    #[inline]
    pub fn as_u64(&self) -> Option<u64> {
        self.raw.parse::<u64>().ok()
    }

    #[inline]
    pub fn as_f64(&self) -> Option<f64> {
        self.raw.parse::<f64>().ok()
    }
}

/// Zero-copy view of an opaque `"""..."""` multiline block payload (Spec § 2.5)[span_30](start_span)[span_30](end_span).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultilinePayload<'a> {
    pub tag: Option<&'a str>,
    pub raw: &'a str,
}

impl<'a> MultilinePayload<'a> {
    /// Returns the payload text, unescaping leading `[OWS]\"""` lines to `[OWS]"""` (Spec § 2.5)[span_31](start_span)[span_31](end_span).
    /// Borrows `self.raw` with zero allocation when no escaped triple quotes appear at line starts.
    pub fn decode_text(&self) -> Cow<'a, str> {
        let mut needs_unescape = false;
        for line in self.raw.split_inclusive('\n') {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if trimmed.starts_with("\\\"\"\"") {
                needs_unescape = true;
                break;
            }
        }

        if !needs_unescape {
            return Cow::Borrowed(self.raw);
        }

        let mut out = String::with_capacity(self.raw.len());
        for line in self.raw.split_inclusive('\n') {
            let ws_len = line.len() - line.trim_start_matches([' ', '\t']).len();
            let rest = &line[ws_len..];
            if let Some(after_slash) = rest.strip_prefix("\\\"\"\"") {
                out.push_str(&line[..ws_len]);
                out.push_str("\"\"\"");
                out.push_str(after_slash);
            } else {
                out.push_str(line);
            }
        }
        Cow::Owned(out)
    }
}

fn parse_property_key<'a>(raw_key: &'a str) -> PropertyKey<'a> {
    // Pass 1 already verified `raw_key` starts with a valid quoted string followed by `{ [OWS] "#" Digits }`.
    let mut lex = Lexer::new(raw_key).expect("validated UTF-8 key");
    let quoted_span = lex.scan_quoted_string().expect("validated quoted key");
    let name = unescape_quoted_cow(quoted_span.slice_str(raw_key));

    let mut indices = Vec::new();
    while !lex.is_eof() {
        lex.skip_ows();
        if lex.peek() == Some(b'#') {
            lex.advance_ascii(1);
            let start = lex.pos();
            while matches!(lex.peek(), Some(b'0'..=b'9')) {
                lex.advance_ascii(1);
            }
            if let Ok(idx) = raw_key[start..lex.pos()].parse::<usize>() {
                indices.push(idx);
            }
        } else {
            break;
        }
    }

    PropertyKey { name, indices }
}

fn decode_scalar<'a>(raw: &'a str, loc: LineCol, offset: usize) -> Result<Scalar<'a>> {
    if raw == "true" {
        Ok(Scalar::Bool(true))
    } else if raw == "false" {
        Ok(Scalar::Bool(false))
    } else if raw == "null" {
        Ok(Scalar::Null)
    } else if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        Ok(Scalar::String(unescape_quoted_cow(raw)))
    } else if !raw.is_empty() {
        Ok(Scalar::Number(Number::new(raw)))
    } else {
        Err(Diagnostic::new(
            ErrorCode::E009,
            loc,
            offset,
            "empty scalar value",
        ))
    }
}

fn decode_flat_collection<'a>(
    raw: &'a str,
    loc: LineCol,
    offset: usize,
) -> Result<Vec<Scalar<'a>>> {
    let mut lex = Lexer::new(raw)?;
    if lex.peek() != Some(b'[') {
        return Err(Diagnostic::new(
            ErrorCode::E012,
            loc,
            offset,
            "expected '[' at start of collection",
        ));
    }
    lex.advance_ascii(1);
    lex.skip_ows();

    let mut items = Vec::new();
    if lex.peek() == Some(b']') {
        return Ok(items);
    }

    loop {
        let item_start = lex.pos();
        let scalar = match lex.peek() {
            Some(b'"') => {
                let span = lex.scan_quoted_string()?;
                Scalar::String(unescape_quoted_cow(span.slice_str(raw)))
            }
            Some(b't') if lex.starts_with(b"true") => {
                lex.advance_ascii(4);
                Scalar::Bool(true)
            }
            Some(b'f') if lex.starts_with(b"false") => {
                lex.advance_ascii(5);
                Scalar::Bool(false)
            }
            Some(b'n') if lex.starts_with(b"null") => {
                lex.advance_ascii(4);
                Scalar::Null
            }
            Some(b'-' | b'0'..=b'9') => {
                let span = lex.scan_number().ok_or_else(|| {
                    Diagnostic::new(ErrorCode::E012, loc, offset + item_start, "invalid number")
                })?;
                Scalar::Number(Number::new(span.slice_str(raw)))
            }
            _ => {
                return Err(Diagnostic::new(
                    ErrorCode::E012,
                    loc,
                    offset + item_start,
                    "invalid element in 1D collection",
                ));
            }
        };
        items.push(scalar);

        lex.skip_ows();
        match lex.peek() {
            Some(b',') => {
                lex.advance_ascii(1);
                lex.skip_ows();
                if lex.peek() == Some(b']') {
                    break;
                }
            }
            Some(b']') => break,
            _ => {
                return Err(Diagnostic::new(
                    ErrorCode::E012,
                    loc,
                    offset + lex.pos(),
                    "expected ',' or ']' in 1D collection",
                ));
            }
        }
    }

    Ok(items)
}

/// Zero-copy quoted string decoder: borrows `&'a str` directly when no `\` escape is present.
pub fn unescape_quoted_cow<'a>(raw_quoted: &'a str) -> Cow<'a, str> {
    debug_assert!(
        raw_quoted.len() >= 2 && raw_quoted.starts_with('"') && raw_quoted.ends_with('"')
    );
    let inner = &raw_quoted[1..raw_quoted.len() - 1];
    if !inner.contains('\\') {
        Cow::Borrowed(inner)
    } else {
        Cow::Owned(crate::topology::unescape_quoted_string(raw_quoted))
    }
}

/// Re-export path parser helper for ergonomic canonical path construction.
pub fn canonical_path(raw: &str) -> Result<CanonicalNodePath> {
    parse_canonical_node_path(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrphanPolicy;

    const FULL_SPEC_EXAMPLE: &str = r###">:
  "project": "atlas-kv"
  "version": "1.0.0"
  > "StorageEngine"
  > "ArchiveEngine"
  > "WAL"
  > "Compactor"
  > <"drivers/os_io.sfgn">

"StorageEngine":
  "kind": "subsystem"
  "thread_safe": true
  "metrics_weights"#0: [0.4, 0.6]
  "metrics_weights"#1: [0.8, 0.2]
  > ["MemTable", "StorageEngine">"BufferPool", "FlushWorker"#0, "FlushWorker"#1]
  >> "WAL"
  >> <"drivers/os_io.sfgn">

"MemTable":
  "kind": "struct"
  "capacity_mb": 64
  "flush_policy": ["size_threshold", 67108864, true]
  "step": "Accept write batch"
  "step": "Insert into skiplist"
  "step": "Signal Compactor when full"
  "sample_dump": """csv
key,seq,op
user:101,8841,PUT
user:102,8842,DEL
"""
  > []
  >> ["WAL", "Compactor", <"drivers/os_io.sfgn">"IoUring">"RingBuffer"#0]

"StorageEngine">"BufferPool":
  "role": "active_read_cache"
  "pages": 4096
  > []
  >> []

"FlushWorker"#0:
  "thread_id": 0
  > []
  >> "WAL"

"FlushWorker"#1:
  "thread_id": 1
  > []
  >> "WAL"

"ArchiveEngine":
  "kind": "subsystem"
  > "ArchiveEngine">"BufferPool"
  >> []

"ArchiveEngine">"BufferPool":
  "role": "cold_compaction_cache"
  "pages": 512
  > []
  >> []

"WAL":
  "kind": "subsystem"
  "sync_mode": "fdatasync"
  > []
  >> <"drivers/os_io.sfgn">"DiskWriter"

"Compactor":
  "kind": "worker"
  "max_threads": 4
  > []
  >> "StorageEngine"
"###;

    #[test]
    fn progressive_retrieval_and_zero_copy_slicing() {
        let doc = Document::parse(FULL_SPEC_EXAMPLE).expect("spec example must parse");
        assert_eq!(doc.len(), 10);

        // 1. Horizon Bootstrapping at root `>:`
        let root = doc.root();
        assert!(root.is_root());
        assert_eq!(root.raw_header_path(), ">:");
        assert_eq!(
            root.property("project").unwrap().decode().unwrap(),
            Value::Scalar(Scalar::String(Cow::Borrowed("atlas-kv")))
        );

        let root_children: Vec<String> = root
            .children()
            .map(|n| n.canonical_path().unwrap().to_string())
            .collect();
        assert_eq!(
            root_children,
            vec![
                "\"StorageEngine\"",
                "\"ArchiveEngine\"",
                "\"WAL\"",
                "\"Compactor\""
            ]
        );

        let root_portals: Vec<ExternalPortal<'_>> = root.external_portals().collect();
        assert_eq!(root_portals.len(), 1);
        assert_eq!(root_portals[0].axis, RelationAxis::Child);
        assert_eq!(root_portals[0].file_path, "drivers/os_io.sfgn");
        assert!(root_portals[0].sub_path.is_none());

        // 2. O(1) Zero-Copy Node Slicing by path
        let raw_wal_slice = doc
            .node_slice_by_path("\"WAL\"")
            .unwrap()
            .expect("WAL exists");
        assert!(raw_wal_slice.starts_with("\"WAL\":\n"));
        assert!(raw_wal_slice.contains("\"sync_mode\": \"fdatasync\""));
        // Verify zero-copy pointer identity into FULL_SPEC_EXAMPLE
        let base_ptr = FULL_SPEC_EXAMPLE.as_ptr() as usize;
        let slice_ptr = raw_wal_slice.as_ptr() as usize;
        assert!(slice_ptr > base_ptr && slice_ptr < base_ptr + FULL_SPEC_EXAMPLE.len());
    }

    #[test]
    fn decodes_scalars_collections_repeated_keys_indexed_keys_and_multiline() {
        let doc = Document::parse(FULL_SPEC_EXAMPLE).unwrap();

        // Indexed property keys (`"metrics_weights"#0`, `"metrics_weights"#1`)
        let storage = doc.node_by_path("\"StorageEngine\"").unwrap().unwrap();
        let weight_props: Vec<PropertyView<'_>> =
            storage.properties_named("metrics_weights").collect();
        assert_eq!(weight_props.len(), 2);
        assert_eq!(weight_props[0].key_indices(), vec![0]);
        assert_eq!(weight_props[1].key_indices(), vec![1]);

        match weight_props[0].decode().unwrap() {
            Value::FlatCollection(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].as_number().unwrap().as_f64(), Some(0.4));
                assert_eq!(items[1].as_number().unwrap().as_f64(), Some(0.6));
            }
            other => panic!("expected FlatCollection, got {:?}", other),
        }

        // Heterogeneous 1D tuple, sequential repeated `"step"` keys, and `"""csv` multiline block
        let memtable = doc.node_by_path("\"MemTable\"").unwrap().unwrap();
        match memtable.property("flush_policy").unwrap().decode().unwrap() {
            Value::FlatCollection(tuple) => {
                assert_eq!(tuple[0].as_str(), Some("size_threshold"));
                assert_eq!(tuple[1].as_number().unwrap().as_i64(), Some(67108864));
                assert_eq!(tuple[2].as_bool(), Some(true));
            }
            other => panic!("expected heterogeneous tuple, got {:?}", other),
        }

        let steps: Vec<String> = memtable
            .properties_named("step")
            .map(|p| match p.decode().unwrap() {
                Value::Scalar(Scalar::String(s)) => s.into_owned(),
                other => panic!("expected string scalar, got {:?}", other),
            })
            .collect();
        assert_eq!(
            steps,
            vec![
                "Accept write batch",
                "Insert into skiplist",
                "Signal Compactor when full"
            ]
        );

        match memtable.property("sample_dump").unwrap().decode().unwrap() {
            Value::Multiline(payload) => {
                assert_eq!(payload.tag, Some("csv"));
                assert_eq!(
                    payload.decode_text(),
                    Cow::Borrowed("key,seq,op\nuser:101,8841,PUT\nuser:102,8842,DEL\n")
                );
            }
            other => panic!("expected Multiline, got {:?}", other),
        }
    }

    #[test]
    fn unescapes_strings_and_escaped_triple_quotes_lazily() {
        let src = r##">:
  "plain": "zero_copy"
  "escaped": "line1\n\t\u{1F680}"
  "nullable": null
  "code": """md
  \"""
  heading
"""
"##;
        let doc = Document::parse(src).unwrap();
        let root = doc.root();

        // Borrowed without allocation
        match root.property("plain").unwrap().decode().unwrap() {
            Value::Scalar(Scalar::String(cow)) => {
                assert!(matches!(cow, Cow::Borrowed("zero_copy")));
            }
            other => panic!("unexpected {:?}", other),
        }

        // Owned unescaped string
        match root.property("escaped").unwrap().decode().unwrap() {
            Value::Scalar(Scalar::String(cow)) => {
                assert_eq!(cow, "line1\n\t🚀");
            }
            other => panic!("unexpected {:?}", other),
        }

        assert_eq!(
            root.property("nullable").unwrap().decode().unwrap(),
            Value::Scalar(Scalar::Null)
        );

        match root.property("code").unwrap().decode().unwrap() {
            Value::Multiline(ml) => {
                assert_eq!(ml.tag, Some("md"));
                assert_eq!(ml.decode_text(), "  \"\"\"\n  heading\n");
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn neighborhood_union_and_parse_bytes_with_options() {
        let src = br#">:
  > "A"

"A":
  > "B"
  >> ["B", "C"]

"B":
  > []

"C":
  > []
"#;
        // Strict default rejects orphan "C" (only referenced via >>)
        assert_eq!(
            Document::parse_bytes(src).unwrap_err().code,
            ErrorCode::E017
        );

        // Opt-in AttachToRoot adopts "C" under `>:`
        let doc = Document::parse_bytes_with_options(
            src,
            ParseOptions {
                orphan_policy: OrphanPolicy::AttachToRoot,
            },
        )
        .unwrap();

        let node_a = doc.node_by_path("\"A\"").unwrap().unwrap();
        // Neighbors deduplicates "B" (present in both > and >>) and yields ["B", "C"]
        let neighbors: Vec<String> = node_a
            .neighbors()
            .map(|n| n.canonical_path().unwrap().to_string())
            .collect();
        assert_eq!(neighbors, vec!["\"B\"", "\"C\""]);
    }
}
