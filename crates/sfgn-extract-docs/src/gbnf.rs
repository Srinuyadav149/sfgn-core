use std::fmt::Write;

use crate::error::{DocsExtractError, Result};

/// Transpiles an EBNF grammar specification into a `llama.cpp`-compatible GBNF grammar
/// (`root ::= ...`) for constrained token decoding.
///
/// Supported EBNF constructs:
/// - Production rules (`RuleName = expr ;` or `RuleName ::= expr`)
/// - Alternation (`|`), grouping `(...)`, optional `[...]` -> `(...)?`, repetition `{...}` -> `(...)*`
/// - Quoted string terminals (`"..."` and `'...'`)
/// - Character ranges (`'a'..'z'`, `"0".."9"`) and hex byte literals (`#x09`, `0x0A`)
/// - Line (`//`, `#`) and block (`(* ... *)`, `/* ... */`) comments
pub fn compile_ebnf_to_gbnf(ebnf: &str) -> Result<String> {
    let stripped = strip_ebnf_comments(ebnf);
    let mut rules: Vec<(String, String)> = Vec::new();
    let mut current_lhs: Option<String> = None;
    let mut current_rhs = String::new();
    let mut current_line = 1usize;

    for (line_idx, raw_line) in stripped.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((lhs_raw, rhs_raw)) = split_rule_assignment(trimmed) {
            if let Some(prev_lhs) = current_lhs.take() {
                let gbnf_rhs = transpile_ebnf_expr(&current_rhs, current_line)?;
                rules.push((normalize_gbnf_rule_name(&prev_lhs), gbnf_rhs));
                current_rhs.clear();
            }

            let lhs = lhs_raw.trim();
            if lhs.is_empty() {
                return Err(DocsExtractError::Grammar {
                    line: line_no,
                    reason: "empty rule name before assignment".to_owned(),
                });
            }
            current_lhs = Some(lhs.to_owned());
            current_line = line_no;
            current_rhs.push_str(rhs_raw.trim());
        } else if current_lhs.is_some() {
            if !current_rhs.is_empty() {
                current_rhs.push(' ');
            }
            current_rhs.push_str(trimmed);
        } else {
            return Err(DocsExtractError::Grammar {
                line: line_no,
                reason: format!("expected EBNF rule assignment, found '{}'", trimmed),
            });
        }
    }

    if let Some(prev_lhs) = current_lhs.take() {
        let gbnf_rhs = transpile_ebnf_expr(&current_rhs, current_line)?;
        rules.push((normalize_gbnf_rule_name(&prev_lhs), gbnf_rhs));
    }

    if rules.is_empty() {
        return Err(DocsExtractError::Grammar {
            line: 1,
            reason: "no EBNF productions found".to_owned(),
        });
    }

    let mut out = String::new();
    if rules[0].0 != "root" {
        writeln!(out, "root ::= {}", rules[0].0).ok();
    }
    for (name, rhs) in &rules {
        writeln!(out, "{} ::= {}", name, rhs).ok();
    }
    Ok(out)
}

fn split_rule_assignment(line: &str) -> Option<(&str, &str)> {
    let mut in_quote: Option<char> = None;
    let mut prev_escape = false;
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let ch = bytes[i] as char;
        if let Some(q) = in_quote {
            if prev_escape {
                prev_escape = false;
            } else if ch == '\\' {
                prev_escape = true;
            } else if ch == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }

        if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
            i += 1;
            continue;
        }

        if line[i..].starts_with("::=") {
            return Some((&line[..i], &line[i + 3..]));
        }
        if ch == '=' {
            return Some((&line[..i], &line[i + 1..]));
        }
        i += 1;
    }
    None
}

