use std::collections::BTreeSet;
use std::path::Path;

use tree_sitter::{Node, Parser};

use crate::error::{CodeExtractError, Result};

/// Supported source code languages (feature-gated in `Cargo.toml`, with `rust` enabled by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageKind {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
}

impl LanguageKind {
    /// Detects a `LanguageKind` from a file extension, returning `None` for non-code files.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            _ => None,
        }
    }

    /// Returns `true` if the corresponding `tree-sitter-*` feature is compiled into this crate.
    pub const fn is_enabled(self) -> bool {
        match self {
            Self::Rust => cfg!(feature = "rust"),
            Self::Python => cfg!(feature = "python"),
            Self::TypeScript | Self::Tsx | Self::JavaScript => cfg!(feature = "typescript"),
        }
    }

    /// Returns the SFGN multiline format tag (`"""rust`, `"""python`, `"""ts`, `"""tsx`, `"""js`).
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "ts",
            Self::Tsx => "tsx",
            Self::JavaScript => "js",
        }
    }
}

/// Extracted syntactic item from a single source file.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedSymbol {
    pub name: String,
    pub kind: &'static str,
    pub visibility: String,
    pub signature: String,
    pub doc: Option<String>,
    /// 1-based inclusive `[start_line, end_line]`.
    pub line_span: [usize; 2],
    pub code: String,
    pub children: Vec<ExtractedSymbol>,
    pub calls: Vec<String>,
    pub type_refs: Vec<String>,
}

impl ExtractedSymbol {
    /// Formats the SFGN node header label prefixed with its syntactic construct kind
    /// (e.g., `"struct SfgnBuilder"`, `"trait Display"`, `"mod lexer"`, `"fn scan_tape"`, `"method new"`).
    pub fn prefixed_name(&self) -> String {
        match self.kind {
            "module" => format!("mod {}", self.name),
            "struct" => format!("struct {}", self.name),
            "enum" => format!("enum {}", self.name),
            "trait" => format!("trait {}", self.name),
            "fn" => format!("fn {}", self.name),
            "method" => format!("method {}", self.name),
            "type_alias" => format!("type {}", self.name),
            "const" => format!("const {}", self.name),
            "static" => format!("static {}", self.name),
            "macro" => format!("macro {}", self.name),
            "class" => format!("class {}", self.name),
            "interface" => format!("interface {}", self.name),
            // `impl` items are already formatted as `"impl Trait for Type"` or `"impl Type"`
            "impl" => self.name.clone(),
            other => format!("{} {}", other, self.name),
        }
    }
}

/// Extracted file-level CST summary.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedFile {
    pub language: LanguageKind,
    pub doc: Option<String>,
    pub imports: Vec<String>,
    pub symbols: Vec<ExtractedSymbol>,
}

/// Parses `source` using the enabled `tree-sitter` grammar for `lang`.
pub(crate) fn extract_file_symbols(
    path: &Path,
    lang: LanguageKind,
    source: &str,
) -> Result<ExtractedFile> {
    let mut parser = Parser::new();
    set_parser_language(&mut parser, path, lang)?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| CodeExtractError::Parse {
            path: path.to_path_buf(),
            language: lang.tag(),
            reason: "tree-sitter returned no syntax tree".to_owned(),
        })?;

    let root = tree.root_node();

    match lang {
        #[cfg(feature = "rust")]
        LanguageKind::Rust => Ok(extract_rust_file(root, source)),
        #[cfg(feature = "python")]
        LanguageKind::Python => Ok(extract_python_file(root, source)),
        #[cfg(feature = "typescript")]
        LanguageKind::TypeScript | LanguageKind::Tsx | LanguageKind::JavaScript => {
            Ok(extract_ts_file(root, source, lang))
        }
        #[allow(unreachable_patterns)]
        _ => Err(CodeExtractError::Parse {
            path: path.to_path_buf(),
            language: lang.tag(),
            reason: format!("language feature '{}' is not enabled", lang.tag()),
        }),
    }
}

