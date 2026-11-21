use std::fmt::{self, Write};

use crate::document::{Document, MultilinePayload, NodeView, PropertyView, Scalar, Value};
use crate::error::{Diagnostic, ErrorCode, LineCol, Result};
use crate::tape::{NodeId, PropertyKind};
use crate::topology::{CanonicalNodePath, RelationTarget, parse_canonical_node_path};

/// Indentation style used when serializing an SFGN v1.0 document (Spec § 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    /// Single leading `\t` (`0x09`) per Depth 1 line.
    Tab,
    /// `N` (`N >= 2`) leading `' '` (`0x20`) bytes per Depth 1 line.
    Spaces(usize),
}

impl Default for IndentStyle {
    fn default() -> Self {
        Self::Spaces(2)
    }
}

impl IndentStyle {
    fn validate(self) -> Result<()> {
        if let Self::Spaces(n) = self {
            if n < 2 {
                return Err(Diagnostic::new(
                    ErrorCode::E004,
                    LineCol::start(),
                    0,
                    "space indentation requires at least 2 spaces (N >= 2)",
                ));
            }
        }
        Ok(())
    }

    fn write_indent(self, out: &mut String) {
        match self {
            Self::Tab => out.push('\t'),
            Self::Spaces(n) => {
                for _ in 0..n {
                    out.push(' ');
                }
            }
        }
    }
}

/// Serializes an existing parsed `Document` into a canonical SFGN v1.0 UTF-8 string
/// using the default 2-space Depth 1 indentation.
pub fn serialize_document(doc: &Document<'_>) -> Result<String> {
    serialize_document_with_indent(doc, IndentStyle::default())
}

/// Serializes an existing parsed `Document` into a canonical SFGN v1.0 UTF-8 string
/// with an explicit `IndentStyle`.
pub fn serialize_document_with_indent(doc: &Document<'_>, indent: IndentStyle) -> Result<String> {
    indent.validate()?;
    let mut out = String::with_capacity(doc.source().len() + 32);

    for (idx, node) in doc.nodes().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        emit_node_view(&mut out, doc, node, indent)?;
    }

    Ok(out)
}

fn emit_node_view(
    out: &mut String,
    doc: &Document<'_>,
    node: NodeView<'_, '_>,
    indent: IndentStyle,
) -> Result<()> {
    if node.is_root() {
        out.push_str(">:\n");
    } else if let Some(canonical) = node.canonical_path() {
        writeln!(out, "{}:", canonical).map_err(fmt_err)?;
    } else {
        writeln!(out, "{}:", node.raw_header_path()).map_err(fmt_err)?;
    }

    for prop in node.properties() {
        emit_property_view(out, prop, indent)?;
    }

    let child_targets = node.child_targets();
    let adopted_orphans: &[NodeId] = if node.is_root() {
        doc.topology().adopted_orphans()
    } else {
        &[]
    };

    let total_children = child_targets.len() + adopted_orphans.len();
    if total_children > 0 {
        indent.write_indent(out);
        out.push_str("> ");
        if total_children == 1 {
            if let Some(first) = child_targets.first() {
                emit_single_relation_target(out, first)?;
            } else if let Some(&orphan_id) = adopted_orphans.first() {
                let path = doc
                    .topology()
                    .canonical_path(orphan_id)
                    .expect("adopted orphan is non-root");
                write!(out, "{}", path).map_err(fmt_err)?;
            }
        } else {
            out.push('[');
            let mut first = true;
            for target in child_targets {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                emit_single_relation_target(out, target)?;
            }
            for &orphan_id in adopted_orphans {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                let path = doc
                    .topology()
                    .canonical_path(orphan_id)
                    .expect("adopted orphan is non-root");
                write!(out, "{}", path).map_err(fmt_err)?;
            }
            out.push(']');
        }
        out.push('\n');
    }

    let relative_targets = node.relative_targets();
    if !relative_targets.is_empty() {
        indent.write_indent(out);
        out.push_str(">> ");
        emit_relation_target_list(out, relative_targets)?;
        out.push('\n');
    }

    Ok(())
}

