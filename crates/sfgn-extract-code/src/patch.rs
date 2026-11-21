use std::fs;
use std::path::Path;

use sfgn_core::{NodeView, Value};

use crate::error::{CodeExtractError, Result};

/// Splices an updated `"""` code payload directly back into the original source file on disk
/// using the node's `"source_file"` and `"line_span": [start, end]` properties (Doc 1 § 2.3, Doc 2 § 4)[span_12](start_span)[span_12](end_span)[span_13](start_span)[span_13](end_span).
pub fn patch_node_source(
    workspace_root: impl AsRef<Path>,
    node: NodeView<'_, '_>,
    new_body: &str,
) -> Result<()> {
    let root = workspace_root.as_ref();

    let source_rel = match node.property("source_file") {
        Some(prop) => match prop.decode()? {
            Value::Scalar(s) => match s.as_str() {
                Some(path_str) => path_str.to_owned(),
                None => {
                    return Err(CodeExtractError::Patch {
                        path: root.to_path_buf(),
                        reason: "\"source_file\" property is not a string".to_owned(),
                    });
                }
            },
            _ => {
                return Err(CodeExtractError::Patch {
                    path: root.to_path_buf(),
                    reason: "\"source_file\" property is not a scalar string".to_owned(),
                });
            }
        },
        None => {
            return Err(CodeExtractError::Patch {
                path: root.to_path_buf(),
                reason: "target node does not carry a \"source_file\" property".to_owned(),
            });
        }
    };

    let target_file = root.join(&source_rel);

    let (start_line, end_line) = match node.property("line_span") {
        Some(prop) => match prop.decode()? {
            Value::FlatCollection(items) if items.len() == 2 => {
                let start = items[0]
                    .as_number()
                    .and_then(|n| n.as_u64())
                    .map(|v| v as usize);
                let end = items[1]
                    .as_number()
                    .and_then(|n| n.as_u64())
                    .map(|v| v as usize);
                match (start, end) {
                    (Some(s), Some(e)) if s >= 1 && s <= e => (s, e),
                    _ => {
                        return Err(CodeExtractError::Patch {
                            path: target_file,
                            reason: "invalid \"line_span\" bounds (expected [start, end] with 1 <= start <= end)".to_owned(),
                        });
                    }
                }
            }
            _ => {
                return Err(CodeExtractError::Patch {
                    path: target_file,
                    reason: "\"line_span\" property must be a 2-element integer collection"
                        .to_owned(),
                });
            }
        },
        None => {
            return Err(CodeExtractError::Patch {
                path: target_file,
                reason: "target node does not carry a \"line_span\" property".to_owned(),
            });
        }
    };

    let original = fs::read_to_string(&target_file).map_err(|source| CodeExtractError::Io {
        path: target_file.clone(),
        source,
    })?;

    let patched = splice_line_span(&target_file, &original, start_line, end_line, new_body)?;

    fs::write(&target_file, patched.as_bytes()).map_err(|source| CodeExtractError::Io {
        path: target_file,
        source,
    })?;

    Ok(())
}

/// Replaces 1-based inclusive lines `start_line..=end_line` inside `original` with `new_body`,
/// preserving the file's dominant line ending (`\n` vs `\r\n`) and trailing newline behavior.
pub fn splice_line_span(
    path: &Path,
    original: &str,
    start_line: usize,
    end_line: usize,
    new_body: &str,
) -> Result<String> {
    let uses_crlf = original.contains("\r\n");
    let had_trailing_newline = original.ends_with('\n');

    let lines: Vec<&str> = original.lines().collect();
    if start_line == 0 || end_line > lines.len() || start_line > end_line {
        return Err(CodeExtractError::Patch {
            path: path.to_path_buf(),
            reason: format!(
                "line_span [{}, {}] out of bounds for file with {} lines",
                start_line,
                end_line,
                lines.len()
            ),
        });
    }

    let mut out_lines: Vec<&str> =
        Vec::with_capacity(lines.len() - (end_line - start_line + 1) + new_body.lines().count());

    out_lines.extend_from_slice(&lines[..(start_line - 1)]);
    for replacement_line in new_body.lines() {
        out_lines.push(replacement_line);
    }
    out_lines.extend_from_slice(&lines[end_line..]);

    let sep = if uses_crlf { "\r\n" } else { "\n" };
    let mut result = out_lines.join(sep);
    if (had_trailing_newline || new_body.ends_with('\n')) && !result.is_empty() {
        result.push_str(sep);
    }
    Ok(result)
}

