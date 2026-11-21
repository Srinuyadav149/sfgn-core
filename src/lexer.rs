use crate::error::{Diagnostic, ErrorCode, LineCol, Result};
use crate::tape::Span;

/// Document-wide Depth 1 indentation state machine (Spec § 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndentState {
    /// No non-blank, non-comment Depth 1 line has been encountered yet.
    #[default]
    Unset,
    /// Locked to a single leading `\t` (`0x09`) per Depth 1 line.
    LockedTab,
    /// Locked to `N` (`N >= 2`) leading `' '` (`0x20`) bytes per Depth 1 line.
    LockedSpaces(usize),
}

/// Validates raw input bytes for UTF-8 encoding and rejects a leading UTF-8 BOM (`0xEF 0xBB 0xBF`)
/// with `E001` (Spec § 2.1).
pub fn validate_utf8_no_bom(bytes: &[u8]) -> Result<&str> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(Diagnostic::new(
            ErrorCode::E001,
            LineCol::start(),
            0,
            "leading UTF-8 BOM (0xEF 0xBB 0xBF) is prohibited",
        ));
    }

    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(err) => {
            let valid_prefix = &bytes[..err.valid_up_to()];
            let loc = compute_line_col_from_valid_utf8(valid_prefix);
            Err(Diagnostic::new(
                ErrorCode::E001,
                loc,
                err.valid_up_to(),
                "invalid UTF-8 byte sequence",
            ))
        }
    }
}

fn compute_line_col_from_valid_utf8(prefix: &[u8]) -> LineCol {
    let mut line = 1usize;
    let mut col = 1usize;
    let mut i = 0usize;
    while i < prefix.len() {
        let b = prefix[i];
        if b == b'\n' {
            line += 1;
            col = 1;
            i += 1;
        } else if b == b'\r' {
            if prefix.get(i + 1) == Some(&b'\n') {
                line += 1;
                col = 1;
                i += 2;
            } else {
                col += 1;
                i += 1;
            }
        } else if b < 0x80 {
            col += 1;
            i += 1;
        } else {
            let width = utf8_char_width(b);
            col += 1;
            i += width;
        }
    }
    LineCol::new(line, col)
}

#[inline]
pub(crate) const fn utf8_char_width(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

/// Zero-copy UTF-8 cursor tracking byte offset, 1-based `LineCol`, and document-wide indentation state.
#[derive(Debug, Clone)]
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    pub indent: IndentState,
}