fn emit_property_view(out: &mut String, prop: PropertyView<'_>, indent: IndentStyle) -> Result<()> {
    let parsed_key = prop.parsed_key();
    indent.write_indent(out);
    write_escaped_quoted(out, parsed_key.name.as_ref()).map_err(fmt_err)?;
    for idx in &parsed_key.indices {
        write!(out, "#{}", idx).map_err(fmt_err)?;
    }
    out.push_str(": ");

    match prop.kind() {
        PropertyKind::Scalar => match prop.decode()? {
            Value::Scalar(scalar) => {
                emit_scalar(out, &scalar)?;
                out.push('\n');
            }
            _ => unreachable!(),
        },
        PropertyKind::FlatCollection => match prop.decode()? {
            Value::FlatCollection(items) => {
                emit_flat_collection(out, &items)?;
                out.push('\n');
            }
            _ => unreachable!(),
        },
        PropertyKind::MultilineBlock => match prop.decode()? {
            Value::Multiline(payload) => {
                emit_multiline(out, payload, indent)?;
            }
            _ => unreachable!(),
        },
    }

    Ok(())
}

fn emit_relation_target_list(out: &mut String, targets: &[RelationTarget]) -> Result<()> {
    if targets.len() == 1 {
        emit_single_relation_target(out, &targets[0])?;
    } else {
        out.push('[');
        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_single_relation_target(out, target)?;
        }
        out.push(']');
    }
    Ok(())
}

fn emit_single_relation_target(out: &mut String, target: &RelationTarget) -> Result<()> {
    match target {
        RelationTarget::Local { path, .. } => {
            write!(out, "{}", path).map_err(fmt_err)?;
        }
        RelationTarget::External {
            file_path,
            sub_path,
            ..
        } => {
            out.push('<');
            write_escaped_quoted(out, file_path).map_err(fmt_err)?;
            out.push('>');
            if let Some(sub) = sub_path {
                write!(out, "{}", sub).map_err(fmt_err)?;
            }
        }
    }
    Ok(())
}

fn emit_scalar(out: &mut String, scalar: &Scalar<'_>) -> Result<()> {
    match scalar {
        Scalar::String(s) => write_escaped_quoted(out, s.as_ref()).map_err(fmt_err),
        Scalar::Number(n) => {
            out.push_str(n.raw());
            Ok(())
        }
        Scalar::Bool(true) => {
            out.push_str("true");
            Ok(())
        }
        Scalar::Bool(false) => {
            out.push_str("false");
            Ok(())
        }
        Scalar::Null => {
            out.push_str("null");
            Ok(())
        }
    }
}

fn emit_flat_collection(out: &mut String, items: &[Scalar<'_>]) -> Result<()> {
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        emit_scalar(out, item)?;
    }
    out.push(']');
    Ok(())
}

fn emit_multiline(
    out: &mut String,
    payload: MultilinePayload<'_>,
    indent: IndentStyle,
) -> Result<()> {
    out.push_str("\"\"\"");
    if let Some(tag) = payload.tag {
        out.push_str(tag);
    }
    out.push('\n');

    let decoded = payload.decode_text();
    if !decoded.is_empty() {
        for line in decoded.split_inclusive('\n') {
            let ws_len = line.len() - line.trim_start_matches([' ', '\t']).len();
            let rest = &line[ws_len..];
            if rest.starts_with("\"\"\"") {
                out.push_str(&line[..ws_len]);
                out.push('\\');
                out.push_str(rest);
            } else {
                out.push_str(line);
            }
        }
        if !decoded.ends_with('\n') {
            out.push('\n');
        }
    }

    indent.write_indent(out);
    out.push_str("\"\"\"\n");
    Ok(())
}

/// Owned value representation for programmatic SFGN graph construction and serialization.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedValue {
    String(String),
    Number(String),
    Bool(bool),
    Null,
    FlatCollection(Vec<OwnedScalar>),
    Multiline { tag: Option<String>, text: String },
}

/// Owned atomic scalar for programmatic 1D `[...]` property collections.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedScalar {
    String(String),
    Number(String),
    Bool(bool),
    Null,
}