fn set_parser_language(parser: &mut Parser, path: &Path, lang: LanguageKind) -> Result<()> {
    match lang {
        #[cfg(feature = "rust")]
        LanguageKind::Rust => parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|e| CodeExtractError::Parse {
                path: path.to_path_buf(),
                language: lang.tag(),
                reason: e.to_string(),
            }),
        #[cfg(feature = "python")]
        LanguageKind::Python => parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .map_err(|e| CodeExtractError::Parse {
                path: path.to_path_buf(),
                language: lang.tag(),
                reason: e.to_string(),
            }),
        #[cfg(feature = "typescript")]
        LanguageKind::TypeScript | LanguageKind::JavaScript => parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .map_err(|e| CodeExtractError::Parse {
                path: path.to_path_buf(),
                language: lang.tag(),
                reason: e.to_string(),
            }),
        #[cfg(feature = "typescript")]
        LanguageKind::Tsx => parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .map_err(|e| CodeExtractError::Parse {
                path: path.to_path_buf(),
                language: lang.tag(),
                reason: e.to_string(),
            }),
        #[allow(unreachable_patterns)]
        _ => Err(CodeExtractError::Parse {
            path: path.to_path_buf(),
            language: lang.tag(),
            reason: format!("language feature '{}' is not enabled", lang.tag()),
        }),
    }
}

// ============================================================================
// Rust Extractor (`feature = "rust"`)
// ============================================================================

#[cfg(feature = "rust")]
fn extract_rust_file(root: Node<'_>, src: &str) -> ExtractedFile {
    let mut imports = BTreeSet::new();
    let mut symbols = Vec::new();
    let mut doc_lines: Vec<String> = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "line_comment" {
            let text = node_text(child, src).trim();
            if let Some(inner) = text.strip_prefix("//!") {
                doc_lines.push(inner.trim().to_owned());
            }
        } else if child.kind() == "use_declaration" {
            collect_rust_use_targets(child, src, &mut imports);
        } else if let Some(sym) = extract_rust_item(child, src) {
            symbols.push(sym);
        }
    }

    merge_rust_impls_into_types(&mut symbols);

    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    };

    ExtractedFile {
        language: LanguageKind::Rust,
        doc,
        imports: imports.into_iter().collect(),
        symbols,
    }
}