fn transpile_ebnf_expr(raw_rhs: &str, line: usize) -> Result<String> {
    let rhs = raw_rhs.trim().trim_end_matches(';').trim();
    if rhs.is_empty() {
        return Err(DocsExtractError::Grammar {
            line,
            reason: "empty right-hand side in EBNF rule".to_owned(),
        });
    }

    let chars: Vec<char> = rhs.chars().collect();
    let mut out = String::with_capacity(rhs.len() + 16);
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('(') {
                out.push(' ');
            }
            i += 1;
            continue;
        }

        // Quoted terminal `'...'` or `"..."` (including `'a'..'z'` character ranges)
        if ch == '"' || ch == '\'' {
            let (lit, next_idx) = scan_quoted_literal(&chars, i, line)?;
            if next_idx + 2 < chars.len()
                && chars[next_idx] == '.'
                && chars[next_idx + 1] == '.'
                && (chars[next_idx + 2] == '"' || chars[next_idx + 2] == '\'')
            {
                let (end_lit, after_end) = scan_quoted_literal(&chars, next_idx + 2, line)?;
                let start_ch = lit.chars().next().unwrap_or(' ');
                let end_ch = end_lit.chars().next().unwrap_or(' ');
                write!(
                    out,
                    "[{}-{}]",
                    escape_gbnf_char(start_ch),
                    escape_gbnf_char(end_ch)
                )
                .ok();
                i = after_end;
            } else {
                write!(out, "\"{}\"", escape_gbnf_string(&lit)).ok();
                i = next_idx;
            }
            continue;
        }

        // Hex byte literal `#x09` or `0x0A`
        if (ch == '#' && i + 2 < chars.len() && (chars[i + 1] == 'x' || chars[i + 1] == 'X'))
            || (ch == '0' && i + 2 < chars.len() && (chars[i + 1] == 'x' || chars[i + 1] == 'X'))
        {
            let start = i + 2;
            let mut end = start;
            while end < chars.len() && chars[end].is_ascii_hexdigit() {
                end += 1;
            }
            if end == start {
                return Err(DocsExtractError::Grammar {
                    line,
                    reason: "invalid hex literal in EBNF expression".to_owned(),
                });
            }
            let hex_digits: String = chars[start..end].iter().collect();
            write!(out, "\"\\x{}\"", hex_digits.to_ascii_lowercase()).ok();
            i = end;
            continue;
        }

        // EBNF repetition `{ ... }` -> GBNF `( ... )*`
        if ch == '{' {
            out.push('(');
            i += 1;
            continue;
        }
        if ch == '}' {
            trim_trailing_space(&mut out);
            out.push_str(")*");
            i += 1;
            continue;
        }

        // EBNF optional `[ ... ]` -> GBNF `( ... )?` (unless it's a character class like `[a-z]`)
        if ch == '[' {
            if is_char_class_bracket(&chars, i) {
                while i < chars.len() && chars[i] != ']' {
                    out.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    out.push(']');
                    i += 1;
                }
            } else {
                out.push('(');
                i += 1;
            }
            continue;
        }
        if ch == ']' {
            trim_trailing_space(&mut out);
            out.push_str(")?");
            i += 1;
            continue;
        }

        // Grouping, alternation, quantifiers, concatenation comma
        match ch {
            '(' | '|' | '?' | '*' | '+' => {
                if ch == '|' && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push(ch);
                if ch == '|' {
                    out.push(' ');
                }
                i += 1;
            }
            ')' => {
                trim_trailing_space(&mut out);
                out.push(')');
                i += 1;
            }
            ',' => {
                // EBNF explicit concatenation comma becomes whitespace in GBNF
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-')
                {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                out.push_str(&normalize_gbnf_rule_name(&ident));
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }

    Ok(out.trim().to_owned())
}

fn scan_quoted_literal(chars: &[char], quote_idx: usize, line: usize) -> Result<(String, usize)> {
    let quote = chars[quote_idx];
    let mut out = String::new();
    let mut i = quote_idx + 1;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\\' && i + 1 < chars.len() {
            out.push(ch);
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if ch == quote {
            return Ok((out, i + 1));
        }
        out.push(ch);
        i += 1;
    }
    Err(DocsExtractError::Grammar {
        line,
        reason: "unterminated string literal in EBNF rule".to_owned(),
    })
}

fn is_char_class_bracket(chars: &[char], open_idx: usize) -> bool {
    let mut i = open_idx + 1;
    while i < chars.len() {
        match chars[i] {
            ']' => return i > open_idx + 1,
            '"' | '\'' | '{' | '(' | '|' | ' ' => return false,
            _ => i += 1,
        }
    }
    false
}

/// Converts `CamelCase` or `snake_case` EBNF non-terminals into GBNF `kebab-case`.
pub fn normalize_gbnf_rule_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    let mut prev_lower = false;
    for ch in raw.chars() {
        if ch == '_' || ch == '-' || ch.is_whitespace() {
            if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
            prev_lower = false;
        } else if ch.is_ascii_uppercase() {
            if prev_lower && !out.ends_with('-') {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out.trim_matches('-').to_owned()
}

fn trim_trailing_space(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
}

fn escape_gbnf_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                out.push('\\');
                out.push(next);
                chars.next();
                continue;
            }
        }
        if ch == '"' {
            out.push_str("\\\"");
        } else {
            out.push(ch);
        }
    }
    out
}

fn escape_gbnf_char(ch: char) -> String {
    match ch {
        '\\' => "\\\\".to_owned(),
        ']' => "\\]".to_owned(),
        '-' => "\\-".to_owned(),
        '\n' => "\\n".to_owned(),
        '\r' => "\\r".to_owned(),
        '\t' => "\\t".to_owned(),
        other => other.to_string(),
    }
}

fn strip_ebnf_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut in_quote: Option<char> = None;

    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = in_quote {
            out.push(ch);
            if ch == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if ch == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }

        if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
            out.push(ch);
            i += 1;
            continue;
        }

        // `(* ... *)` or `/* ... */` block comments
        if (ch == '(' && i + 1 < chars.len() && chars[i + 1] == '*')
            || (ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*')
        {
            let close_first = if ch == '(' { '*' } else { '*' };
            i += 2;
            while i + 1 < chars.len() {
                if chars[i] == close_first
                    && ((ch == '(' && chars[i + 1] == ')') || (ch == '/' && chars[i + 1] == '/'))
                {
                    i += 2;
                    break;
                }
                if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            continue;
        }

        // `//` line comments
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}
