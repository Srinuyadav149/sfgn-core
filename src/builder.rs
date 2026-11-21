use std::collections::HashMap;
use std::fmt::{self, Write};

use crate::document::Document;
use crate::emit::IndentStyle;
use crate::error::{Diagnostic, ErrorCode, LineCol, Result};
use crate::lexer::{Lexer, is_tag_char, is_tag_start};
use crate::topology::{CanonicalNodePath, NodeSegmentKey, parse_canonical_node_path};

/// Compilation error type returned by `SfgnBuilder::build`.
pub type CompileError = Diagnostic;

/// Opaque handle referencing a vertex registered inside an `SfgnBuilder` arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeHandle(pub usize);

impl NodeHandle {
    /// Handle representing the mandatory file root (`>:`).
    pub const ROOT: Self = Self(0);

    #[inline]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// Atomic scalar value used inside 1D `[...]` property collections in `SfgnBuilder`.
#[derive(Debug, Clone, PartialEq)]
pub enum SfgnScalar {
    String(String),
    Number(String),
    Bool(bool),
    Null,
}

impl SfgnScalar {
    pub fn string(val: impl Into<String>) -> Self {
        Self::String(val.into())
    }

    pub fn int(val: i64) -> Self {
        Self::Number(val.to_string())
    }

    pub fn uint(val: u64) -> Self {
        Self::Number(val.to_string())
    }

    pub fn float(val: f64) -> Self {
        Self::Number(val.to_string())
    }

    pub fn number(raw: impl Into<String>) -> Self {
        Self::Number(raw.into())
    }

    pub fn bool(val: bool) -> Self {
        Self::Bool(val)
    }

    pub fn null() -> Self {
        Self::Null
    }
}

impl From<&str> for SfgnScalar {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<String> for SfgnScalar {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<i64> for SfgnScalar {
    fn from(v: i64) -> Self {
        Self::int(v)
    }
}

impl From<i32> for SfgnScalar {
    fn from(v: i32) -> Self {
        Self::int(i64::from(v))
    }
}

impl From<usize> for SfgnScalar {
    fn from(v: usize) -> Self {
        Self::uint(v as u64)
    }
}

impl From<u64> for SfgnScalar {
    fn from(v: u64) -> Self {
        Self::uint(v)
    }
}

impl From<f64> for SfgnScalar {
    fn from(v: f64) -> Self {
        Self::float(v)
    }
}

impl From<bool> for SfgnScalar {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

/// Typed inline property value accepted by `SfgnBuilder::set_root_prop` and `SfgnBuilder::add_prop`.
#[derive(Debug, Clone, PartialEq)]
pub enum SfgnValue {
    String(String),
    Number(String),
    Bool(bool),
    Null,
    Collection(Vec<SfgnScalar>),
}

impl SfgnValue {
    pub fn string(val: impl Into<String>) -> Self {
        Self::String(val.into())
    }

    pub fn int(val: i64) -> Self {
        Self::Number(val.to_string())
    }

    pub fn uint(val: u64) -> Self {
        Self::Number(val.to_string())
    }

    pub fn float(val: f64) -> Self {
        Self::Number(val.to_string())
    }

    pub fn number(raw: impl Into<String>) -> Self {
        Self::Number(raw.into())
    }

    pub fn bool(val: bool) -> Self {
        Self::Bool(val)
    }

    pub fn null() -> Self {
        Self::Null
    }

    pub fn collection<I, T>(items: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<SfgnScalar>,
    {
        Self::Collection(items.into_iter().map(Into::into).collect())
    }
}

impl From<&str> for SfgnValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<String> for SfgnValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<i64> for SfgnValue {
    fn from(v: i64) -> Self {
        Self::int(v)
    }
}

impl From<i32> for SfgnValue {
    fn from(v: i32) -> Self {
        Self::int(i64::from(v))
    }
}

impl From<usize> for SfgnValue {
    fn from(v: usize) -> Self {
        Self::uint(v as u64)
    }
}

impl From<u64> for SfgnValue {
    fn from(v: u64) -> Self {
        Self::uint(v)
    }
}

impl From<f64> for SfgnValue {
    fn from(v: f64) -> Self {
        Self::float(v)
    }
}

impl From<bool> for SfgnValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl<T: Into<SfgnScalar>> From<Vec<T>> for SfgnValue {
    fn from(items: Vec<T>) -> Self {
        Self::collection(items)
    }
}

/// Target of a relation edge in `SfgnBuilder` (`Node` local handle or `External` `<"file.sfgn">` portal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeTarget {
    /// Local vertex registered in the same `SfgnBuilder` arena.
    Node(NodeHandle),
    /// External subgraph portal `<"file.sfgn">[sub_path]`.
    External {
        file_path: String,
        sub_path: Option<String>,
    },
}

impl EdgeTarget {
    #[inline]
    pub const fn node(handle: NodeHandle) -> Self {
        Self::Node(handle)
    }