/// Owned relation target (`Local` node path or `External` `<"file.sfgn">` subgraph portal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedTarget {
    Local(CanonicalNodePath),
    External {
        file_path: String,
        sub_path: Option<CanonicalNodePath>,
    },
}

impl OwnedTarget {
    pub fn local(raw_node_path: &str) -> Result<Self> {
        Ok(Self::Local(parse_canonical_node_path(raw_node_path)?))
    }

    pub fn external(file_path: impl Into<String>, raw_sub_path: Option<&str>) -> Result<Self> {
        let sub_path = match raw_sub_path {
            Some(p) => Some(parse_canonical_node_path(p)?),
            None => None,
        };
        Ok(Self::External {
            file_path: file_path.into(),
            sub_path,
        })
    }
}

/// Owned property entry for programmatic SFGN graph construction.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedProperty {
    pub key: String,
    pub indices: Vec<usize>,
    pub value: OwnedValue,
}

/// Builder for a single Depth 0 SFGN node (`>:` root or `NodePath:`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeBuilder {
    path: Option<CanonicalNodePath>,
    properties: Vec<OwnedProperty>,
    children: Vec<OwnedTarget>,
    relatives: Vec<OwnedTarget>,
}

impl NodeBuilder {
    /// Creates the mandatory root node `>:`.
    pub fn root() -> Self {
        Self::default()
    }

    /// Creates a child vertex builder from an SFGN `NodePath` string (e.g., `"StorageEngine">"BufferPool"`).
    pub fn new(raw_node_path: &str) -> Result<Self> {
        Ok(Self {
            path: Some(parse_canonical_node_path(raw_node_path)?),
            ..Self::default()
        })
    }

    pub fn prop_str(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices: Vec::new(),
            value: OwnedValue::String(val.into()),
        });
        self
    }

    pub fn prop_number(mut self, key: impl Into<String>, raw_num: impl Into<String>) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices: Vec::new(),
            value: OwnedValue::Number(raw_num.into()),
        });
        self
    }

    pub fn prop_bool(mut self, key: impl Into<String>, val: bool) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices: Vec::new(),
            value: OwnedValue::Bool(val),
        });
        self
    }

    pub fn prop_null(mut self, key: impl Into<String>) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices: Vec::new(),
            value: OwnedValue::Null,
        });
        self
    }

    pub fn prop_collection(mut self, key: impl Into<String>, items: Vec<OwnedScalar>) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices: Vec::new(),
            value: OwnedValue::FlatCollection(items),
        });
        self
    }

    pub fn prop_indexed(
        mut self,
        key: impl Into<String>,
        indices: Vec<usize>,
        value: OwnedValue,
    ) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices,
            value,
        });
        self
    }

    pub fn prop_multiline(
        mut self,
        key: impl Into<String>,
        tag: Option<impl Into<String>>,
        text: impl Into<String>,
    ) -> Self {
        self.properties.push(OwnedProperty {
            key: key.into(),
            indices: Vec::new(),
            value: OwnedValue::Multiline {
                tag: tag.map(Into::into),
                text: text.into(),
            },
        });
        self
    }

    /// Appends a hierarchical `>` child edge.
    pub fn child(mut self, target: OwnedTarget) -> Self {
        self.children.push(target);
        self
    }

    /// Appends a non-hierarchical `>>` relative edge.
    pub fn relative(mut self, target: OwnedTarget) -> Self {
        self.relatives.push(target);
        self
    }
}

/// Programmatic builder for constructing and serializing valid SFGN v1.0 documents.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphBuilder {
    root: NodeBuilder,
    nodes: Vec<NodeBuilder>,
    indent: IndentStyle,
}

impl GraphBuilder {
    pub fn new(root: NodeBuilder) -> Self {
        Self {
            root,
            nodes: Vec::new(),
            indent: IndentStyle::default(),
        }
    }

    pub fn with_indent(mut self, indent: IndentStyle) -> Self {
        self.indent = indent;
        self
    }

