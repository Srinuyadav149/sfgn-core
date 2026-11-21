use std::collections::{HashMap, VecDeque};
use std::fmt;

use crate::error::{Diagnostic, ErrorCode, LineCol, Result};
use crate::lexer::Lexer;
use crate::tape::{NodeId, Span, StructuralTape};
use crate::{OrphanPolicy, ParseOptions};

/// A single canonicalized segment of a `NodePath`: unescaped segment name plus zero or more ordinal indices.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeSegmentKey {
    pub name: String,
    pub indices: Vec<usize>,
}

impl fmt::Display for NodeSegmentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_escaped_quoted(f, &self.name)?;
        for idx in &self.indices {
            write!(f, "#{}", idx)?;
        }
        Ok(())
    }
}

/// Canonical representation of a non-root `NodePath` (`Segment { ">" Segment }`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalNodePath(pub Vec<NodeSegmentKey>);

impl CanonicalNodePath {
    #[inline]
    pub fn segments(&self) -> &[NodeSegmentKey] {
        &self.0
    }

    #[inline]
    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for CanonicalNodePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(">")?;
            }
            write!(f, "{}", seg)?;
        }
        Ok(())
    }
}

/// A parsed target inside a `>` (child) or `>>` (relative) relation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationTarget {
    /// A node reference within the current `.sfgn` file.
    Local {
        path: CanonicalNodePath,
        /// Resolved `NodeId` if the target node is declared in this document.
        node_id: Option<NodeId>,
        /// Raw byte span of this target in the source buffer.
        span: Span,
    },
    /// An external subgraph portal (`<"File.sfgn">` with optional child-axis traversal).
    External {
        /// Unescaped external file path inside `<"..." >`.
        file_path: String,
        /// Optional descendant `NodePath` inside the external file's subgraph.
        sub_path: Option<CanonicalNodePath>,
        /// Raw byte span of this external portal in the source buffer.
        span: Span,
    },
}

/// Pass 2 topology index mapping canonical paths to `NodeId`s and storing validated `>` / `>>` edges.
#[derive(Debug, Clone)]
pub struct TopologyIndex {
    canonical_paths: Vec<Option<CanonicalNodePath>>,
    path_to_id: HashMap<CanonicalNodePath, NodeId>,
    children: Vec<Vec<RelationTarget>>,
    relatives: Vec<Vec<RelationTarget>>,
    local_child_ids: Vec<Vec<NodeId>>,
    parent_of: Vec<Option<NodeId>>,
    adopted_orphans: Vec<NodeId>,
}

impl TopologyIndex {
    /// Resolves a canonical `NodePath` to its `NodeId` in O(1) average time,
    /// falling back to top-down `>` containment traversal for full ancestor paths.
    #[inline]
    pub fn lookup(&self, path: &CanonicalNodePath) -> Option<NodeId> {
        if let Some(&id) = self.path_to_id.get(path) {
            return Some(id);
        }
        self.lookup_by_containment_chain(path)
    }

    fn lookup_by_containment_chain(&self, path: &CanonicalNodePath) -> Option<NodeId> {
        let segments = path.segments();
        if segments.is_empty() {
            return None;
        }
        let mut current = NodeId::ROOT;
        for seg in segments {
            let mut matched_child = None;
            for &child_id in self.local_child_ids(current) {
                if let Some(child_path) = self.canonical_path(child_id) {
                    if child_path.segments().last() == Some(seg) {
                        matched_child = Some(child_id);
                        break;
                    }
                }
            }
            current = matched_child?;
        }
        Some(current)
    }

    /// Parses an SFGN `NodePath` string (e.g., `"StorageEngine" > "BufferPool"`) and looks up its `NodeId`.
    pub fn lookup_str(&self, raw_path: &str) -> Result<Option<NodeId>> {
        if raw_path.trim() == ">:" {
            return Ok(Some(NodeId::ROOT));
        }
        let canonical = parse_canonical_node_path(raw_path)?;
        Ok(self.lookup(&canonical))
    }

    /// Returns the canonical `NodePath` for a non-root `NodeId` (`None` for `NodeId::ROOT`).
    #[inline]
    pub fn canonical_path(&self, id: NodeId) -> Option<&CanonicalNodePath> {
        self.canonical_paths.get(id.0).and_then(|p| p.as_ref())
    }

    /// Returns all parsed `>` child relation targets declared on `id`.
    #[inline]
    pub fn children(&self, id: NodeId) -> &[RelationTarget] {
        &self.children[id.0]
    }

    /// Returns all parsed `>>` relative relation targets declared on `id`.
    #[inline]
    pub fn relatives(&self, id: NodeId) -> &[RelationTarget] {
        &self.relatives[id.0]
    }