    pub fn external_root(file_path: impl Into<String>) -> Self {
        Self::External {
            file_path: file_path.into(),
            sub_path: None,
        }
    }

    pub fn external_node(file_path: impl Into<String>, sub_path: impl Into<String>) -> Self {
        Self::External {
            file_path: file_path.into(),
            sub_path: Some(sub_path.into()),
        }
    }
}

impl From<NodeHandle> for EdgeTarget {
    fn from(handle: NodeHandle) -> Self {
        Self::Node(handle)
    }
}

#[derive(Debug, Clone)]
enum BuilderPropValue {
    Inline(SfgnValue),
    Multiline { tag: Option<String>, body: String },
}

#[derive(Debug, Clone)]
struct BuilderProperty {
    key: String,
    indices: Vec<usize>,
    value: BuilderPropValue,
}

#[derive(Debug, Clone)]
struct ArenaNode {
    parent: Option<NodeHandle>,
    local_name: String,
    properties: Vec<BuilderProperty>,
    /// Children along the `>` containment axis (`NodeHandle` or external root/subgraph portal).
    children: Vec<EdgeTarget>,
    /// Non-hierarchical relatives along the `>>` associative axis.
    relatives: Vec<EdgeTarget>,
}

/// Handle-based graph compiler backend guaranteeing zero orphans (`E017`) and automatic
/// minimal collision qualification across hierarchical branches and sibling sets.
#[derive(Debug, Clone)]
pub struct SfgnBuilder {
    arena: Vec<ArenaNode>,
    indent: IndentStyle,
}

impl Default for SfgnBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SfgnBuilder {
    /// Creates a new `SfgnBuilder` initialized with the mandatory root vertex `>:` (`NodeHandle::ROOT`).
    pub fn new() -> Self {
        Self {
            arena: vec![ArenaNode {
                parent: None,
                local_name: String::new(),
                properties: Vec::new(),
                children: Vec::new(),
                relatives: Vec::new(),
            }],
            indent: IndentStyle::default(),
        }
    }

    /// Configures the Depth 1 indentation style (`Spaces(N)` with `N >= 2`, or `Tab`).
    pub fn with_indent(mut self, indent: IndentStyle) -> Self {
        self.indent = indent;
        self
    }

    /// Returns the total number of registered vertices (including `NodeHandle::ROOT`).
    #[inline]
    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Always returns `false` because `NodeHandle::ROOT` is always present.
    #[inline]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Attaches a property directly onto the file root `>:`.
    pub fn set_root_prop(&mut self, key: &str, val: impl Into<SfgnValue>) {
        self.add_prop(NodeHandle::ROOT, key, val);
    }

    /// Registers a new child vertex under `parent` via a `>` containment edge,
    /// guaranteeing reachability from `>:` by construction.
    pub fn add_node(&mut self, parent: NodeHandle, local_name: &str) -> NodeHandle {
        assert!(
            parent.0 < self.arena.len(),
            "parent NodeHandle({}) out of bounds (len = {})",
            parent.0,
            self.arena.len()
        );
        let handle = NodeHandle(self.arena.len());
        self.arena.push(ArenaNode {
            parent: Some(parent),
            local_name: local_name.to_owned(),
            properties: Vec::new(),
            children: Vec::new(),
            relatives: Vec::new(),
        });
        self.arena[parent.0].children.push(EdgeTarget::Node(handle));
        handle
    }

    /// Attaches an external `<"file.sfgn">[sub_path]` portal as a `>` child edge on `parent`.
    pub fn add_external_child(
        &mut self,
        parent: NodeHandle,
        file_path: impl Into<String>,
        sub_path: Option<impl Into<String>>,
    ) {
        assert!(
            parent.0 < self.arena.len(),
            "parent NodeHandle({}) out of bounds (len = {})",
            parent.0,
            self.arena.len()
        );
        self.arena[parent.0].children.push(EdgeTarget::External {
            file_path: file_path.into(),
            sub_path: sub_path.map(Into::into),
        });
    }