#[cfg(feature = "rust")]
fn extract_rust_item(node: Node<'_>, src: &str) -> Option<ExtractedSymbol> {
    let kind_str = match node.kind() {
        "mod_item" => "module",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "impl_item" => "impl",
        "function_item" => "fn",
        "type_item" => "type_alias",
        "const_item" => "const",
        "static_item" => "static",
        "macro_definition" => "macro",
        _ => return None,
    };

    let visibility = extract_rust_visibility(node, src);
    let doc = collect_leading_doc_comments(node, src);
    let full_text = node_text(node, src);
    let line_span = [node.start_position().row + 1, node.end_position().row + 1];

    let mut calls = BTreeSet::new();
    let mut type_refs = BTreeSet::new();
    let mut children = Vec::new();
    let mut self_type: Option<String> = None;

    let name = match node.kind() {
        "impl_item" => {
            let ty_node = node.child_by_field_name("type")?;
            let ty_name = simplify_rust_type_name(node_text(ty_node, src));
            type_refs.insert(ty_name.clone());
            self_type = Some(ty_name.clone());

            if let Some(trait_node) = node.child_by_field_name("trait") {
                let tr_name = simplify_rust_type_name(node_text(trait_node, src));
                type_refs.insert(tr_name.clone());
                format!("impl {} for {}", tr_name, ty_name)
            } else {
                format!("impl {}", ty_name)
            }
        }
        _ => {
            let name_node = node.child_by_field_name("name")?;
            node_text(name_node, src).trim().to_owned()
        }
    };

    let signature = extract_signature_before_body(node, src, b'{');

    if let Some(body) = node.child_by_field_name("body") {
        match node.kind() {
            "mod_item" => {
                let mut cursor = body.walk();
                for item in body.children(&mut cursor) {
                    if let Some(sub) = extract_rust_item(item, src) {
                        children.push(sub);
                    }
                }
                merge_rust_impls_into_types(&mut children);
            }
            "trait_item" | "impl_item" => {
                let enclosing_type = self_type.as_deref().unwrap_or(name.as_str());
                let mut cursor = body.walk();
                for item in body.children(&mut cursor) {
                    if item.kind() == "function_item" || item.kind() == "function_signature_item" {
                        if let Some(method) = extract_rust_method(item, src, Some(enclosing_type)) {
                            children.push(method);
                        }
                    }
                }
            }
            "function_item" => {
                collect_rust_calls_and_types(body, src, None, &mut calls, &mut type_refs);
            }
            _ => {
                collect_rust_types_only(body, src, &mut type_refs);
            }
        }
    }

    if let Some(params) = node.child_by_field_name("parameters") {
        collect_rust_types_only(params, src, &mut type_refs);
    }
    if let Some(ret) = node.child_by_field_name("return_type") {
        collect_rust_types_only(ret, src, &mut type_refs);
    }

    type_refs.remove(&name);
    calls.remove(&name);

    Some(ExtractedSymbol {
        name,
        kind: kind_str,
        visibility,
        signature,
        doc,
        line_span,
        code: full_text.to_owned(),
        children,
        calls: calls.into_iter().collect(),
        type_refs: type_refs.into_iter().collect(),
    })
}

#[cfg(feature = "rust")]
fn extract_rust_method(
    node: Node<'_>,
    src: &str,
    enclosing_type: Option<&str>,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, src).trim().to_owned();
    let visibility = extract_rust_visibility(node, src);
    let doc = collect_leading_doc_comments(node, src);
    let full_text = node_text(node, src);
    let line_span = [node.start_position().row + 1, node.end_position().row + 1];
    let signature = extract_signature_before_body(node, src, b'{');

    let mut calls = BTreeSet::new();
    let mut type_refs = BTreeSet::new();

    if let Some(body) = node.child_by_field_name("body") {
        collect_rust_calls_and_types(body, src, enclosing_type, &mut calls, &mut type_refs);
    }
    if let Some(params) = node.child_by_field_name("parameters") {
        collect_rust_types_only(params, src, &mut type_refs);
    }
    if let Some(ret) = node.child_by_field_name("return_type") {
        collect_rust_types_only(ret, src, &mut type_refs);
    }

    calls.remove(&name);
    if let Some(ty) = enclosing_type {
        calls.remove(&format!("{}::{}", ty, name));
    }

    Some(ExtractedSymbol {
        name,
        kind: "method",
        visibility,
        signature,
        doc,
        line_span,
        code: full_text.to_owned(),
        children: Vec::new(),
        calls: calls.into_iter().collect(),
        type_refs: type_refs.into_iter().collect(),
    })
}

/// Attaches inherent `impl Type { ... }` methods directly as children of `struct Type` / `enum Type`
/// when defined in the same module, matching `Struct > Method` lexical containment (Doc 1 § 3.2).
#[cfg(feature = "rust")]
fn merge_rust_impls_into_types(symbols: &mut Vec<ExtractedSymbol>) {
    let mut type_positions = std::collections::HashMap::new();
    for (idx, sym) in symbols.iter().enumerate() {
        if matches!(sym.kind, "struct" | "enum") {
            type_positions.insert(sym.name.clone(), idx);
        }
    }

    let mut merged_impl_indices = BTreeSet::new();
    for idx in 0..symbols.len() {
        if symbols[idx].kind == "impl" {
            if let Some(target_type) = symbols[idx].name.strip_prefix("impl ") {
                if !target_type.contains(" for ") {
                    if let Some(&target_idx) = type_positions.get(target_type) {
                        let methods = std::mem::take(&mut symbols[idx].children);
                        symbols[target_idx].children.extend(methods);
                        merged_impl_indices.insert(idx);
                    }
                }
            }
        }
    }

    if !merged_impl_indices.is_empty() {
        let mut filtered = Vec::with_capacity(symbols.len() - merged_impl_indices.len());
        for (idx, sym) in symbols.drain(..).enumerate() {
            if !merged_impl_indices.contains(&idx) {
                filtered.push(sym);
            }
        }
        *symbols = filtered;
    }
}

