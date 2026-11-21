use std::ops::Range;

use crate::error::LineCol;

/// Typed index into `StructuralTape::nodes`. Index `0` is always the mandatory file root `>:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

impl NodeId {
    pub const ROOT: Self = Self(0);

    #[inline]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// Offset-and-length span `(start, len)` into the source UTF-8 buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub len: usize,
}

impl Span {
    #[inline]
    pub const fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }

    #[inline]
    pub const fn end(self) -> usize {
        self.start + self.len
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    #[inline]
    pub const fn as_range(self) -> Range<usize> {
        self.start..self.end()
    }

    #[inline]
    pub fn slice_bytes<'a>(self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.as_range()]
    }

    #[inline]
    pub fn slice_str<'a>(self, src: &'a str) -> &'a str {
        &src[self.as_range()]
    }
}

/// Half-open byte range `[start_byte..end_byte)` representing a node's full contiguous encapsulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl ByteRange {
    #[inline]
    pub const fn new(start_byte: usize, end_byte: usize) -> Self {
        Self {
            start_byte,
            end_byte,
        }
    }

    #[inline]
    pub const fn len(self) -> usize {
        self.end_byte - self.start_byte
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start_byte == self.end_byte
    }

    #[inline]
    pub const fn as_range(self) -> Range<usize> {
        self.start_byte..self.end_byte
    }

    #[inline]
    pub fn slice_str<'a>(self, src: &'a str) -> &'a str {
        &src[self.as_range()]
    }
}

/// Index range `(start_idx, count)` into `StructuralTape::properties` or `StructuralTape::relations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TapeRange {
    pub start_idx: usize,
    pub count: usize,
}

impl TapeRange {
    #[inline]
    pub const fn new(start_idx: usize, count: usize) -> Self {
        Self { start_idx, count }
    }

    #[inline]
    pub const fn as_range(self) -> Range<usize> {
        self.start_idx..(self.start_idx + self.count)
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.count == 0
    }
}

/// Pass 1 Node Tape entry (Spec § 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeEntry {
    /// Span of the node header path (`>:` for Node 0, or `NodePath` excluding trailing `:`).
    pub path_span: Span,
    /// Full byte span from column 0 of the node header to the byte before the next node header or EOF.
    pub full_span: ByteRange,
    /// Contiguous range in `StructuralTape::properties` owned by this node.
    pub prop_range: TapeRange,
    /// Contiguous range in `StructuralTape::relations` owned by this node.
    pub rel_range: TapeRange,
    /// Source location of the node header for diagnostics.
    pub header_loc: LineCol,
}

/// Discriminant for property values recorded on the Property Tape (Spec § 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyKind {
    /// Atomic scalar: quoted string, number, boolean, or null.
    Scalar,
    /// Single-line 1D `[...]` collection of atomic scalars.
    FlatCollection,
    /// Opaque `"""..."""` multiline block payload.
    MultilineBlock,
}

/// Pass 1 Property Tape entry (Spec § 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyEntry {
    /// Span of the quoted property key including any `#index` chain.
    pub key_span: Span,
    /// Span of the raw value payload (scalar, `[...]` slice, or interior `"""` payload).
    pub val_span: Span,
    /// Optional span of the format tag on a `"""tag` header (e.g., `rust`, `csv`).
    pub tag_span: Option<Span>,
    /// Syntactic value category.
    pub kind: PropertyKind,
    /// Source location of the property key for diagnostics.
    pub loc: LineCol,
}

/// Discriminant for relation lines recorded on the Relation Tape (Spec § 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationKind {
    /// Hierarchical single child (`> Target`).
    ChildSingle,
    /// Hierarchical child array (`> [Target, ...]`).
    ChildArray,
    /// Non-hierarchical single relative (`>> Target`).
    RelativeSingle,
    /// Non-hierarchical relative array (`>> [Target, ...]`).
    RelativeArray,
}

impl RelationKind {
    #[inline]
    pub const fn is_child(self) -> bool {
        matches!(self, Self::ChildSingle | Self::ChildArray)
    }

    #[inline]
    pub const fn is_relative(self) -> bool {
        matches!(self, Self::RelativeSingle | Self::RelativeArray)
    }

    #[inline]
    pub const fn is_array(self) -> bool {
        matches!(self, Self::ChildArray | Self::RelativeArray)
    }
}

/// Pass 1 Relation Tape entry (Spec § 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationEntry {
    /// Relation axis and single vs. array form.
    pub kind: RelationKind,
    /// Span pointing to the raw `NodeRef` or `[...]` slice.
    pub target_span: Span,
    /// Source location of the relation operator for diagnostics.
    pub loc: LineCol,
}

/// Zero-allocation structural tape emitted by the Pass 1 linear scanner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructuralTape {
    pub nodes: Vec<NodeEntry>,
    pub properties: Vec<PropertyEntry>,
    pub relations: Vec<RelationEntry>,
}

impl StructuralTape {
    pub fn with_capacity(nodes: usize, properties: usize, relations: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(nodes),
            properties: Vec::with_capacity(properties),
            relations: Vec::with_capacity(relations),
        }
    }

    #[inline]
    pub fn root(&self) -> Option<&NodeEntry> {
        self.nodes.first()
    }

    #[inline]
    pub fn node(&self, id: NodeId) -> Option<&NodeEntry> {
        self.nodes.get(id.0)
    }

    #[inline]
    pub fn node_properties(&self, node: &NodeEntry) -> &[PropertyEntry] {
        &self.properties[node.prop_range.as_range()]
    }

    #[inline]
    pub fn node_relations(&self, node: &NodeEntry) -> &[RelationEntry] {
        &self.relations[node.rel_range.as_range()]
    }
}
