use crate::error::{Diagnostic, ErrorCode, LineCol, Result};
use crate::lexer::{IndentState, Lexer, is_illegal_control_byte};
use crate::tape::{
    ByteRange, NodeEntry, PropertyEntry, PropertyKind, RelationEntry, RelationKind, Span,
    StructuralTape, TapeRange,
};

/// Single-pass `O(N)` linear scanner emitting a zero-allocation `StructuralTape`
/// with `O(1)` auxiliary stack space (Spec § 6.1).
pub fn scan_tape(src: &str) -> Result<StructuralTape> {
    let mut lexer = Lexer::new(src)?;
    scan_with_lexer(&mut lexer)
}

/// Scans raw bytes into a `StructuralTape`, validating UTF-8 and rejecting BOM (`E001`).
pub fn scan_tape_bytes(bytes: &[u8]) -> Result<StructuralTape> {
    let mut lexer = Lexer::from_bytes(bytes)?;
    scan_with_lexer(&mut lexer)
}

fn scan_with_lexer(lexer: &mut Lexer<'_>) -> Result<StructuralTape> {
    let len = lexer.bytes().len();
    let mut tape =
        StructuralTape::with_capacity((len / 64).max(4), (len / 32).max(8), (len / 48).max(8));

    let mut has_root = false;

    loop {
        if lexer.is_eof() {
            break;
        }

        if consume_col0_blank_or_comment(lexer)? {
            continue;
        }

        if lexer.is_eof() {
            break;
        }

        let node_start_byte = lexer.pos();
        let header_loc = lexer.loc();

        match lexer.peek() {
            Some(b'>') => {
                if lexer.peek_at(1) != Some(b':') {
                    let code = if has_root {
                        ErrorCode::E003
                    } else {
                        ErrorCode::E002
                    };
                    return Err(lexer.err(
                        code,
                        "expected root node header '>:' or quoted node header '\"...\":' at column 0",
                    ));
                }
                if has_root {
                    return Err(lexer.err(
                        ErrorCode::E002,
                        "duplicate root node '>:' declaration; only one '>:' block is permitted",
                    ));
                }
                has_root = true;
                lexer.advance_ascii(2);
                let path_span = Span::new(node_start_byte, 2);
                lexer.consume_optional_inline_comment_and_eol(ErrorCode::E009)?;

                let prop_start = tape.properties.len();
                let rel_start = tape.relations.len();
                scan_block_body(lexer, &mut tape)?;

                tape.nodes.push(NodeEntry {
                    path_span,
                    full_span: ByteRange::new(node_start_byte, lexer.pos()),
                    prop_range: TapeRange::new(prop_start, tape.properties.len() - prop_start),
                    rel_range: TapeRange::new(rel_start, tape.relations.len() - rel_start),
                    header_loc,
                });
            }
            Some(b'"') => {
                if !has_root {
                    return Err(lexer.err(
                        ErrorCode::E002,
                        "file must declare root block '>:' at column 0 before any other node block",
                    ));
                }

                let path_span = scan_node_header(lexer)?;

                let prop_start = tape.properties.len();
                let rel_start = tape.relations.len();
                scan_block_body(lexer, &mut tape)?;

                tape.nodes.push(NodeEntry {
                    path_span,
                    full_span: ByteRange::new(node_start_byte, lexer.pos()),
                    prop_range: TapeRange::new(prop_start, tape.properties.len() - prop_start),
                    rel_range: TapeRange::new(rel_start, tape.relations.len() - rel_start),
                    header_loc,
                });
            }
            Some(b' ' | b'\t') => {
                let code = if has_root {
                    ErrorCode::E003
                } else {
                    ErrorCode::E002
                };
                return Err(lexer.err(code, "unexpected indented line outside of a node block"));
            }
            Some(b) => {
                if is_illegal_control_byte(b) {
                    return Err(lexer.err(
                        ErrorCode::E001,
                        format!("illegal ASCII control byte 0x{:02X} at column 0", b),
                    ));
                }
                return Err(lexer.err(
                    ErrorCode::E003,
                    format!(
                        "unexpected token '{}' at column 0; expected '>:', '\"', '#', or blank line",
                        b as char
                    ),
                ));
            }
            None => break,
        }
    }

    if !has_root {
        return Err(lexer.err(
            ErrorCode::E002,
            "missing mandatory root node '>:' declaration",
        ));
    }

    Ok(tape)
}

