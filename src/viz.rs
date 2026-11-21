use std::collections::HashSet;
use std::fmt::Write;

use crate::document::{Document, NodeView, PropertyView, RelationAxis};
use crate::tape::{NodeId, PropertyKind};

/// Renders any parsed SFGN `Document` as a human-readable ASCII tree showing both
/// hierarchical `>` containment edges and associative `>>` relative links.
pub fn render_ascii_tree(doc: &Document<'_>) -> String {
    let mut out = String::with_capacity(doc.len() * 96);
    let mut visited = HashSet::with_capacity(doc.len());

    let root = doc.root();
    visited.insert(root.id());

    let root_props = format_inline_props(root);
    if root_props.is_empty() {
        writeln!(out, ">:").ok();
    } else {
        writeln!(out, ">:  {{{}}}", root_props).ok();
    }
    write_node_edges(&mut out, root, "");

    let children: Vec<NodeView<'_, '_>> = root.children().collect();
    for (idx, child) in children.iter().enumerate() {
        let is_last = idx + 1 == children.len();
        render_ascii_subtree(&mut out, *child, "", is_last, &mut visited);
    }

    out
}

fn render_ascii_subtree(
    out: &mut String,
    node: NodeView<'_, '_>,
    prefix: &str,
    is_last: bool,
    visited: &mut HashSet<NodeId>,
) {
    let branch = if is_last { "└── " } else { "├── " };
    let child_prefix = if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    let label = node
        .canonical_path()
        .map(|p| p.to_string())
        .unwrap_or_else(|| node.raw_header_path().to_owned());

    let props = format_inline_props(node);
    if props.is_empty() {
        writeln!(out, "{}{}{}", prefix, branch, label).ok();
    } else {
        writeln!(out, "{}{}{}  {{{}}}", prefix, branch, label, props).ok();
    }

    write_node_edges(out, node, &child_prefix);

    if !visited.insert(node.id()) {
        writeln!(out, "{}└── (cycle ref {})", child_prefix, label).ok();
        return;
    }

    let children: Vec<NodeView<'_, '_>> = node.children().collect();
    for (idx, child) in children.iter().enumerate() {
        let child_is_last = idx + 1 == children.len();
        render_ascii_subtree(out, *child, &child_prefix, child_is_last, visited);
    }
}

fn write_node_edges(out: &mut String, node: NodeView<'_, '_>, indent_prefix: &str) {
    let rels: Vec<String> = node
        .relatives()
        .map(|r| {
            r.canonical_path()
                .map(|p| p.to_string())
                .unwrap_or_else(|| r.raw_header_path().to_owned())
        })
        .collect();
    if !rels.is_empty() {
        writeln!(out, "{}  ──>> [{}]", indent_prefix, rels.join(", ")).ok();
    }

    for portal in node.external_portals() {
        let arrow = match portal.axis {
            RelationAxis::Child => "──> ",
            RelationAxis::Relative => "──>>",
        };
        match portal.sub_path {
            Some(sub) => {
                writeln!(
                    out,
                    "{}  {} <\"{}\">{}",
                    indent_prefix, arrow, portal.file_path, sub
                )
                .ok();
            }
            None => {
                writeln!(
                    out,
                    "{}  {} <\"{}\">",
                    indent_prefix, arrow, portal.file_path
                )
                .ok();
            }
        }
    }
}

fn format_inline_props(node: NodeView<'_, '_>) -> String {
    let parts: Vec<String> = node.properties().map(format_single_prop).collect();
    parts.join(", ")
}

fn format_single_prop(prop: PropertyView<'_>) -> String {
    let key = prop.key_name();
    let indices = prop.key_indices();
    let mut key_str = key.into_owned();
    for idx in indices {
        write!(key_str, "#{}", idx).ok();
    }

    match prop.kind() {
        PropertyKind::Scalar | PropertyKind::FlatCollection => {
            format!("{}: {}", key_str, prop.raw_value())
        }
        PropertyKind::MultilineBlock => {
            let tag = prop.format_tag().unwrap_or("text");
            let bytes = prop.raw_value().len();
            format!("{}: \"\"\"{}\"\"\" ({}B)", key_str, tag, bytes)
        }
    }
}

/// Renders any parsed SFGN `Document` as a Mermaid flowchart (`graph TD`) with solid arrows
/// for `>` hierarchical containment and dashed arrows for `>>` associative links.
pub fn render_mermaid(doc: &Document<'_>) -> String {
    let mut out = String::with_capacity(doc.len() * 128);
    writeln!(out, "graph TD").ok();

    for node in doc.nodes() {
        let id = node.id().index();
        let header = if node.is_root() {
            ">: (root)".to_owned()
        } else {
            node.canonical_path()
                .map(|p| p.to_string())
                .unwrap_or_else(|| node.raw_header_path().to_owned())
        };
        let sanitized = sanitize_mermaid_label(&header);
        if node.is_root() {
            writeln!(out, "  N{}[\"{}\"]", id, sanitized).ok();
        } else {
            writeln!(out, "  N{}(\"{}\")", id, sanitized).ok();
        }
    }

    let mut ext_counter = 0usize;
    for node in doc.nodes() {
        let from_id = node.id().index();

        for child in node.children() {
            writeln!(out, "  N{} --> N{}", from_id, child.id().index()).ok();
        }

        for rel in node.relatives() {
            writeln!(out, "  N{} -. \">>\" .-> N{}", from_id, rel.id().index()).ok();
        }

        for portal in node.external_portals() {
            let ext_id = ext_counter;
            ext_counter += 1;
            let raw_portal = match portal.sub_path {
                Some(sub) => format!("<\"{}\">{}", portal.file_path, sub),
                None => format!("<\"{}\">", portal.file_path),
            };
            let sanitized = sanitize_mermaid_label(&raw_portal);
            writeln!(out, "  EXT{}([\"{}\"])", ext_id, sanitized).ok();
            match portal.axis {
                RelationAxis::Child => {
                    writeln!(out, "  N{} --> EXT{}", from_id, ext_id).ok();
                }
                RelationAxis::Relative => {
                    writeln!(out, "  N{} -. \">>\" .-> EXT{}", from_id, ext_id).ok();
                }
            }
        }
    }

    out
}

fn sanitize_mermaid_label(raw: &str) -> String {
    raw.replace('"', "'")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