    /// Returns resolved local child `NodeId`s in the containment tree (including adopted orphans if configured).
    #[inline]
    pub fn local_child_ids(&self, id: NodeId) -> &[NodeId] {
        &self.local_child_ids[id.0]
    }

    /// Returns the containment parent of `id` in the tree rooted at `>:` (`None` for `NodeId::ROOT`).
    #[inline]
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.parent_of.get(id.0).copied().flatten()
    }

    /// Returns `NodeId`s that were adopted as direct children of `>:` under `OrphanPolicy::AttachToRoot`.
    #[inline]
    pub fn adopted_orphans(&self) -> &[NodeId] {
        &self.adopted_orphans
    }
}

/// Runs Pass 2 topology indexing and validation over a Pass 1 `StructuralTape` (Spec § 6.2).
pub fn build_topology(
    src: &str,
    tape: &StructuralTape,
    options: ParseOptions,
) -> Result<TopologyIndex> {
    let node_count = tape.nodes.len();
    if node_count == 0 {
        return Err(Diagnostic::new(
            ErrorCode::E002,
            LineCol::start(),
            0,
            "missing mandatory root node '>:' declaration",
        ));
    }

    let mut canonical_paths: Vec<Option<CanonicalNodePath>> = Vec::with_capacity(node_count);
    let mut path_to_id: HashMap<CanonicalNodePath, NodeId> =
        HashMap::with_capacity(node_count.saturating_sub(1));

    canonical_paths.push(None);

    for (idx, entry) in tape.nodes.iter().enumerate().skip(1) {
        let raw_header_path = entry.path_span.slice_str(src);
        let canonical =
            parse_canonical_node_path_at(raw_header_path, entry.header_loc, entry.path_span.start)?;

        if let Some(&existing_id) = path_to_id.get(&canonical) {
            let prev_loc = tape.nodes[existing_id.0].header_loc;
            return Err(Diagnostic::new(
                ErrorCode::E016,
                entry.header_loc,
                entry.full_span.start_byte,
                format!(
                    "duplicate canonical node identifier {} (first declared at {})",
                    canonical, prev_loc
                ),
            ));
        }

        path_to_id.insert(canonical.clone(), NodeId(idx));
        canonical_paths.push(Some(canonical));
    }

    let mut children: Vec<Vec<RelationTarget>> = vec![Vec::new(); node_count];
    let mut relatives: Vec<Vec<RelationTarget>> = vec![Vec::new(); node_count];
    let mut local_child_ids: Vec<Vec<NodeId>> = vec![Vec::new(); node_count];

    for (node_idx, entry) in tape.nodes.iter().enumerate() {
        for rel in tape.node_relations(entry) {
            let raw_target = rel.target_span.slice_str(src);
            let parsed_targets = parse_relation_targets(
                raw_target,
                rel.target_span.start,
                rel.loc,
                rel.kind.is_array(),
                &path_to_id,
            )?;

            if rel.kind.is_child() {
                for target in &parsed_targets {
                    if let RelationTarget::Local {
                        node_id: Some(child_id),
                        ..
                    } = target
                    {
                        local_child_ids[node_idx].push(*child_id);
                    }
                }
                children[node_idx].extend(parsed_targets);
            } else {
                relatives[node_idx].extend(parsed_targets);
            }
        }
    }

    let mut reachable = vec![false; node_count];
    let mut parent_of: Vec<Option<NodeId>> = vec![None; node_count];
    let mut queue: VecDeque<NodeId> = VecDeque::with_capacity(node_count);

    reachable[0] = true;
    queue.push_back(NodeId::ROOT);

    while let Some(curr) = queue.pop_front() {
        for &child_id in &local_child_ids[curr.0] {
            if !reachable[child_id.0] {
                reachable[child_id.0] = true;
                parent_of[child_id.0] = Some(curr);
                queue.push_back(child_id);
            }
        }
    }

    let mut adopted_orphans: Vec<NodeId> = Vec::new();
    for node_idx in 1..node_count {
        if !reachable[node_idx] {
            match options.orphan_policy {
                OrphanPolicy::StrictError => {
                    let entry = &tape.nodes[node_idx];
                    let display_name = canonical_paths[node_idx]
                        .as_ref()
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| entry.path_span.slice_str(src).to_string());
                    return Err(Diagnostic::new(
                        ErrorCode::E017,
                        entry.header_loc,
                        entry.full_span.start_byte,
                        format!(
                            "orphan node {} is not reachable from '>:' via '>' child edges",
                            display_name
                        ),
                    ));
                }
                OrphanPolicy::AttachToRoot => {
                    let orphan_id = NodeId(node_idx);
                    reachable[node_idx] = true;
                    parent_of[node_idx] = Some(NodeId::ROOT);
                    local_child_ids[0].push(orphan_id);
                    adopted_orphans.push(orphan_id);

                    queue.push_back(orphan_id);
                    while let Some(curr) = queue.pop_front() {
                        for &child_id in &local_child_ids[curr.0] {
                            if !reachable[child_id.0] {
                                reachable[child_id.0] = true;
                                parent_of[child_id.0] = Some(curr);
                                queue.push_back(child_id);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(TopologyIndex {
        canonical_paths,
        path_to_id,
        children,
        relatives,
        local_child_ids,
        parent_of,
        adopted_orphans,
    })
}

/// Parses an SFGN `NodePath` string into a `CanonicalNodePath`.
pub fn parse_canonical_node_path(raw: &str) -> Result<CanonicalNodePath> {
    parse_canonical_node_path_at(raw, LineCol::start(), 0)
}

fn parse_canonical_node_path_at(
    raw: &str,
    base_loc: LineCol,
    base_offset: usize,
) -> Result<CanonicalNodePath> {
    let mut lex = Lexer::new(raw).map_err(|d| shift_diag(d, base_loc, base_offset))?;
    lex.skip_ows();
    let path = parse_node_path_from_lexer(&mut lex, base_loc, base_offset)?;
    lex.skip_ows();
    if !lex.is_eof() {
        return Err(shift_diag(
            lex.err(ErrorCode::E009, "unexpected trailing tokens in node path"),
            base_loc,
            base_offset,
        ));
    }
    Ok(path)
}

fn parse_node_path_from_lexer(
    lex: &mut Lexer<'_>,
    base_loc: LineCol,
    base_offset: usize,
) -> Result<CanonicalNodePath> {
    let mut segments = Vec::with_capacity(2);
    segments.push(parse_node_segment_from_lexer(lex, base_loc, base_offset)?);

    loop {
        let saved_pos = lex.pos();
        let saved_line = lex.loc().line;
        let saved_col = lex.loc().col;
        lex.skip_ows();

        if lex.peek() == Some(b'>') {
            lex.advance_ascii(1);
            lex.skip_ows();
            if lex.peek() != Some(b'"') {
                return Err(shift_diag(
                    lex.err(
                        ErrorCode::E009,
                        "expected quoted segment after '>' in node path",
                    ),
                    base_loc,
                    base_offset,
                ));
            }
            segments.push(parse_node_segment_from_lexer(lex, base_loc, base_offset)?);
        } else {
            lex.restore_same_line(saved_pos, saved_line, saved_col);
            break;
        }
    }

    Ok(CanonicalNodePath(segments))
}

fn parse_node_segment_from_lexer(
    lex: &mut Lexer<'_>,
    base_loc: LineCol,
    base_offset: usize,
) -> Result<NodeSegmentKey> {
    if lex.peek() != Some(b'"') {
        return Err(shift_diag(
            lex.err(ErrorCode::E009, "expected double-quoted node segment"),
            base_loc,
            base_offset,
        ));
    }

    let quoted_span = lex
        .scan_quoted_string()
        .map_err(|d| shift_diag(d, base_loc, base_offset))?;
    let name = unescape_quoted_string(quoted_span.slice_str(lex.source()));
    let mut indices = Vec::new();

    loop {
        let saved_pos = lex.pos();
        let saved_line = lex.loc().line;
        let saved_col = lex.loc().col;
        lex.skip_ows();

        if lex.peek() == Some(b'#') {
            let hash_loc = lex.loc();
            let hash_pos = lex.pos();
            lex.advance_ascii(1);
            let digits_start = lex.pos();
            while let Some(b) = lex.peek() {
                if b.is_ascii_digit() {
                    lex.advance_ascii(1);
                } else {
                    break;
                }
            }
            let digits_end = lex.pos();
            if digits_start == digits_end {
                return Err(shift_diag(
                    lex.err_at(
                        ErrorCode::E010,
                        hash_loc,
                        hash_pos,
                        "ordinal index '#' must be followed by ASCII digits",
                    ),
                    base_loc,
                    base_offset,
                ));
            }
            let digits_str = &lex.source()[digits_start..digits_end];
            let idx = digits_str.parse::<usize>().map_err(|_| {
                shift_diag(
                    lex.err_at(
                        ErrorCode::E010,
                        hash_loc,
                        hash_pos,
                        "ordinal index overflows usize",
                    ),
                    base_loc,
                    base_offset,
                )
            })?;
            indices.push(idx);
        } else {
            lex.restore_same_line(saved_pos, saved_line, saved_col);
            break;
        }
    }

    Ok(NodeSegmentKey { name, indices })
}

fn parse_relation_targets(
    raw_target: &str,
    base_offset: usize,
    base_loc: LineCol,
    is_array: bool,
    path_to_id: &HashMap<CanonicalNodePath, NodeId>,
) -> Result<Vec<RelationTarget>> {
    let mut lex = Lexer::new(raw_target).map_err(|d| shift_diag(d, base_loc, base_offset))?;
    let mut targets = Vec::new();

    if is_array {
        debug_assert_eq!(lex.peek(), Some(b'['));
        lex.advance_ascii(1);
        lex.skip_ows();
        if lex.peek() == Some(b']') {
            return Ok(targets);
        }

        loop {
            targets.push(parse_single_target_from_lexer(
                &mut lex,
                base_offset,
                base_loc,
                path_to_id,
            )?);
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
                    return Err(shift_diag(
                        lex.err(ErrorCode::E012, "expected ',' or ']' in target array"),
                        base_loc,
                        base_offset,
                    ));
                }
            }
        }
    } else {
        targets.push(parse_single_target_from_lexer(
            &mut lex,
            base_offset,
            base_loc,
            path_to_id,
        )?);
    }

    Ok(targets)
}

fn parse_single_target_from_lexer(
    lex: &mut Lexer<'_>,
    base_offset: usize,
    base_loc: LineCol,
    path_to_id: &HashMap<CanonicalNodePath, NodeId>,
) -> Result<RelationTarget> {
    let start_rel = lex.pos();
    if lex.peek() == Some(b'<') {
        lex.advance_ascii(1);
        lex.skip_ows();
        let file_span = lex
            .scan_quoted_string()
            .map_err(|d| shift_diag(d, base_loc, base_offset))?;
        let file_path = unescape_quoted_string(file_span.slice_str(lex.source()));
        lex.skip_ows();
        if lex.peek() != Some(b'>') {
            return Err(shift_diag(
                lex.err(
                    ErrorCode::E015,
                    "malformed external reference: missing closing '>'",
                ),
                base_loc,
                base_offset,
            ));
        }
        lex.advance_ascii(1);

        let saved_pos = lex.pos();
        let saved_line = lex.loc().line;
        let saved_col = lex.loc().col;
        lex.skip_ows();

        let sub_path = if lex.peek() == Some(b'"') {
            Some(parse_node_path_from_lexer(lex, base_loc, base_offset)?)
        } else {
            lex.restore_same_line(saved_pos, saved_line, saved_col);
            None
        };

        let end_rel = lex.pos();
        Ok(RelationTarget::External {
            file_path,
            sub_path,
            span: Span::new(base_offset + start_rel, end_rel - start_rel),
        })
    } else {
        let path = parse_node_path_from_lexer(lex, base_loc, base_offset)?;
        let end_rel = lex.pos();
        let node_id = path_to_id.get(&path).copied();
        Ok(RelationTarget::Local {
            path,
            node_id,
            span: Span::new(base_offset + start_rel, end_rel - start_rel),
        })
    }
}

/// Decodes a validated SFGN double-quoted string literal (`"..."`) into its unescaped `String`.
pub fn unescape_quoted_string(raw_quoted: &str) -> String {
    debug_assert!(
        raw_quoted.len() >= 2 && raw_quoted.starts_with('"') && raw_quoted.ends_with('"')
    );
    let inner = &raw_quoted[1..raw_quoted.len() - 1];
    if !inner.contains('\\') {
        return inner.to_string();
    }

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('u') => {
                let _ = chars.next();
                let mut codepoint = 0u32;
                for h in chars.by_ref() {
                    if h == '}' {
                        break;
                    }
                    if let Some(d) = h.to_digit(16) {
                        codepoint = (codepoint << 4) | d;
                    }
                }
                if let Some(ch) = char::from_u32(codepoint) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

fn write_escaped_quoted(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for ch in s.chars() {
        match ch {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            '\0' => f.write_str("\\0")?,
            c if (c as u32) <= 0x1F || (c as u32) == 0x7F => {
                write!(f, "\\u{{{:X}}}", c as u32)?;
            }
            c => write!(f, "{}", c)?,
        }
    }
    f.write_str("\"")
}

#[inline]
fn shift_diag(mut diag: Diagnostic, base_loc: LineCol, base_offset: usize) -> Diagnostic {
    if diag.loc.line == 1 {
        diag.loc = LineCol::new(base_loc.line, base_loc.col + diag.loc.col - 1);
    } else {
        diag.loc = LineCol::new(base_loc.line + diag.loc.line - 1, diag.loc.col);
    }
    diag.offset += base_offset;
    diag
}