    pub fn add_node(mut self, node: NodeBuilder) -> Self {
        self.nodes.push(node);
        self
    }

    /// Serializes the constructed graph into canonical SFGN v1.0 UTF-8 text.
    pub fn serialize(&self) -> Result<String> {
        self.indent.validate()?;
        let mut out = String::new();
        emit_node_builder(&mut out, &self.root, self.indent)?;
        for node in &self.nodes {
            out.push('\n');
            emit_node_builder(&mut out, node, self.indent)?;
        }
        Ok(out)
    }

    /// Serializes the constructed graph into UTF-8 bytes ready for filesystem persistence.
    pub fn serialize_bytes(&self) -> Result<Vec<u8>> {
        Ok(self.serialize()?.into_bytes())
    }
}

fn emit_node_builder(out: &mut String, node: &NodeBuilder, indent: IndentStyle) -> Result<()> {
    match &node.path {
        None => out.push_str(">:\n"),
        Some(path) => writeln!(out, "{}:", path).map_err(fmt_err)?,
    }

    for prop in &node.properties {
        indent.write_indent(out);
        write_escaped_quoted(out, &prop.key).map_err(fmt_err)?;
        for idx in &prop.indices {
            write!(out, "#{}", idx).map_err(fmt_err)?;
        }
        out.push_str(": ");

        match &prop.value {
            OwnedValue::String(s) => {
                write_escaped_quoted(out, s).map_err(fmt_err)?;
                out.push('\n');
            }
            OwnedValue::Number(raw) => {
                out.push_str(raw);
                out.push('\n');
            }
            OwnedValue::Bool(b) => {
                out.push_str(if *b { "true\n" } else { "false\n" });
            }
            OwnedValue::Null => {
                out.push_str("null\n");
            }
            OwnedValue::FlatCollection(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match item {
                        OwnedScalar::String(s) => write_escaped_quoted(out, s).map_err(fmt_err)?,
                        OwnedScalar::Number(raw) => out.push_str(raw),
                        OwnedScalar::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                        OwnedScalar::Null => out.push_str("null"),
                    }
                }
                out.push_str("]\n");
            }
            OwnedValue::Multiline { tag, text } => {
                emit_multiline(
                    out,
                    MultilinePayload {
                        tag: tag.as_deref(),
                        raw: text.as_str(),
                    },
                    indent,
                )?;
            }
        }
    }

    if !node.children.is_empty() {
        indent.write_indent(out);
        out.push_str("> ");
        emit_owned_targets(out, &node.children)?;
        out.push('\n');
    }

    if !node.relatives.is_empty() {
        indent.write_indent(out);
        out.push_str(">> ");
        emit_owned_targets(out, &node.relatives)?;
        out.push('\n');
    }

    Ok(())
}

fn emit_owned_targets(out: &mut String, targets: &[OwnedTarget]) -> Result<()> {
    if targets.len() == 1 {
        emit_single_owned_target(out, &targets[0])?;
    } else {
        out.push('[');
        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_single_owned_target(out, target)?;
        }
        out.push(']');
    }
    Ok(())
}

fn emit_single_owned_target(out: &mut String, target: &OwnedTarget) -> Result<()> {
    match target {
        OwnedTarget::Local(path) => write!(out, "{}", path).map_err(fmt_err),
        OwnedTarget::External {
            file_path,
            sub_path,
        } => {
            out.push('<');
            write_escaped_quoted(out, file_path).map_err(fmt_err)?;
            out.push('>');
            if let Some(sub) = sub_path {
                write!(out, "{}", sub).map_err(fmt_err)?;
            }
            Ok(())
        }
    }
}

fn write_escaped_quoted(out: &mut String, s: &str) -> fmt::Result {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) <= 0x1F || (c as u32) == 0x7F => {
                write!(out, "\\u{{{:X}}}", c as u32)?;
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(())
}

fn fmt_err(err: fmt::Error) -> Diagnostic {
    Diagnostic::new(
        ErrorCode::E009,
        LineCol::start(),
        0,
        format!("formatting failure during SFGN serialization: {}", err),
    )
}
