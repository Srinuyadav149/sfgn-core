pub mod builder;
pub mod document;
pub mod emit;
pub mod error;
pub mod lexer;
pub mod scanner;
pub mod tape;
pub mod topology;
pub mod viz;

pub use builder::{CompileError, EdgeTarget, NodeHandle, SfgnBuilder, SfgnScalar, SfgnValue};
pub use document::{
    Document, ExternalPortal, MultilinePayload, NodeView, Number, PropertyKey, PropertyView,
    RelationAxis, Scalar, Value, canonical_path, unescape_quoted_cow,
};
pub use emit::{
    GraphBuilder, IndentStyle, NodeBuilder, OwnedProperty, OwnedScalar, OwnedTarget, OwnedValue,
    serialize_document, serialize_document_with_indent,
};
pub use error::{Diagnostic, ErrorCode, LineCol, Result};
pub use scanner::{scan_tape, scan_tape_bytes};
pub use tape::{
    ByteRange, NodeEntry, NodeId, PropertyEntry, PropertyKind, RelationEntry, RelationKind, Span,
    StructuralTape, TapeRange,
};
pub use topology::{
    CanonicalNodePath, NodeSegmentKey, RelationTarget, TopologyIndex, build_topology,
    parse_canonical_node_path, unescape_quoted_string,
};
pub use viz::{render_ascii_tree, render_mermaid};

/// Policy governing nodes declared at Depth 0 that are not reachable from `>:` via `>` child edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrphanPolicy {
    /// Reject any unreachable node with `E017: OrphanNodeError` (Spec § 3.5 default).
    #[default]
    StrictError,
    /// Implicitly adopt unreachable nodes as direct children of `>:` (Spec § 3.5 opt-in).
    AttachToRoot,
}

/// Configuration options for parsing and validating an SFGN document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseOptions {
    pub orphan_policy: OrphanPolicy,
}