fn consume_col0_blank_or_comment(lexer: &mut Lexer<'_>) -> Result<bool> {
    if is_blank_line_ahead(lexer)? {
        lexer.skip_ows();
        lexer.expect_eol_or_eof(ErrorCode::E003)?;
        return Ok(true);
    }

    if lexer.peek() == Some(b'#') {
        lexer.consume_comment_to_eol()?;
        lexer.expect_eol_or_eof(ErrorCode::E003)?;
        return Ok(true);
    }

    Ok(false)
}

fn is_blank_line_ahead(lexer: &Lexer<'_>) -> Result<bool> {
    let bytes = lexer.bytes();
    let mut i = lexer.pos();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' {
            i += 1;
        } else if b == b'\n' {
            return Ok(true);
        } else if b == b'\r' {
            if bytes.get(i + 1) == Some(&b'\n') {
                return Ok(true);
            }
            return Err(Diagnostic::new(
                ErrorCode::E001,
                LineCol::new(lexer.loc().line, lexer.loc().col + (i - lexer.pos())),
                i,
                "standalone carriage return (\\r) without \\n is illegal",
            ));
        } else if is_illegal_control_byte(b) {
            return Err(Diagnostic::new(
                ErrorCode::E001,
                LineCol::new(lexer.loc().line, lexer.loc().col + (i - lexer.pos())),
                i,
                format!("illegal ASCII control byte 0x{:02X}", b),
            ));
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn scan_node_header(lexer: &mut Lexer<'_>) -> Result<Span> {
    let path_span = scan_node_path(lexer, true)?;

    if lexer.peek() != Some(b':') {
        return Err(lexer.err(
            ErrorCode::E009,
            "malformed node header: expected ':' immediately after node path",
        ));
    }
    lexer.advance_ascii(1);

    lexer.consume_optional_inline_comment_and_eol(ErrorCode::E009)?;
    Ok(path_span)
}

fn scan_node_path(lexer: &mut Lexer<'_>, strict_no_comment: bool) -> Result<Span> {
    let first_seg = lexer.scan_indexed_identifier(strict_no_comment)?;
    let start = first_seg.start;
    let mut end = first_seg.end();

    loop {
        let saved_pos = lexer.pos();
        let saved_loc = lexer.loc();
        lexer.skip_ows();

        if lexer.peek() == Some(b'>') {
            lexer.advance_ascii(1);
            lexer.skip_ows();
            if lexer.peek() != Some(b'"') {
                return Err(lexer.err(
                    ErrorCode::E009,
                    "malformed node path: trailing '>' must be followed by a quoted segment",
                ));
            }
            let next_seg = lexer.scan_indexed_identifier(strict_no_comment)?;
            end = next_seg.end();
        } else {
            rewind_lexer_ws(lexer, saved_pos, saved_loc.col);
            break;
        }
    }

    Ok(Span::new(start, end - start))
}

#[inline]
fn rewind_lexer_ws(lexer: &mut Lexer<'_>, target_pos: usize, target_col: usize) {
    let delta = lexer.pos() - target_pos;
    if delta > 0 {
        let src = lexer.source();
        let indent = lexer.indent;
        let line = lexer.loc().line;
        *lexer = rebuilt_same_line_lexer(src, target_pos, line, target_col, indent);
    }
}

#[inline]
fn rebuilt_same_line_lexer<'a>(
    src: &'a str,
    pos: usize,
    line: usize,
    col: usize,
    indent: IndentState,
) -> Lexer<'a> {
    let mut l = Lexer::new(src).expect("already validated UTF-8 without BOM");
    l.indent = indent;
    l.restore_same_line(pos, line, col);
    l
}

