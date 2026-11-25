use std::fmt::Write;

use sfgn_context::InspectError;

/// Structured diagnosis injected into `TurnHistory` and `Scratchpad` when a turn fails,
/// enabling LFM3 1.7B to self-correct on the immediate next turn.
#[derive(Debug, Clone)]
pub struct FailureDiagnosis {
    /// Concise 1-line failure summary stored in `=== HISTORY ===`.
    pub error_summary: String,
    /// Actionable recovery suggestion appended as `Hint:` in `=== HISTORY ===`.
    pub recovery_hint: Option<String>,
    /// Focused notice rendered in `=== ACTION REQUIRED: RECOVER FROM PREVIOUS ERROR ===`.
    pub recovery_banner: String,
}

/// Translates execution, token-budget, schema, and compiler failures into direct recovery hints.
#[derive(Debug, Default, Clone, Copy)]
pub struct RecoveryPlanner;

impl RecoveryPlanner {
    pub fn thinking_truncated() -> FailureDiagnosis {
        let hint = "Your <think> exceeded the token limit before closing </think>. Do NOT summarize History. Write ONE sentence (<20 words) inside <think>, close </think>, and output {\"progress\":\"...\",\"action\":\"...\"}.".to_owned();
        FailureDiagnosis {
            error_summary: "Output truncated inside <think> before emitting JSON action".to_owned(),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn malformed_json(raw_output: &str, parse_err: &str) -> FailureDiagnosis {
        let preview = truncate_single_line(raw_output, 80);
        let hint = "Close </think>, print a newline, and output ONE flat JSON object starting with \"progress\" and \"action\", e.g. {\"progress\":\"Done: none | Next: inspect src/\",\"action\":\"inspect_node\",\"address\":\"src/\"}.".to_owned();
        FailureDiagnosis {
            error_summary: format!("Malformed JSON action ({}): '{}'", parse_err, preview),
            recovery_hint: Some(hint.clone()),
            recovery_banner: format!(
                "Your previous output was not valid JSON ({}).\n{}",
                parse_err, hint
            ),
        }
    }

    pub fn missing_action_field() -> FailureDiagnosis {
        let hint =
            "Every JSON output must be an object with top-level \"progress\" and \"action\" string keys."
                .to_owned();
        FailureDiagnosis {
            error_summary: "JSON object missing required \"action\" key".to_owned(),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn missing_param(action_name: &str, param_name: &str) -> FailureDiagnosis {
        let hint = format!(
            "Action \"{}\" requires string field \"{}\". Copy the exact field names from its schema in REVEALED TOOL NODES.",
            action_name, param_name
        );
        FailureDiagnosis {
            error_summary: format!(
                "Action \"{}\" missing required field \"{}\"",
                action_name, param_name
            ),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn locked_tool(action_name: &str, tool_address: Option<&str>) -> FailureDiagnosis {
        let hint = match tool_address {
            Some(addr) => {
                let branch = addr
                    .rsplit_once('/')
                    .map(|(b, _)| format!("{}/", b))
                    .unwrap_or_else(|| "tools/".to_owned());
                format!(
                    "Action \"{}\" is locked. First call {{\"progress\":\"...\",\"action\":\"inspect_tool\",\"tool\":\"{}\"}} (then \"{}\") to unlock it.",
                    action_name, branch, addr
                )
            }
            None => format!(
                "Unknown action \"{}\". Valid built-in actions: inspect_node, inspect_tool, finish.",
                action_name
            ),
        };
        FailureDiagnosis {
            error_summary: format!("Attempted locked or unknown action \"{}\"", action_name),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn duplicate_target(primitive: &str, target: &str) -> FailureDiagnosis {
        let clean = target.trim();
        let hint = if primitive == "inspect_tool" {
            if clean.ends_with('/') {
                format!(
                    "\"{}\" is ALREADY loaded in ACTIVE SFGN NODES—do NOT call inspect_tool(\"{}\") again. Call inspect_tool on its child leaf tool listed under '>' (e.g. \"tools/verify/run_verify\").",
                    clean, clean
                )
            } else {
                let action_hint = clean.rsplit('/').next().unwrap_or(clean);
                format!(
                    "\"{}\" is ALREADY unlocked in REVEALED TOOL NODES—do NOT call inspect_tool(\"{}\") again. Call {{\"progress\":\"...\",\"action\":\"{}\", ...}} directly matching its schema.",
                    clean, clean, action_hint
                )
            }
        } else if !clean.contains("::") && !clean.ends_with('/') {
            format!(
                "\"{}\" is ALREADY loaded in ACTIVE SFGN NODES—do NOT call inspect_node(\"{}\") again. Inspect a specific child symbol using \"{}::<symbol>\" from its '>' list, or move to the next step.",
                clean, clean, clean
            )
        } else {
            format!(
                "\"{}\" is ALREADY loaded in ACTIVE SFGN NODES—do NOT call {}(\"{}\") again. Proceed with the next step or call `finish`.",
                clean, primitive, clean
            )
        };
        FailureDiagnosis {
            error_summary: format!("Already inspected \"{}\" (already loaded)", target),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn exact_duplicate_failure(action_summary: &str) -> FailureDiagnosis {
        let hint = "This exact call already failed earlier. Do NOT repeat it—follow the Hint from the earlier failure."
            .to_owned();
        FailureDiagnosis {
            error_summary: format!("Repeated previously failed call: {}", action_summary),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn patch_before_read(file_path: &str, symbol: &str) -> FailureDiagnosis {
        let target_addr = format!("{}::{}", file_path, symbol);
        let hint = format!(
            "Call {{\"progress\":\"...\",\"action\":\"inspect_node\",\"address\":\"{}\"}} first to load its current source payload before calling `patch_symbol`.",
            target_addr
        );
        FailureDiagnosis {
            error_summary: format!(
                "Cannot patch \"{}\" before inspecting its Level 2 payload",
                target_addr
            ),
            recovery_hint: Some(hint.clone()),
            recovery_banner: hint,
        }
    }

    pub fn inspect_node_failed(address: &str, err: &InspectError) -> FailureDiagnosis {
        let banner = match &err.hint {
            Some(h) => format!(
                "inspect_node(\"{}\") failed: {}\nHint: {}",
                address, err.message, h
            ),
            None => format!("inspect_node(\"{}\") failed: {}", address, err.message),
        };
        FailureDiagnosis {
            error_summary: err.message.clone(),
            recovery_hint: err.hint.clone(),
            recovery_banner: banner,
        }
    }

    pub fn inspect_tool_failed(tool: &str, err_msg: &str) -> FailureDiagnosis {
        FailureDiagnosis {
            error_summary: err_msg.to_owned(),
            recovery_hint: Some(err_msg.to_owned()),
            recovery_banner: format!("inspect_tool(\"{}\") failed: {}", tool, err_msg),
        }
    }

    pub fn tool_exec_failed(action_name: &str, err: &InspectError) -> FailureDiagnosis {
        let banner = match &err.hint {
            Some(h) => format!("{} failed: {}\nHint: {}", action_name, err.message, h),
            None => format!("{} failed: {}", action_name, err.message),
        };
        FailureDiagnosis {
            error_summary: err.message.clone(),
            recovery_hint: err.hint.clone(),
            recovery_banner: banner,
        }
    }

    pub fn verify_failed(cmd_display: &str, combined_output: &str) -> FailureDiagnosis {
        let mut diag_lines = Vec::new();
        for line in combined_output.lines() {
            let trimmed = line.trim();
            if trimmed.contains(": error")
                || trimmed.starts_with("error[")
                || trimmed.starts_with("error:")
                || trimmed.ends_with("... FAILED")
                || trimmed.contains("panicked at")
            {
                diag_lines.push(trimmed.to_owned());
                if diag_lines.len() >= 5 {
                    break;
                }
            }
        }

        if diag_lines.is_empty() {
            for line in combined_output
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(5)
            {
                diag_lines.push(line.to_owned());
            }
        }

        let top_diag = if diag_lines.is_empty() {
            "command exited with non-zero status".to_owned()
        } else {
            diag_lines.join(" | ")
        };

        let mut banner = String::new();
        let _ = writeln!(banner, "Verification (`{}`) failed:", cmd_display);
        for d in &diag_lines {
            let _ = writeln!(banner, "  - {}", d);
        }
        banner.push_str(
            "Inspect the failing symbol via `inspect_node` and fix it with `patch_symbol`.",
        );

        FailureDiagnosis {
            error_summary: format!(
                "Verification failed: {}",
                truncate_single_line(&top_diag, 180)
            ),
            recovery_hint: Some(format!(
                "Fix compiler diagnostic: {}",
                truncate_single_line(&top_diag, 220)
            )),
            recovery_banner: banner,
        }
    }
}

fn truncate_single_line(raw: &str, max_chars: usize) -> String {
    let single = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= max_chars {
        single
    } else {
        let prefix: String = single.chars().take(max_chars).collect();
        format!("{}...", prefix)
    }
}
