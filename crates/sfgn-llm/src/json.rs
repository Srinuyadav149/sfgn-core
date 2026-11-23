use std::fmt::Write;

use crate::{LlmError, Result};

/// Minimal zero-dependency RFC 8259 JSON value representation.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            Self::Object(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn index(&self, idx: usize) -> Option<&JsonValue> {
        match self {
            Self::Array(items) => items.get(idx),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(raw) => raw.parse::<u64>().ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(raw) => raw.parse::<i64>().ok(),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            Self::Object(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }
}

/// Serializes a Rust string into an RFC 8259 double-quoted JSON string literal.
pub fn escape_json_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) <= 0x1F => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parses a complete UTF-8 JSON document into a [`JsonValue`].
pub fn parse_json(src: &str) -> Result<JsonValue> {
    let mut parser = JsonParser::new(src);
    let val = parser.parse_value()?;
    parser.skip_ws();
    if !parser.is_eof() {
        return Err(LlmError::Json(format!(
            "unexpected trailing token at byte {}",
            parser.pos
        )));
    }
    Ok(val)
}

struct JsonParser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue> {
        self.skip_ws();
        let Some(b) = self.peek() else {
            return Err(LlmError::Json("unexpected end of JSON input".to_owned()));
        };

        match b {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(JsonValue::String),
            b't' => self.expect_literal(b"true", JsonValue::Bool(true)),
            b'f' => self.expect_literal(b"false", JsonValue::Bool(false)),
            b'n' => self.expect_literal(b"null", JsonValue::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(LlmError::Json(format!(
                "unexpected JSON token '{}' at byte {}",
                other as char, self.pos
            ))),
        }
    }

    fn expect_literal(&mut self, lit: &[u8], value: JsonValue) -> Result<JsonValue> {
        if self.bytes[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(value)
        } else {
            Err(LlmError::Json(format!(
                "invalid JSON literal at byte {}",
                self.pos
            )))
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue> {
        self.pos += 1; // '{'
        self.skip_ws();
        let mut entries = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(entries));
        }

        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(LlmError::Json(format!(
                    "expected ':' after object key at byte {}",
                    self.pos
                )));
            }
            self.pos += 1;
            let val = self.parse_value()?;
            entries.push((key, val));

            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(entries));
                }
                _ => {
                    return Err(LlmError::Json(format!(
                        "expected ',' or '}}' in JSON object at byte {}",
                        self.pos
                    )));
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue> {
        self.pos += 1; // '['
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }

        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(items));
                }
                _ => {
                    return Err(LlmError::Json(format!(
                        "expected ',' or ']' in JSON array at byte {}",
                        self.pos
                    )));
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => {
                return Err(LlmError::Json(format!(
                    "invalid JSON number at byte {}",
                    start
                )));
            }
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err(LlmError::Json(format!(
                    "expected fraction digits in JSON number at byte {}",
                    start
                )));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(LlmError::Json(format!(
                    "expected exponent digits in JSON number at byte {}",
                    start
                )));
            }
        }
        Ok(JsonValue::Number(self.src[start..self.pos].to_owned()))
    }

    fn parse_string(&mut self) -> Result<String> {
        if self.peek() != Some(b'"') {
            return Err(LlmError::Json(format!(
                "expected '\"' at byte {}",
                self.pos
            )));
        }
        self.pos += 1;
        let mut out = String::new();
        let mut segment_start = self.pos;

        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' {
                out.push_str(&self.src[segment_start..self.pos]);
                self.pos += 1;
                return Ok(out);
            }
            if b == b'\\' {
                out.push_str(&self.src[segment_start..self.pos]);
                self.pos += 1;
                let Some(esc) = self.peek() else {
                    return Err(LlmError::Json("unterminated JSON string escape".to_owned()));
                };
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let code = self.parse_hex_u16()?;
                        if (0xD800..=0xDBFF).contains(&code)
                            && self.bytes[self.pos..].starts_with(b"\\u")
                        {
                            self.pos += 2;
                            let low = self.parse_hex_u16()?;
                            if (0xDC00..=0xDFFF).contains(&low) {
                                let combined = 0x10000
                                    + (((code as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                                if let Some(ch) = char::from_u32(combined) {
                                    out.push(ch);
                                }
                            }
                        } else if let Some(ch) = char::from_u32(code as u32) {
                            out.push(ch);
                        }
                    }
                    other => {
                        return Err(LlmError::Json(format!(
                            "invalid JSON escape '\\{}'",
                            other as char
                        )));
                    }
                }
                segment_start = self.pos;
                continue;
            }
            if b < 0x20 {
                return Err(LlmError::Json(format!(
                    "unescaped control byte 0x{:02X} in JSON string",
                    b
                )));
            }
            self.pos += 1;
        }

        Err(LlmError::Json(
            "unterminated JSON string literal".to_owned(),
        ))
    }

    fn parse_hex_u16(&mut self) -> Result<u16> {
        if self.pos + 4 > self.bytes.len() {
            return Err(LlmError::Json("incomplete \\uXXXX escape".to_owned()));
        }
        let hex_str = &self.src[self.pos..self.pos + 4];
        let val = u16::from_str_radix(hex_str, 16)
            .map_err(|_| LlmError::Json(format!("invalid hex in \\u{}", hex_str)))?;
        self.pos += 4;
        Ok(val)
    }
}