fn scan_block_body(lexer: &mut Lexer<'_>, tape: &mut StructuralTape) -> Result<()> {
    loop {
        if lexer.is_eof() {
            break;
        }

        if is_blank_line_ahead(lexer)? {
            lexer.skip_ows();
            lexer.expect_eol_or_eof(ErrorCode::E003)?;
            continue;
        }

        match lexer.peek() {
            Some(b'"') | Some(b'>') => break,
            Some(b'#') => {
                lexer.consume_comment_to_eol()?;
                lexer.expect_eol_or_eof(ErrorCode::E003)?;
            }
            Some(b' ') | Some(b'\t') => {
                let is_comment = first_non_ws_byte(lexer) == Some(b'#');
                lexer.consume_depth1_indent(is_comment)?;
                scan_depth1_item(lexer, tape)?;
            }
            Some(b) => {
                if is_illegal_control_byte(b) {
                    return Err(lexer.err(
                        ErrorCode::E001,
                        format!("illegal ASCII control byte 0x{:02X} at column 0", b),
                    ));
                }
                return Err(lexer.err(
                    ErrorCode::E003,
                    format!(
                        "unexpected token '{}' at column 0; expected indented item or column-0 node header",
                        b as char
                    ),
                ));
            }
            None => break,
        }
    }

    Ok(())
}

#[inline]
fn first_non_ws_byte(lexer: &Lexer<'_>) -> Option<u8> {
    let bytes = lexer.bytes();
    let mut i = lexer.pos();
    while i < bytes.len() {
        let b = bytes[i];
        if b != b' ' && b != b'\t' {
            return Some(b);
        }
        i += 1;
    }
    None
}

fn scan_depth1_item(lexer: &mut Lexer<'_>, tape: &mut StructuralTape) -> Result<()> {
    let item_loc = lexer.loc();

    match lexer.peek() {
        Some(b'#') => {
            lexer.consume_comment_to_eol()?;
            lexer.expect_eol_or_eof(ErrorCode::E003)?;
            Ok(())
        }
        Some(b'>') => {
            let is_relative = if lexer.peek_at(1) == Some(b'>') {
                lexer.advance_ascii(2);
                true
            } else {
                lexer.advance_ascii(1);
                false
            };

            lexer.skip_ows();

            let (kind, target_span) = match lexer.peek() {
                Some(b'[') => {
                    let span = scan_target_array(lexer)?;
                    let kind = if is_relative {
                        RelationKind::RelativeArray
                    } else {
                        RelationKind::ChildArray
                    };
                    (kind, span)
                }
                Some(b'"') | Some(b'<') => {
                    let span = scan_node_ref(lexer, false)?;
                    let kind = if is_relative {
                        RelationKind::RelativeSingle
                    } else {
                        RelationKind::ChildSingle
                    };
                    (kind, span)
                }
                Some(b) => {
                    if is_illegal_control_byte(b) {
                        return Err(lexer.err(
                            ErrorCode::E001,
                            format!("illegal ASCII control byte 0x{:02X} in relation target", b),
                        ));
                    }
                    return Err(lexer.err(
                        ErrorCode::E009,
                        "expected quoted node reference, '<\"file.sfgn\">' portal, or '[...]' target array",
                    ));
                }
                None => {
                    return Err(lexer.err(
                        ErrorCode::E009,
                        "unexpected end of file after relation operator",
                    ));
                }
            };

            lexer.consume_optional_inline_comment_and_eol(ErrorCode::E009)?;

            tape.relations.push(RelationEntry {
                kind,
                target_span,
                loc: item_loc,
            });
            Ok(())
        }
        Some(b'"') => {
            let key_span = lexer.scan_indexed_identifier(true)?;

            if lexer.peek() != Some(b':') {
                return Err(lexer.err(
                    ErrorCode::E009,
                    "expected ':' immediately after property key",
                ));
            }
            lexer.advance_ascii(1);
            lexer.skip_ows();

            let (kind, val_span, tag_span) = scan_property_value(lexer)?;

            tape.properties.push(PropertyEntry {
                key_span,
                val_span,
                tag_span,
                kind,
                loc: item_loc,
            });
            Ok(())
        }
        Some(b) => {
            if is_illegal_control_byte(b) {
                return Err(lexer.err(
                    ErrorCode::E001,
                    format!("illegal ASCII control byte 0x{:02X}", b),
                ));
            }
            Err(lexer.err(
                ErrorCode::E009,
                format!(
                    "unexpected token '{}' at Depth 1; expected property key '\"...\"', '>', '>>', or '#'",
                    b as char
                ),
            ))
        }
        None => Ok(()),
    }
}