#[cfg(feature = "rust")]
fn extract_rust_visibility(node: Node<'_>, src: &str) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return node_text(child, src).trim().to_owned();
        }
    }
    "private".to_owned()
}

#[cfg(feature = "rust")]
fn collect_rust_use_targets(node: Node<'_>, src: &str, imports: &mut BTreeSet<String>) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "identifier" || curr.kind() == "type_identifier" {
            let ident = node_text(curr, src).trim();
            if !matches!(ident, "crate" | "super" | "self") && !ident.is_empty() {
                imports.insert(ident.to_owned());
            }
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(feature = "rust")]
fn collect_rust_calls_and_types(
    node: Node<'_>,
    src: &str,
    enclosing_type: Option<&str>,
    calls: &mut BTreeSet<String>,
    type_refs: &mut BTreeSet<String>,
) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        match curr.kind() {
            "call_expression" => {
                if let Some(func_node) = curr.child_by_field_name("function") {
                    if let Some(callee) = extract_callee_name(func_node, src, enclosing_type) {
                        calls.insert(callee);
                    }
                }
            }
            "macro_invocation" => {
                if let Some(mac_node) = curr.child_by_field_name("macro") {
                    let name = simplify_scoped_name(node_text(mac_node, src));
                    if !name.is_empty() {
                        calls.insert(format!("{}!", name));
                    }
                }
            }
            "type_identifier" => {
                let ty = node_text(curr, src).trim();
                if !is_builtin_rust_type(ty) {
                    type_refs.insert(ty.to_owned());
                }
            }
            _ => {}
        }

        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(feature = "rust")]
fn collect_rust_types_only(node: Node<'_>, src: &str, type_refs: &mut BTreeSet<String>) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "type_identifier" {
            let ty = node_text(curr, src).trim();
            if !is_builtin_rust_type(ty) {
                type_refs.insert(ty.to_owned());
            }
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(feature = "rust")]
fn simplify_rust_type_name(raw: &str) -> String {
    let before_generics = raw.split('<').next().unwrap_or(raw).trim();
    simplify_scoped_name(before_generics)
}

#[cfg(feature = "rust")]
fn simplify_scoped_name(raw: &str) -> String {
    raw.rsplit("::").next().unwrap_or(raw).trim().to_owned()
}

#[cfg(feature = "rust")]
fn is_builtin_rust_type(ty: &str) -> bool {
    matches!(
        ty,
        "Self"
            | "String"
            | "Vec"
            | "Option"
            | "Result"
            | "Box"
            | "Arc"
            | "Rc"
            | "Cow"
            | "HashMap"
            | "HashSet"
            | "BTreeMap"
            | "BTreeSet"
            | "Path"
            | "PathBuf"
    )
}

// ============================================================================
// Python Extractor (`feature = "python"`)
// ============================================================================