impl<'a> Lexer<'a> {
    /// Creates a new lexer over a UTF-8 string, rejecting a leading UTF-8 BOM (`\u{FEFF}`) with `E001`.
    pub fn new(src: &'a str) -> Result<Self> {
        if src.starts_with('\u{FEFF}') {
            return Err(Diagnostic::new(
                ErrorCode::E001,
                LineCol::start(),
                0,
                "leading UTF-8 BOM (0xEF 0xBB 0xBF) is prohibited",
            ));
        }

        Ok(Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            indent: IndentState::Unset,
        })
    }

    /// Creates a new lexer directly from raw bytes, validating UTF-8 and rejecting BOM (`E001`).
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self> {
        let src = validate_utf8_no_bom(bytes)?;
        Self::new(src)
    }

    #[inline]
    pub fn source(&self) -> &'a str {
        self.src
    }

    #[inline]
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn loc(&self) -> LineCol {
        LineCol::new(self.line, self.col)
    }

    #[inline]
    pub fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    #[inline]
    pub fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    #[inline]
    pub fn peek_at(&self, rel: usize) -> Option<u8> {
        self.bytes.get(self.pos + rel).copied()
    }

    #[inline]
    pub fn remaining_bytes(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    #[inline]
    pub fn remaining_str(&self) -> &'a str {
        &self.src[self.pos..]
    }

    #[inline]
    pub fn starts_with(&self, needle: &[u8]) -> bool {
        self.remaining_bytes().starts_with(needle)
    }

    #[inline]
    pub fn err(&self, code: ErrorCode, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, self.loc(), self.pos, message)
    }

    #[inline]
    pub fn err_at(
        &self,
        code: ErrorCode,
        loc: LineCol,
        offset: usize,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic::new(code, loc, offset, message)
    }

    #[inline]
    pub(crate) fn restore_same_line(&mut self, pos: usize, line: usize, col: usize) {
        self.pos = pos;
        self.line = line;
        self.col = col;
    }

    /// Advances by `n` ASCII non-newline bytes (`0x20..=0x7E` or `0x09`).
    #[inline]
    pub fn advance_ascii(&mut self, n: usize) {
        self.pos += n;
        self.col += n;
    }

    /// Advances by a single non-newline UTF-8 scalar value, rejecting illegal control bytes (`E001`).
    #[inline]
    pub fn advance_non_newline_char(&mut self) -> Result<()> {
        let b = self.bytes[self.pos];
        if is_illegal_control_byte(b) {
            return Err(self.err(
                ErrorCode::E001,
                format!("illegal ASCII control byte 0x{:02X}", b),
            ));
        }
        if b < 0x80 {
            self.pos += 1;
            self.col += 1;
        } else {
            self.pos += utf8_char_width(b);
            self.col += 1;
        }
        Ok(())
    }

    /// Consumes optional horizontal whitespace (`' '` `0x20` or `'\t'` `0x09`) and returns the byte count consumed.
    #[inline]
    pub fn skip_ows(&mut self) -> usize {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
                self.col += 1;
            } else {
                break;
            }
        }
        self.pos - start
    }

    /// Returns `true` if the cursor is currently at `\n`, `\r\n`, or EOF.
    #[inline]
    pub fn is_at_eol_or_eof(&self) -> bool {
        match self.peek() {
            None | Some(b'\n') => true,
            Some(b'\r') => self.peek_at(1) == Some(b'\n'),
            _ => false,
        }
    }

    /// Consumes a mandatory EOL (`\n` or `\r\n`) or EOF.
    /// Rejects standalone `\r` (`E001`) or unexpected trailing tokens (`trailing_err`).
    pub fn expect_eol_or_eof(&mut self, trailing_err: ErrorCode) -> Result<()> {
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.pos += 1;
                self.line += 1;
                self.col = 1;
                Ok(())
            }
            Some(b'\r') => {
                if self.peek_at(1) == Some(b'\n') {
                    self.pos += 2;
                    self.line += 1;
                    self.col = 1;
                    Ok(())
                } else {
                    Err(self.err(
                        ErrorCode::E001,
                        "standalone carriage return (\\r) without \\n is illegal",
                    ))
                }
            }
            Some(b) => {
                if is_illegal_control_byte(b) {
                    Err(self.err(
                        ErrorCode::E001,
                        format!("illegal ASCII control byte 0x{:02X}", b),
                    ))
                } else {
                    Err(self.err(
                        trailing_err,
                        format!(
                            "unexpected trailing token '{}' before end of line",
                            b as char
                        ),
                    ))
                }
            }
        }
    }

    /// Consumes a mandatory EOL (`\n` or `\r\n`), rejecting EOF with `eof_err` and standalone `\r` with `E001`.
    pub fn expect_eol(&mut self, eof_err: ErrorCode, trailing_err: ErrorCode) -> Result<()> {
        if self.is_eof() {
            return Err(self.err(eof_err, "unexpected end of file before end of line"));
        }
        self.expect_eol_or_eof(trailing_err)
    }

    /// Consumes an optional inline comment (`[OWS] ["#" ...]`) followed by EOL or EOF.
    pub fn consume_optional_inline_comment_and_eol(
        &mut self,
        trailing_err: ErrorCode,
    ) -> Result<()> {
        self.skip_ows();
        if self.peek() == Some(b'#') {
            self.consume_comment_to_eol()?;
        }
        self.expect_eol_or_eof(trailing_err)
    }

    /// Consumes from `#` to immediately before `\n`, `\r\n`, or EOF, checking for illegal control bytes (`E001`).
    pub fn consume_comment_to_eol(&mut self) -> Result<()> {
        debug_assert_eq!(self.peek(), Some(b'#'));
        self.advance_ascii(1);
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            if b == b'\r' {
                if self.peek_at(1) == Some(b'\n') {
                    break;
                }
                return Err(self.err(
                    ErrorCode::E001,
                    "standalone carriage return (\\r) inside comment is illegal",
                ));
            }
            self.advance_non_newline_char()?;
        }
        Ok(())
    }

    /// Validates and locks Depth 1 indentation at column 0 (Spec § 2.2).
    ///
    /// Precondition: caller is at column 0 (`self.col == 1`), the line begins with `' '` or `'\t'`,
    /// and the line is NOT a blank line (horizontal whitespace alone before EOL is handled by caller).
    /// `is_comment` indicates whether the first non-whitespace byte on this line is `'#'`.
    pub fn consume_depth1_indent(&mut self, is_comment: bool) -> Result<()> {
        debug_assert_eq!(self.col, 1);

        match self.indent {
            IndentState::Unset => {
                if is_comment {
                    self.skip_ows();
                    return Ok(());
                }

                match self.peek() {
                    Some(b'\t') => {
                        self.advance_ascii(1);
                        match self.peek() {
                            Some(b' ') | Some(b'\t') => {
                                return Err(self.err(
                                    ErrorCode::E004,
                                    "initial tab indentation cannot be followed by extra spaces or tabs",
                                ));
                            }
                            Some(b) if is_illegal_control_byte(b) => {
                                return Err(self.err(
                                    ErrorCode::E001,
                                    format!("illegal ASCII control byte 0x{:02X}", b),
                                ));
                            }
                            _ => {}
                        }
                        self.indent = IndentState::LockedTab;
                        Ok(())
                    }
                    Some(b' ') => {
                        let start_loc = self.loc();
                        let start_pos = self.pos;
                        let mut count = 0usize;
                        while self.peek() == Some(b' ') {
                            self.advance_ascii(1);
                            count += 1;
                        }
                        if self.peek() == Some(b'\t') {
                            return Err(self.err(
                                ErrorCode::E004,
                                "initial indentation cannot mix spaces and tabs",
                            ));
                        }
                        if count < 2 {
                            return Err(self.err_at(
                                ErrorCode::E004,
                                start_loc,
                                start_pos,
                                "initial space indentation requires at least 2 spaces (found 1)",
                            ));
                        }
                        if let Some(b) = self.peek() {
                            if is_illegal_control_byte(b) {
                                return Err(self.err(
                                    ErrorCode::E001,
                                    format!("illegal ASCII control byte 0x{:02X}", b),
                                ));
                            }
                        }
                        self.indent = IndentState::LockedSpaces(count);
                        Ok(())
                    }
                    _ => unreachable!("consume_depth1_indent called without leading whitespace"),
                }
            }
            IndentState::LockedTab => {
                if self.peek() != Some(b'\t') {
                    return Err(self.err(
                        ErrorCode::E005,
                        "expected leading tab (\\t) to match locked document indentation",
                    ));
                }
                self.advance_ascii(1);
                match self.peek() {
                    Some(b' ') | Some(b'\t') => Err(self.err(
                        ErrorCode::E006,
                        "extra leading whitespace beyond locked tab indentation (Depth 2 is prohibited)",
                    )),
                    Some(b) if is_illegal_control_byte(b) => Err(self.err(
                        ErrorCode::E001,
                        format!("illegal ASCII control byte 0x{:02X}", b),
                    )),
                    _ => Ok(()),
                }
            }
            IndentState::LockedSpaces(expected) => {
                if self.peek() == Some(b'\t') {
                    return Err(self.err(
                        ErrorCode::E005,
                        format!(
                            "expected {} leading spaces to match locked indentation, found tab",
                            expected
                        ),
                    ));
                }
                let mut count = 0usize;
                while self.peek() == Some(b' ') && count < expected {
                    self.advance_ascii(1);
                    count += 1;
                }
                if count < expected {
                    return Err(self.err(
                        ErrorCode::E005,
                        format!(
                            "mismatched indentation: expected {} spaces, found {}",
                            expected, count
                        ),
                    ));
                }
                match self.peek() {
                    Some(b' ') | Some(b'\t') => Err(self.err(
                        ErrorCode::E006,
                        format!(
                            "extra leading whitespace beyond locked {} spaces (Depth 2 is prohibited)",
                            expected
                        ),
                    )),
                    Some(b) if is_illegal_control_byte(b) => Err(self.err(
                        ErrorCode::E001,
                        format!("illegal ASCII control byte 0x{:02X}", b),
                    )),
                    _ => Ok(()),
                }
            }
        }
    }

    /// Scans a double-quoted string `"..."` including its opening and closing quotes (Spec § 2.3).
    /// Validates escape sequences (`\"`, `\\`, `\n`, `\r`, `\t`, `\0`, `\u{XXXXXX}`) and rejects
    /// unescaped newlines (`E007`), invalid escapes (`E008`), and illegal control bytes (`E001`).
    pub fn scan_quoted_string(&mut self) -> Result<Span> {
        let start = self.pos;
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.advance_ascii(1);

        loop {
            match self.peek() {
                None | Some(b'\n') | Some(b'\r') => {
                    return Err(self.err(
                        ErrorCode::E007,
                        "unterminated quoted string before end of line or file",
                    ));
                }
                Some(b'"') => {
                    self.advance_ascii(1);
                    return Ok(Span::new(start, self.pos - start));
                }
                Some(b'\\') => {
                    self.scan_escape_sequence()?;
                }
                Some(_) => {
                    self.advance_non_newline_char()?;
                }
            }
        }
    }

    fn scan_escape_sequence(&mut self) -> Result<()> {
        let esc_loc = self.loc();
        let esc_pos = self.pos;
        debug_assert_eq!(self.peek(), Some(b'\\'));
        self.advance_ascii(1);

        match self.peek() {
            None | Some(b'\n') | Some(b'\r') => Err(self.err(
                ErrorCode::E007,
                "unterminated escape sequence in quoted string before end of line or file",
            )),
            Some(b'"' | b'\\' | b'n' | b'r' | b't' | b'0') => {
                self.advance_ascii(1);
                Ok(())
            }
            Some(b'u') => {
                self.advance_ascii(1);
                if self.peek() != Some(b'{') {
                    return Err(self.err_at(
                        ErrorCode::E008,
                        esc_loc,
                        esc_pos,
                        "expected '{' after \\u escape",
                    ));
                }
                self.advance_ascii(1);

                let mut digits = 0usize;
                let mut codepoint = 0u32;
                while let Some(b) = self.peek() {
                    if b == b'}' {
                        break;
                    }
                    let Some(hex_val) = decode_hex_digit(b) else {
                        return Err(self.err_at(
                            ErrorCode::E008,
                            esc_loc,
                            esc_pos,
                            "invalid hex digit in \\u{...} unicode escape",
                        ));
                    };
                    if digits == 6 {
                        return Err(self.err_at(
                            ErrorCode::E008,
                            esc_loc,
                            esc_pos,
                            "\\u{...} unicode escape exceeds 6 hex digits",
                        ));
                    }
                    codepoint = (codepoint << 4) | hex_val;
                    digits += 1;
                    self.advance_ascii(1);
                }

                if digits == 0 || self.peek() != Some(b'}') {
                    return Err(self.err_at(
                        ErrorCode::E008,
                        esc_loc,
                        esc_pos,
                        "malformed or unclosed \\u{...} unicode escape",
                    ));
                }
                self.advance_ascii(1);

                if char::from_u32(codepoint).is_none() {
                    return Err(self.err_at(
                        ErrorCode::E008,
                        esc_loc,
                        esc_pos,
                        format!(
                            "\\u{{{:X}}} is not a valid Unicode scalar value (surrogate or out of range)",
                            codepoint
                        ),
                    ));
                }
                Ok(())
            }
            Some(other) => {
                if is_illegal_control_byte(other) {
                    Err(self.err(
                        ErrorCode::E001,
                        format!("illegal control byte 0x{:02X} after backslash", other),
                    ))
                } else {
                    Err(self.err_at(
                        ErrorCode::E008,
                        esc_loc,
                        esc_pos,
                        format!("unrecognized escape sequence '\\{}'", other as char),
                    ))
                }
            }
        }
    }

    /// Scans an indexed identifier (`QuotedString { [OWS] "#" Digit { Digit } }`) used by both
    /// `NodeSegment` and `PropertyKey` (Spec § 2.4, § 5, Error E010).
    pub fn scan_indexed_identifier(&mut self, strict_no_comment: bool) -> Result<Span> {
        let str_span = self.scan_quoted_string()?;
        let start = str_span.start;
        let mut end = str_span.end();

        loop {
            let saved_pos = self.pos;
            let saved_col = self.col;
            let ws = self.skip_ows();

            if self.peek() == Some(b'#') {
                let hash_loc = self.loc();
                let hash_pos = self.pos;

                match self.peek_at(1) {
                    Some(b'0'..=b'9') => {
                        self.advance_ascii(1);
                        while let Some(b) = self.peek() {
                            if b.is_ascii_digit() {
                                self.advance_ascii(1);
                            } else {
                                break;
                            }
                        }
                        end = self.pos;
                    }
                    _ => {
                        if strict_no_comment || ws == 0 {
                            return Err(self.err_at(
                                ErrorCode::E010,
                                hash_loc,
                                hash_pos,
                                "ordinal index operator '#' must be followed by one or more ASCII digits",
                            ));
                        }
                        self.pos = saved_pos;
                        self.col = saved_col;
                        break;
                    }
                }
            } else {
                self.pos = saved_pos;
                self.col = saved_col;
                break;
            }
        }

        Ok(Span::new(start, end - start))
    }

    /// Scans a JSON-compatible SFGN numeric literal (Spec § 4.1, § 5) and returns its `Span`.
    /// Returns `None` if the syntax is not a valid SFGN number.
    pub fn scan_number(&mut self) -> Option<Span> {
        let start = self.pos;
        let saved_col = self.col;

        if self.peek() == Some(b'-') {
            self.advance_ascii(1);
        }

        match self.peek() {
            Some(b'0') => {
                self.advance_ascii(1);
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos = start;
                    self.col = saved_col;
                    return None;
                }
            }
            Some(b'1'..=b'9') => {
                self.advance_ascii(1);
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.advance_ascii(1);
                }
            }
            _ => {
                self.pos = start;
                self.col = saved_col;
                return None;
            }
        }

        if self.peek() == Some(b'.') {
            self.advance_ascii(1);
            let mut frac_digits = 0usize;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.advance_ascii(1);
                frac_digits += 1;
            }
            if frac_digits == 0 {
                self.pos = start;
                self.col = saved_col;
                return None;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.advance_ascii(1);
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.advance_ascii(1);
            }
            let mut exp_digits = 0usize;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.advance_ascii(1);
                exp_digits += 1;
            }
            if exp_digits == 0 {
                self.pos = start;
                self.col = saved_col;
                return None;
            }
        }

        Some(Span::new(start, self.pos - start))
    }

    /// Scans an opening `"""[FormatTag][OWS]EOL` multiline block header (Spec § 2.5, § 5, Error E014).
    /// Precondition: cursor is at `"""`.
    pub fn scan_multiline_header(&mut self) -> Result<Option<Span>> {
        debug_assert!(self.starts_with(b"\"\"\""));
        self.advance_ascii(3);

        let tag_span = match self.peek() {
            Some(b) if is_tag_start(b) => {
                let tag_start = self.pos;
                self.advance_ascii(1);
                while let Some(c) = self.peek() {
                    if is_tag_char(c) {
                        self.advance_ascii(1);
                    } else {
                        break;
                    }
                }
                Some(Span::new(tag_start, self.pos - tag_start))
            }
            _ => None,
        };

        self.skip_ows();

        if self.is_eof() {
            return Err(self.err(
                ErrorCode::E013,
                "unterminated multiline block: reached EOF on opening '\"\"\"' line",
            ));
        }

        self.expect_eol(ErrorCode::E013, ErrorCode::E014)?;
        Ok(tag_span)
    }
}

/// Returns `true` for illegal ASCII control bytes outside valid whitespace (`\t`, `\n`, `\r`).
/// Note: NUL (`0x00`) is illegal everywhere, including inside multiline blocks (Spec § 2.1).
#[inline]
pub const fn is_illegal_control_byte(b: u8) -> bool {
    (b <= 0x1F && b != b'\t' && b != b'\n' && b != b'\r') || b == 0x7F
}

#[inline]
pub const fn is_tag_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

#[inline]
pub const fn is_tag_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[inline]
const fn decode_hex_digit(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'f' => Some((b - b'a' + 10) as u32),
        b'A'..=b'F' => Some((b - b'A' + 10) as u32),
        _ => None,
    }
}