fn scan_node_ref(lexer: &mut Lexer<'_>, strict_no_comment: bool) -> Result<Span> {
    if lexer.peek() == Some(b'<') {
        scan_external_ref(lexer, strict_no_comment)
    } else {
        scan_node_path(lexer, strict_no_comment)
    }
}

fn scan_external_ref(lexer: &mut Lexer<'_>, strict_no_comment: bool) -> Result<Span> {
    let start = lexer.pos();
    debug_assert_eq!(lexer.peek(), Some(b'<'));
    lexer.advance_ascii(1);
    lexer.skip_ows();

    if lexer.peek() != Some(b'"') {
        return Err(lexer.err(
            ErrorCode::E015,
            "malformed external reference: expected double-quoted file path after '<'",
        ));
    }

    lexer.scan_quoted_string()?;
    lexer.skip_ows();

    if lexer.peek() != Some(b'>') {
        return Err(lexer.err(
            ErrorCode::E015,
            "malformed external reference: missing closing '>' after file path",
        ));
    }
    lexer.advance_ascii(1);
    let mut end = lexer.pos();

    let saved_pos = lexer.pos();
    let saved_col = lexer.loc().col;
    lexer.skip_ows();

    match lexer.peek() {
        Some(b'"') => {
            let sub_path = scan_node_path(lexer, strict_no_comment)?;
            end = sub_path.end();
        }
        Some(b'>') => {
            return Err(lexer.err(
                ErrorCode::E015,
                "invalid trailing path syntax after external reference: do not insert '>' before the target node segment",
            ));
        }
        _ => {
            rewind_lexer_ws(lexer, saved_pos, saved_col);
        }
    }

    Ok(Span::new(start, end - start))
}