#[cfg(feature = "python")]
fn extract_python_file(root: Node<'_>, src: &str) -> ExtractedFile {
    let mut imports = BTreeSet::new();
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let actual = if child.kind() == "decorated_definition" {
            child.child_by_field_name("definition").unwrap_or(child)
        } else {
            child
        };

        match actual.kind() {
            "import_statement" | "import_from_statement" => {
                collect_python_imports(actual, src, &mut imports);
            }
            "class_definition" => {
                if let Some(sym) = extract_python_class(child, actual, src) {
                    symbols.push(sym);
                }
            }
            "function_definition" => {
                if let Some(sym) = extract_python_function(child, actual, src, None) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }

    ExtractedFile {
        language: LanguageKind::Python,
        doc: extract_python_docstring(root, src),
        imports: imports.into_iter().collect(),
        symbols,
    }
}

#[cfg(feature = "python")]
fn extract_python_class(
    outer: Node<'_>,
    class_node: Node<'_>,
    src: &str,
) -> Option<ExtractedSymbol> {
    let name_node = class_node.child_by_field_name("name")?;
    let name = node_text(name_node, src).trim().to_owned();
    let visibility = if name.starts_with('_') {
        "private".to_owned()
    } else {
        "public".to_owned()
    };
    let line_span = [outer.start_position().row + 1, outer.end_position().row + 1];
    let signature = extract_signature_before_body(class_node, src, b':');

    let mut type_refs = BTreeSet::new();
    if let Some(superclasses) = class_node.child_by_field_name("superclasses") {
        let mut cursor = superclasses.walk();
        for arg in superclasses.children(&mut cursor) {
            if arg.kind() == "identifier" {
                type_refs.insert(node_text(arg, src).trim().to_owned());
            }
        }
    }

    let mut children = Vec::new();
    let mut doc = None;
    if let Some(body) = class_node.child_by_field_name("body") {
        doc = extract_python_docstring(body, src);
        let mut cursor = body.walk();
        for item in body.children(&mut cursor) {
            let actual = if item.kind() == "decorated_definition" {
                item.child_by_field_name("definition").unwrap_or(item)
            } else {
                item
            };
            if actual.kind() == "function_definition" {
                if let Some(method) =
                    extract_python_function(item, actual, src, Some(name.as_str()))
                {
                    children.push(method);
                }
            }
        }
    }

    Some(ExtractedSymbol {
        name,
        kind: "class",
        visibility,
        signature,
        doc,
        line_span,
        code: node_text(outer, src).to_owned(),
        children,
        calls: Vec::new(),
        type_refs: type_refs.into_iter().collect(),
    })
}

#[cfg(feature = "python")]
fn extract_python_function(
    outer: Node<'_>,
    fn_node: Node<'_>,
    src: &str,
    enclosing_class: Option<&str>,
) -> Option<ExtractedSymbol> {
    let name_node = fn_node.child_by_field_name("name")?;
    let name = node_text(name_node, src).trim().to_owned();
    let visibility = if name.starts_with('_') && !name.starts_with("__") {
        "private".to_owned()
    } else {
        "public".to_owned()
    };
    let line_span = [outer.start_position().row + 1, outer.end_position().row + 1];
    let signature = extract_signature_before_body(fn_node, src, b':');

    let mut calls = BTreeSet::new();
    let mut doc = None;
    if let Some(body) = fn_node.child_by_field_name("body") {
        doc = extract_python_docstring(body, src);
        collect_python_calls(body, src, enclosing_class, &mut calls);
    }
    calls.remove(&name);
    if let Some(cls) = enclosing_class {
        calls.remove(&format!("{}::{}", cls, name));
    }

    Some(ExtractedSymbol {
        name,
        kind: if enclosing_class.is_some() {
            "method"
        } else {
            "fn"
        },
        visibility,
        signature,
        doc,
        line_span,
        code: node_text(outer, src).to_owned(),
        children: Vec::new(),
        calls: calls.into_iter().collect(),
        type_refs: Vec::new(),
    })
}

#[cfg(feature = "python")]
fn extract_python_docstring(block: Node<'_>, src: &str) -> Option<String> {
    let mut cursor = block.walk();
    for child in block.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            let mut inner_cursor = child.walk();
            for expr in child.children(&mut inner_cursor) {
                if expr.kind() == "string" {
                    let raw = node_text(expr, src).trim();
                    let stripped = raw
                        .trim_start_matches("\"\"\"")
                        .trim_end_matches("\"\"\"")
                        .trim_start_matches("'''")
                        .trim_end_matches("'''")
                        .trim();
                    if !stripped.is_empty() {
                        return Some(stripped.to_owned());
                    }
                }
            }
            break;
        } else if !child.is_extra() {
            break;
        }
    }
    None
}