    /// Appends a scalar or 1D `[...]` collection property to `node`.
    pub fn add_prop(&mut self, node: NodeHandle, key: &str, val: impl Into<SfgnValue>) {
        assert!(
            node.0 < self.arena.len(),
            "node NodeHandle({}) out of bounds (len = {})",
            node.0,
            self.arena.len()
        );
        self.arena[node.0].properties.push(BuilderProperty {
            key: key.to_owned(),
            indices: Vec::new(),
            value: BuilderPropValue::Inline(val.into()),
        });
    }

    /// Appends an ordinal-indexed property (`"key"#i#j: val`) to `node`.
    pub fn add_indexed_prop(
        &mut self,
        node: NodeHandle,
        key: &str,
        indices: impl Into<Vec<usize>>,
        val: impl Into<SfgnValue>,
    ) {
        assert!(
            node.0 < self.arena.len(),
            "node NodeHandle({}) out of bounds (len = {})",
            node.0,
            self.arena.len()
        );
        self.arena[node.0].properties.push(BuilderProperty {
            key: key.to_owned(),
            indices: indices.into(),
            value: BuilderPropValue::Inline(val.into()),
        });
    }

    /// Appends an opaque `"""[tag]..."""` multiline block property to `node`.
    pub fn add_multiline(&mut self, node: NodeHandle, key: &str, tag: Option<&str>, body: &str) {
        assert!(
            node.0 < self.arena.len(),
            "node NodeHandle({}) out of bounds (len = {})",
            node.0,
            self.arena.len()
        );
        self.arena[node.0].properties.push(BuilderProperty {
            key: key.to_owned(),
            indices: Vec::new(),
            value: BuilderPropValue::Multiline {
                tag: tag.map(ToOwned::to_owned),
                body: body.to_owned(),
            },
        });
    }

    /// Appends a non-hierarchical `>>` relative edge from `from` to `target`
    /// (local `NodeHandle` or external `<"file.sfgn">` portal).
    pub fn add_relative(&mut self, from: NodeHandle, target: impl Into<EdgeTarget>) {
        assert!(
            from.0 < self.arena.len(),
            "source NodeHandle({}) out of bounds (len = {})",
            from.0,
            self.arena.len()
        );
        self.arena[from.0].relatives.push(target.into());
    }

    /// Resolves minimal collision qualification across all registered vertices,
    /// validates the emitted document through Pass 1 and Pass 2, and returns canonical SFGN.
    pub fn build(self) -> std::result::Result<String, CompileError> {
        self.validate_indent()?;
        let resolved_paths = self.resolve_minimal_paths();

        let mut out = String::with_capacity(self.arena.len() * 96);
        for idx in 0..self.arena.len() {
            if idx > 0 {
                out.push('\n');
            }
            self.emit_node(&mut out, idx, &resolved_paths)?;
        }

        // Validation gate: verify emitted UTF-8 passes Pass 1 scanner and Pass 2 topology (E017).
        let _verified = Document::parse(&out)?;
        Ok(out)
    }