fn scan_target_array(lexer: &mut Lexer<'_>) -> Result<Span> {
    let start = lexer.pos();
    debug_assert_eq!(lexer.peek(), Some(b'['));
    lexer.advance_ascii(1);
    lexer.skip_ows();

    if lexer.peek() == Some(b']') {
        lexer.advance_ascii(1);
        return Ok(Span::new(start, lexer.pos() - start));
    }

    loop {
        match lexer.peek() {
            None | Some(b'\n') | Some(b'\r') => {
                return Err(lexer.err(
                    ErrorCode::E012,
                    "unterminated target array '[' before end of line or file",
                ));
            }
            Some(b'[') | Some(b'{') => {
                return Err(lexer.err(
                    ErrorCode::E011,
                    "nested collection '[' or '{' is prohibited inside target array",
                ));
            }
            Some(b'"') | Some(b'<') => {
                scan_node_ref(lexer, true)?;
            }
            Some(b) => {
                if is_illegal_control_byte(b) {
                    return Err(lexer.err(
                        ErrorCode::E001,
                        format!("illegal ASCII control byte 0x{:02X} in target array", b),
                    ));
                }
                return Err(lexer.err(
                    ErrorCode::E015,
                    format!(
                        "invalid target '{}' in relation array; expected quoted node path or '<\"file.sfgn\">'",
                        b as char
                    ),
                ));
            }
        }

        lexer.skip_ows();

        match lexer.peek() {
            Some(b',') => {
                lexer.advance_ascii(1);
                lexer.skip_ows();
                if lexer.peek() == Some(b']') {
                    lexer.advance_ascii(1);
                    return Ok(Span::new(start, lexer.pos() - start));
                }
            }
            Some(b']') => {
                lexer.advance_ascii(1);
                return Ok(Span::new(start, lexer.pos() - start));
            }
            None | Some(b'\n') | Some(b'\r') => {
                return Err(lexer.err(
                    ErrorCode::E012,
                    "unterminated target array '[' before end of line or file",
                ));
            }
            Some(b'[') | Some(b'{') => {
                return Err(lexer.err(
                    ErrorCode::E011,
                    "nested collection '[' or '{' is prohibited inside target array",
                ));
            }
            Some(b) => {
                return Err(lexer.err(
                    ErrorCode::E012,
                    format!("expected ',' or ']' in target array, found '{}'", b as char),
                ));
            }
        }
    }
}

fn scan_property_value(lexer: &mut Lexer<'_>) -> Result<(PropertyKind, Span, Option<Span>)> {
    if lexer.starts_with(b"\"\"\"") {
        let tag_span = lexer.scan_multiline_header()?;
        let val_span = scan_multiline_body(lexer)?;
        Ok((PropertyKind::MultilineBlock, val_span, tag_span))
    } else if lexer.peek() == Some(b'[') {
        let val_span = scan_flat_collection(lexer)?;
        lexer.consume_optional_inline_comment_and_eol(ErrorCode::E009)?;
        Ok((PropertyKind::FlatCollection, val_span, None))
    } else {
        let val_span = scan_scalar(lexer, ErrorCode::E009)?;
        lexer.consume_optional_inline_comment_and_eol(ErrorCode::E009)?;
        Ok((PropertyKind::Scalar, val_span, None))
    }
}

fn scan_flat_collection(lexer: &mut Lexer<'_>) -> Result<Span> {
    let start = lexer.pos();
    debug_assert_eq!(lexer.peek(), Some(b'['));
    lexer.advance_ascii(1);
    lexer.skip_ows();

    if lexer.peek() == Some(b']') {
        lexer.advance_ascii(1);
        return Ok(Span::new(start, lexer.pos() - start));
    }

    loop {
        match lexer.peek() {
            None | Some(b'\n') | Some(b'\r') => {
                return Err(lexer.err(
                    ErrorCode::E012,
                    "unterminated collection '[' before end of line or file",
                ));
            }
            Some(b'[') | Some(b'{') => {
                return Err(lexer.err(
                    ErrorCode::E011,
                    "nested collection '[' or '{' is prohibited inside 1D property array",
                ));
            }
            _ => {
                scan_scalar(lexer, ErrorCode::E012)?;
            }
        }

        lexer.skip_ows();

        match lexer.peek() {
            Some(b',') => {
                lexer.advance_ascii(1);
                lexer.skip_ows();
                if lexer.peek() == Some(b']') {
                    lexer.advance_ascii(1);
                    return Ok(Span::new(start, lexer.pos() - start));
                }
            }
            Some(b']') => {
                lexer.advance_ascii(1);
                return Ok(Span::new(start, lexer.pos() - start));
            }
            None | Some(b'\n') | Some(b'\r') => {
                return Err(lexer.err(
                    ErrorCode::E012,
                    "unterminated collection '[' before end of line or file",
                ));
            }
            Some(b'[') | Some(b'{') => {
                return Err(lexer.err(
                    ErrorCode::E011,
                    "nested collection '[' or '{' is prohibited inside 1D property array",
                ));
            }
            Some(b) => {
                return Err(lexer.err(
                    ErrorCode::E012,
                    format!("expected ',' or ']' in collection, found '{}'", b as char),
                ));
            }
        }
    }
}