#[cfg(feature = "python")]
fn collect_python_imports(node: Node<'_>, src: &str, imports: &mut BTreeSet<String>) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "dotted_name" || curr.kind() == "identifier" {
            let text = node_text(curr, src).trim();
            if let Some(leaf) = text.rsplit('.').next() {
                if !leaf.is_empty() {
                    imports.insert(leaf.to_owned());
                }
            }
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(feature = "python")]
fn collect_python_calls(
    node: Node<'_>,
    src: &str,
    enclosing_class: Option<&str>,
    calls: &mut BTreeSet<String>,
) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "call" {
            if let Some(func) = curr.child_by_field_name("function") {
                if let Some(callee) = extract_callee_name(func, src, enclosing_class) {
                    calls.insert(callee);
                }
            }
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

// ============================================================================
// TypeScript / JavaScript Extractor (`feature = "typescript"`)
// ============================================================================

#[cfg(feature = "typescript")]
fn extract_ts_file(root: Node<'_>, src: &str, lang: LanguageKind) -> ExtractedFile {
    let mut imports = BTreeSet::new();
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let (actual, exported) = if child.kind() == "export_statement" {
            (
                child.child_by_field_name("declaration").unwrap_or(child),
                true,
            )
        } else {
            (child, false)
        };

        if actual.kind() == "import_statement" {
            collect_ts_imports(actual, src, &mut imports);
        } else if let Some(sym) = extract_ts_item(child, actual, src, exported) {
            symbols.push(sym);
        }
    }

    ExtractedFile {
        language: lang,
        doc: None,
        imports: imports.into_iter().collect(),
        symbols,
    }
}

#[cfg(feature = "typescript")]
fn extract_ts_item(
    outer: Node<'_>,
    node: Node<'_>,
    src: &str,
    exported: bool,
) -> Option<ExtractedSymbol> {
    let kind_str = match node.kind() {
        "class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type_alias",
        "enum_declaration" => "enum",
        "function_declaration" => "fn",
        _ => return None,
    };

    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, src).trim().to_owned();
    let visibility = if exported {
        "export".to_owned()
    } else {
        "private".to_owned()
    };
    let doc = collect_leading_doc_comments(outer, src);
    let line_span = [outer.start_position().row + 1, outer.end_position().row + 1];
    let signature = extract_signature_before_body(node, src, b'{');

    let mut calls = BTreeSet::new();
    let mut type_refs = BTreeSet::new();
    let mut children = Vec::new();

    if let Some(body) = node.child_by_field_name("body") {
        match node.kind() {
            "class_declaration" => {
                let mut cursor = body.walk();
                for member in body.children(&mut cursor) {
                    if member.kind() == "method_definition" {
                        if let Some(method) = extract_ts_method(member, src, Some(name.as_str())) {
                            children.push(method);
                        }
                    }
                }
            }
            "function_declaration" => {
                collect_ts_calls_and_types(body, src, None, &mut calls, &mut type_refs);
            }
            _ => {
                collect_ts_types_only(body, src, &mut type_refs);
            }
        }
    }

    type_refs.remove(&name);
    calls.remove(&name);

    Some(ExtractedSymbol {
        name,
        kind: kind_str,
        visibility,
        signature,
        doc,
        line_span,
        code: node_text(outer, src).to_owned(),
        children,
        calls: calls.into_iter().collect(),
        type_refs: type_refs.into_iter().collect(),
    })
}