    fn validate_indent(&self) -> Result<()> {
        if let IndentStyle::Spaces(n) = self.indent {
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

    fn write_indent(&self, out: &mut String) {
        match self.indent {
            IndentStyle::Tab => out.push('\t'),
            IndentStyle::Spaces(n) => {
                for _ in 0..n {
                    out.push(' ');
                }
            }
        }
    }

    /// Two-pass Minimal Collision Qualification (Spec § 3.2, Doc 1 § 2.2):
    /// 1. Sibling disambiguation (`#0`, `#1`) when identical names share the exact same parent.
    /// 2. Shortest unique ancestor prefix (`"Parent">"Child"`) when names collide across branches.
    fn resolve_minimal_paths(&self) -> Vec<Option<CanonicalNodePath>> {
        let count = self.arena.len();
        let mut sibling_indices: Vec<Option<usize>> = vec![None; count];

        // Pass A: Assign zero-based `#index` only to colliding siblings under the same parent.
        for parent_node in &self.arena {
            let mut local_children_by_name: HashMap<&str, Vec<usize>> = HashMap::new();
            for target in &parent_node.children {
                if let EdgeTarget::Node(child_handle) = target {
                    let child_idx = child_handle.0;
                    local_children_by_name
                        .entry(self.arena[child_idx].local_name.as_str())
                        .or_default()
                        .push(child_idx);
                }
            }

            for (_name, indices) in local_children_by_name {
                if indices.len() > 1 {
                    for (ordinal, &child_idx) in indices.iter().enumerate() {
                        sibling_indices[child_idx] = Some(ordinal);
                    }
                }
            }
        }

        let base_segment = |idx: usize| -> NodeSegmentKey {
            NodeSegmentKey {
                name: self.arena[idx].local_name.clone(),
                indices: sibling_indices[idx].into_iter().collect(),
            }
        };

        // Pass B: Start each non-root node with `[self_segment]` and expand upward along `>`
        // parent edges strictly for nodes whose candidate path collides with another node.
        let mut reversed_chains: Vec<Vec<NodeSegmentKey>> = vec![Vec::new(); count];
        let mut top_ancestor: Vec<usize> = (0..count).collect();

        for idx in 1..count {
            reversed_chains[idx].push(base_segment(idx));
        }

        loop {
            let mut path_counts: HashMap<CanonicalNodePath, usize> =
                HashMap::with_capacity(count.saturating_sub(1));
            let mut candidates: Vec<Option<CanonicalNodePath>> = vec![None; count];

            for idx in 1..count {
                let mut segs = reversed_chains[idx].clone();
                segs.reverse();
                let canonical = CanonicalNodePath(segs);
                *path_counts.entry(canonical.clone()).or_insert(0) += 1;
                candidates[idx] = Some(canonical);
            }

            let mut expanded_any = false;
            for idx in 1..count {
                let is_colliding = candidates[idx]
                    .as_ref()
                    .and_then(|c| path_counts.get(c))
                    .copied()
                    .unwrap_or(0)
                    > 1;

                if is_colliding {
                    let curr_top = top_ancestor[idx];
                    if let Some(parent_handle) = self.arena[curr_top].parent {
                        if parent_handle != NodeHandle::ROOT {
                            let p_idx = parent_handle.0;
                            reversed_chains[idx].push(base_segment(p_idx));
                            top_ancestor[idx] = p_idx;
                            expanded_any = true;
                        }
                    }
                }
            }

            if !expanded_any {
                return candidates;
            }
        }
    }

    fn emit_node(
        &self,
        out: &mut String,
        idx: usize,
        resolved_paths: &[Option<CanonicalNodePath>],
    ) -> Result<()> {
        let node = &self.arena[idx];
        if idx == 0 {
            out.push_str(">:\n");
        } else {
            let canonical = resolved_paths[idx]
                .as_ref()
                .expect("non-root node has resolved canonical path");
            writeln!(out, "{}:", canonical).map_err(fmt_err)?;
        }

        for prop in &node.properties {
            self.emit_property(out, prop)?;
        }

        if !node.children.is_empty() {
            self.write_indent(out);
            out.push_str("> ");
            self.emit_edge_targets(out, &node.children, resolved_paths)?;
            out.push('\n');
        }

        if !node.relatives.is_empty() {
            self.write_indent(out);
            out.push_str(">> ");
            self.emit_edge_targets(out, &node.relatives, resolved_paths)?;
            out.push('\n');
        }

        Ok(())
    }

    fn emit_property(&self, out: &mut String, prop: &BuilderProperty) -> Result<()> {
        self.write_indent(out);
        write_escaped_quoted(out, &prop.key).map_err(fmt_err)?;
        for idx in &prop.indices {
            write!(out, "#{}", idx).map_err(fmt_err)?;
        }
        out.push_str(": ");

        match &prop.value {
            BuilderPropValue::Inline(val) => {
                emit_sfgn_value(out, val)?;
                out.push('\n');
            }
            BuilderPropValue::Multiline { tag, body } => {
                if let Some(t) = tag {
                    validate_format_tag(t)?;
                }
                out.push_str("\"\"\"");
                if let Some(t) = tag {
                    out.push_str(t);
                }
                out.push('\n');

                if !body.is_empty() {
                    for line in body.split_inclusive('\n') {
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
                    if !body.ends_with('\n') {
                        out.push('\n');
                    }
                }

                self.write_indent(out);
                out.push_str("\"\"\"\n");
            }
        }

        Ok(())
    }

    fn emit_edge_targets(
        &self,
        out: &mut String,
        targets: &[EdgeTarget],
        resolved_paths: &[Option<CanonicalNodePath>],
    ) -> Result<()> {
        if targets.len() == 1 {
            self.emit_single_target(out, &targets[0], resolved_paths)?;
        } else {
            out.push('[');
            for (i, target) in targets.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                self.emit_single_target(out, target, resolved_paths)?;
            }
            out.push(']');
        }
        Ok(())
    }

    fn emit_single_target(
        &self,
        out: &mut String,
        target: &EdgeTarget,
        resolved_paths: &[Option<CanonicalNodePath>],
    ) -> Result<()> {
        match target {
            EdgeTarget::Node(handle) => {
                if handle.0 >= self.arena.len() {
                    return Err(Diagnostic::new(
                        ErrorCode::E009,
                        LineCol::start(),
                        0,
                        format!("edge target NodeHandle({}) is out of bounds", handle.0),
                    ));
                }
                if *handle == NodeHandle::ROOT {
                    out.push_str(">:");
                } else {
                    let path = resolved_paths[handle.0]
                        .as_ref()
                        .expect("non-root target has resolved path");
                    write!(out, "{}", path).map_err(fmt_err)?;
                }
                Ok(())
            }
            EdgeTarget::External {
                file_path,
                sub_path,
            } => {
                out.push('<');
                write_escaped_quoted(out, file_path).map_err(fmt_err)?;
                out.push('>');
                if let Some(sub) = sub_path {
                    let canonical_sub = parse_canonical_node_path(sub)?;
                    write!(out, "{}", canonical_sub).map_err(fmt_err)?;
                }
                Ok(())
            }
        }
    }
}

fn emit_sfgn_value(out: &mut String, val: &SfgnValue) -> Result<()> {
    match val {
        SfgnValue::String(s) => write_escaped_quoted(out, s).map_err(fmt_err),
        SfgnValue::Number(raw) => {
            validate_number_literal(raw)?;
            out.push_str(raw);
            Ok(())
        }
        SfgnValue::Bool(true) => {
            out.push_str("true");
            Ok(())
        }
        SfgnValue::Bool(false) => {
            out.push_str("false");
            Ok(())
        }
        SfgnValue::Null => {
            out.push_str("null");
            Ok(())
        }
        SfgnValue::Collection(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_sfgn_scalar(out, item)?;
            }
            out.push(']');
            Ok(())
        }
    }
}

fn emit_sfgn_scalar(out: &mut String, scalar: &SfgnScalar) -> Result<()> {
    match scalar {
        SfgnScalar::String(s) => write_escaped_quoted(out, s).map_err(fmt_err),
        SfgnScalar::Number(raw) => {
            validate_number_literal(raw)?;
            out.push_str(raw);
            Ok(())
        }
        SfgnScalar::Bool(true) => {
            out.push_str("true");
            Ok(())
        }
        SfgnScalar::Bool(false) => {
            out.push_str("false");
            Ok(())
        }
        SfgnScalar::Null => {
            out.push_str("null");
            Ok(())
        }
    }
}

fn validate_number_literal(raw: &str) -> Result<()> {
    let mut lex = Lexer::new(raw)?;
    match lex.scan_number() {
        Some(span) if span.len == raw.len() => Ok(()),
        _ => Err(Diagnostic::new(
            ErrorCode::E009,
            LineCol::start(),
            0,
            format!("invalid SFGN numeric literal '{}'", raw),
        )),
    }
}

fn validate_format_tag(tag: &str) -> Result<()> {
    let bytes = tag.as_bytes();
    let Some(&first) = bytes.first() else {
        return Err(Diagnostic::new(
            ErrorCode::E014,
            LineCol::start(),
            0,
            "multiline format tag cannot be empty when Some is provided",
        ));
    };
    if !is_tag_start(first) || !bytes[1..].iter().all(|&b| is_tag_char(b)) {
        return Err(Diagnostic::new(
            ErrorCode::E014,
            LineCol::start(),
            0,
            format!("invalid multiline format tag '{}'", tag),
        ));
    }
    Ok(())
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
        format!("formatting failure during SFGN build: {}", err),
    )
}