fn scan_scalar(lexer: &mut Lexer<'_>, invalid_err: ErrorCode) -> Result<Span> {
    if lexer.starts_with(b"\"\"\"") {
        return Err(lexer.err(
            ErrorCode::E011,
            "multiline block '\"\"\"' cannot appear inside a 1D collection",
        ));
    }

    match lexer.peek() {
        Some(b'"') => lexer.scan_quoted_string(),
        Some(b't') if lexer.starts_with(b"true") && is_scalar_delimiter(lexer.peek_at(4)) => {
            let start = lexer.pos();
            lexer.advance_ascii(4);
            Ok(Span::new(start, 4))
        }
        Some(b'f') if lexer.starts_with(b"false") && is_scalar_delimiter(lexer.peek_at(5)) => {
            let start = lexer.pos();
            lexer.advance_ascii(5);
            Ok(Span::new(start, 5))
        }
        Some(b'n') if lexer.starts_with(b"null") && is_scalar_delimiter(lexer.peek_at(4)) => {
            let start = lexer.pos();
            lexer.advance_ascii(4);
            Ok(Span::new(start, 4))
        }
        Some(b'-' | b'0'..=b'9') => {
            if let Some(span) = lexer.scan_number() {
                if is_scalar_delimiter(lexer.peek()) {
                    return Ok(span);
                }
            }
            Err(lexer.err(invalid_err, "invalid numeric literal"))
        }
        Some(b) => {
            if is_illegal_control_byte(b) {
                return Err(lexer.err(
                    ErrorCode::E001,
                    format!("illegal ASCII control byte 0x{:02X}", b),
                ));
            }
            Err(lexer.err(
                invalid_err,
                format!("invalid scalar value starting with '{}'", b as char),
            ))
        }
        None => Err(lexer.err(
            invalid_err,
            "unexpected end of file while expecting scalar value",
        )),
    }
}

#[inline]
const fn is_scalar_delimiter(next: Option<u8>) -> bool {
    matches!(
        next,
        None | Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'#')
    )
}

fn scan_multiline_body(lexer: &mut Lexer<'_>) -> Result<Span> {
    let payload_start = lexer.pos();

    loop {
        if lexer.is_eof() {
            return Err(lexer.err(
                ErrorCode::E013,
                "unterminated multiline block: reached EOF before closing '\"\"\"' line",
            ));
        }

        let line_start = lexer.pos();
        lexer.skip_ows();

        if lexer.starts_with(b"\"\"\"") {
            lexer.advance_ascii(3);
            lexer.consume_optional_inline_comment_and_eol(ErrorCode::E014)?;
            return Ok(Span::new(payload_start, line_start - payload_start));
        }

        loop {
            match lexer.peek() {
                None => {
                    return Err(lexer.err(
                        ErrorCode::E013,
                        "unterminated multiline block: reached EOF before closing '\"\"\"' line",
                    ));
                }
                Some(0x00) => {
                    return Err(lexer.err(
                        ErrorCode::E001,
                        "null byte (0x00) is illegal inside multiline block payload",
                    ));
                }
                Some(b'\n') => {
                    lexer.expect_eol_or_eof(ErrorCode::E001)?;
                    break;
                }
                Some(b'\r') => {
                    if lexer.peek_at(1) == Some(b'\n') {
                        lexer.expect_eol_or_eof(ErrorCode::E001)?;
                        break;
                    }
                    return Err(lexer.err(
                        ErrorCode::E001,
                        "standalone carriage return (\\r) inside multiline block is illegal",
                    ));
                }
                Some(b) if b < 0x80 => {
                    lexer.advance_ascii(1);
                }
                Some(_) => {
                    lexer.advance_non_newline_char()?;
                }
            }
        }
    }
}