#[cfg(feature = "typescript")]
fn extract_ts_method(
    node: Node<'_>,
    src: &str,
    enclosing_class: Option<&str>,
) -> Option<ExtractedSymbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, src).trim().to_owned();
    let doc = collect_leading_doc_comments(node, src);
    let line_span = [node.start_position().row + 1, node.end_position().row + 1];
    let signature = extract_signature_before_body(node, src, b'{');

    let mut calls = BTreeSet::new();
    let mut type_refs = BTreeSet::new();
    if let Some(body) = node.child_by_field_name("body") {
        collect_ts_calls_and_types(body, src, enclosing_class, &mut calls, &mut type_refs);
    }
    calls.remove(&name);
    if let Some(cls) = enclosing_class {
        calls.remove(&format!("{}::{}", cls, name));
    }

    Some(ExtractedSymbol {
        name,
        kind: "method",
        visibility: "public".to_owned(),
        signature,
        doc,
        line_span,
        code: node_text(node, src).to_owned(),
        children: Vec::new(),
        calls: calls.into_iter().collect(),
        type_refs: type_refs.into_iter().collect(),
    })
}

#[cfg(feature = "typescript")]
fn collect_ts_imports(node: Node<'_>, src: &str, imports: &mut BTreeSet<String>) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "identifier" {
            let ident = node_text(curr, src).trim();
            if !ident.is_empty() {
                imports.insert(ident.to_owned());
            }
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(feature = "typescript")]
fn collect_ts_calls_and_types(
    node: Node<'_>,
    src: &str,
    enclosing_class: Option<&str>,
    calls: &mut BTreeSet<String>,
    type_refs: &mut BTreeSet<String>,
) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        match curr.kind() {
            "call_expression" => {
                if let Some(func) = curr.child_by_field_name("function") {
                    if let Some(callee) = extract_callee_name(func, src, enclosing_class) {
                        calls.insert(callee);
                    }
                }
            }
            "type_identifier" => {
                let ty = node_text(curr, src).trim();
                if !ty.is_empty() {
                    type_refs.insert(ty.to_owned());
                }
            }
            _ => {}
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

#[cfg(feature = "typescript")]
fn collect_ts_types_only(node: Node<'_>, src: &str, type_refs: &mut BTreeSet<String>) {
    let mut stack = vec![node];
    while let Some(curr) = stack.pop() {
        if curr.kind() == "type_identifier" {
            let ty = node_text(curr, src).trim();
            if !ty.is_empty() {
                type_refs.insert(ty.to_owned());
            }
        }
        let mut cursor = curr.walk();
        for child in curr.children(&mut cursor) {
            stack.push(child);
        }
    }
}

// ============================================================================
// Shared CST Utilities
// ============================================================================

#[inline]
fn node_text<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn extract_signature_before_body(node: Node<'_>, src: &str, body_delim: u8) -> String {
    let text = node_text(node, src);
    let cut = text
        .as_bytes()
        .iter()
        .position(|&b| b == body_delim || b == b';')
        .unwrap_or(text.len());
    text[..cut].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_leading_doc_comments(node: Node<'_>, src: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(sib) = prev {
        match sib.kind() {
            "line_comment" => {
                let raw = node_text(sib, src).trim();
                if let Some(doc_line) = raw.strip_prefix("///") {
                    lines.push(doc_line.trim().to_owned());
                } else {
                    break;
                }
            }
            "block_comment" | "comment" => {
                let raw = node_text(sib, src).trim();
                if raw.starts_with("/**") {
                    let cleaned = raw
                        .trim_start_matches("/**")
                        .trim_end_matches("*/")
                        .lines()
                        .map(|l| l.trim().trim_start_matches('*').trim())
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !cleaned.is_empty() {
                        lines.push(cleaned);
                    }
                } else {
                    break;
                }
            }
            "attribute_item" => {
                // Skip `#[...]` attributes between doc comments and the item
            }
            _ => break,
        }
        prev = sib.prev_sibling();
    }

    if lines.is_empty() {
        None
    } else {
        lines.reverse();
        Some(lines.join("\n"))
    }
}

/// Extracts a receiver-qualified callee key where available:
/// - `SfgnBuilder::new(...)` -> `"SfgnBuilder::new"`
/// - `self.check_expiry(...)` / `this.checkExpiry(...)` inside `AuthService` -> `"AuthService::check_expiry"`
/// - `helper_fn(...)` or `codec.decode(...)` -> `"helper_fn"` or `"decode"` (unless ubiquitous stdlib name)
fn extract_callee_name(
    func_node: Node<'_>,
    src: &str,
    enclosing_type: Option<&str>,
) -> Option<String> {
    let raw = node_text(func_node, src).trim();
    if raw.is_empty() {
        return None;
    }

    // 1. Path-qualified call: `Type::method` or `module::func`
    if let Some((prefix, method_part)) = raw.rsplit_once("::") {
        let method = method_part.split('<').next().unwrap_or(method_part).trim();
        let owner = prefix
            .rsplit("::")
            .next()
            .unwrap_or(prefix)
            .split('<')
            .next()
            .unwrap_or(prefix)
            .trim();
        if method.is_empty() {
            return None;
        }
        if owner == "Self" {
            if let Some(ty) = enclosing_type {
                return Some(format!("{}::{}", ty, method));
            }
        }
        if owner
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
        {
            return Some(format!("{}::{}", owner, method));
        }
        if is_ubiquitous_method_name(method) {
            return None;
        }
        return Some(method.to_owned());
    }

    // 2. Receiver field/method call: `receiver.method`
    if let Some((receiver_part, method_part)) = raw.rsplit_once('.') {
        let method = method_part.split('<').next().unwrap_or(method_part).trim();
        let receiver = receiver_part.trim();
        if method.is_empty() {
            return None;
        }
        if matches!(receiver, "self" | "this") {
            if let Some(ty) = enclosing_type {
                return Some(format!("{}::{}", ty, method));
            }
        }
        // Handle Python `@staticmethod`/`@classmethod` and TS/JS `ClassName.staticMethod()`
        let leaf_receiver = receiver.rsplit('.').next().unwrap_or(receiver).trim();
        if leaf_receiver
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
        {
            return Some(format!("{}::{}", leaf_receiver, method));
        }
        if is_ubiquitous_method_name(method) {
            return None;
        }
        return Some(method.to_owned());
    }

    // 3. Direct function call: `foo(...)`
    let ident = raw.split('<').next().unwrap_or(raw).trim();
    if ident.is_empty() || is_ubiquitous_method_name(ident) {
        None
    } else {
        Some(ident.to_owned())
    }
}

/// Filters bare receiverless method names that belong to standard library traits/collections
/// and would otherwise create false-positive fanout without full type inference.
fn is_ubiquitous_method_name(name: &str) -> bool {
    matches!(
        name,
        "new"
            | "default"
            | "from"
            | "into"
            | "fmt"
            | "drop"
            | "clone"
            | "len"
            | "is_empty"
            | "as_str"
            | "as_ref"
            | "to_owned"
            | "to_string"
            | "collect"
            | "iter"
            | "next"
            | "ok"
            | "err"
            | "unwrap"
            | "expect"
            | "push"
            | "insert"
            | "get"
            | "contains"
            | "starts_with"
            | "ends_with"
            | "trim"
            | "split"
            | "join"
            | "lines"
            | "bytes"
            | "chars"
            | "with_capacity"
            | "parent"
            | "path"
            | "id"
            | "name"
            | "kind"
            | "loc"
            | "source"
            | "root"
            | "node"
            | "nodes"
            | "children"
            | "relatives"
            | "properties"
            | "decode"
            | "raw"
            | "end"
            | "start"
            | "read"
            | "write"
            | "parse"
            | "append"
            | "extend"
            | "items"
            | "keys"
            | "values"
            | "forEach"
            | "map"
            | "filter"
            | "reduce"
            | "stringify"
            | "log"
    )
}
