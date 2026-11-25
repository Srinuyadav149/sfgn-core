use std::collections::HashSet;
use std::fmt::Write;
use std::process::Command;

use sfgn_context::{
    GraphNode, MemoryStore, RevealLevel, Scratchpad, ToolInspectResult, ToolRegistry, TurnHistory,
    WorkspaceGraph,
};
use sfgn_llm::{ChatMessage, JsonValue, LlmClient, TokenUsage, parse_json};

use crate::recovery::{FailureDiagnosis, RecoveryPlanner};

/// Outcome of dispatching a single parsed model action.
#[derive(Debug, Clone)]
pub enum DispatchStep {
    Ok {
        action_summary: String,
        progress: Option<String>,
        observation: String,
    },
    Failed {
        action_summary: String,
        progress: Option<String>,
        diagnosis: FailureDiagnosis,
    },
    Finished {
        progress: Option<String>,
        summary: String,
    },
}

/// Validates and executes core SFGN primitives and unlocked tools against the workspace graph.
pub struct ActionDispatcher {
    pub graph: WorkspaceGraph,
    pub memories: MemoryStore,
    pub tools: ToolRegistry,
    pub scratchpad: Scratchpad,
    pub history: TurnHistory,
    verify_command: Vec<String>,
    coder_client: LlmClient,
    pending_sub_usage: TokenUsage,
    recorded_memories: HashSet<String>,
}

impl ActionDispatcher {
    pub fn new(
        graph: WorkspaceGraph,
        memories: MemoryStore,
        tools: ToolRegistry,
        scratchpad: Scratchpad,
        history: TurnHistory,
        verify_command: Vec<String>,
        coder_client: LlmClient,
    ) -> Self {
        Self {
            graph,
            memories,
            tools,
            scratchpad,
            history,
            verify_command,
            coder_client,
            pending_sub_usage: TokenUsage::default(),
            recorded_memories: HashSet::new(),
        }
    }

    /// Drains token usage accumulated by any coder sub-agent calls during the last turn.
    pub fn take_sub_usage(&mut self) -> TokenUsage {
        std::mem::take(&mut self.pending_sub_usage)
    }

    /// Parses `raw_content` as a single JSON action object, validates `"progress"` + `"action"`,
    /// executes the target, and records the result into [`TurnHistory`] and [`Scratchpad`].
    pub fn dispatch(&mut self, turn: usize, raw_content: &str) -> DispatchStep {
        let json_slice = extract_json_object_slice(raw_content);
        let parsed = match parse_json(json_slice) {
            Ok(val) => val,
            Err(err) => {
                let diag = RecoveryPlanner::malformed_json(raw_content, &err.to_string());
                let action_summary = "invalid_json".to_owned();
                self.record_failure(turn, &action_summary, &diag);
                return DispatchStep::Failed {
                    action_summary,
                    progress: None,
                    diagnosis: diag,
                };
            }
        };

        let Some(action_name) = parsed.get("action").and_then(JsonValue::as_str) else {
            let diag = RecoveryPlanner::missing_action_field();
            let action_summary = "missing_action".to_owned();
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress: None,
                diagnosis: diag,
            };
        };

        let Some(progress_str) = parsed
            .get("progress")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            let diag = RecoveryPlanner::missing_param(action_name, "progress");
            let action_summary = format!("{}(...)", action_name);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress: None,
                diagnosis: diag,
            };
        };
        let progress = Some(progress_str.to_owned());

        if !self.tools.is_action_unlocked(action_name) {
            let tool_addr = self.tools.tool_address_for_action(action_name);
            let diag = RecoveryPlanner::locked_tool(action_name, tool_addr);
            let action_summary = format!("{}(...)", action_name);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        let step = match action_name {
            "inspect_node" => self.exec_inspect_node(turn, &parsed, progress.clone()),
            "inspect_tool" => self.exec_inspect_tool(turn, &parsed, progress.clone()),
            "finish" => self.exec_finish(turn, &parsed, progress.clone()),
            "list_dir" => self.exec_list_dir(turn, &parsed, progress.clone()),
            "search_text" => self.exec_search_text(turn, &parsed, progress.clone()),
            "create_file" => self.exec_create_file(turn, &parsed, progress.clone()),
            "follow_ref" => self.exec_follow_ref(turn, &parsed, progress.clone()),
            "patch_symbol" => self.exec_patch_symbol(turn, &parsed, progress.clone()),
            "search_docs" => self.exec_search_docs(turn, &parsed, progress.clone()),
            "recall_memory" => self.exec_recall_memory(turn, &parsed, progress.clone()),
            "record_memory" => self.exec_record_memory(turn, &parsed, progress.clone()),
            "run_verify" => self.exec_run_verify(turn, progress.clone()),
            other => {
                let diag = RecoveryPlanner::locked_tool(other, None);
                let action_summary = format!("{}(...)", other);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        };

        match &step {
            DispatchStep::Ok {
                progress: Some(p), ..
            }
            | DispatchStep::Finished {
                progress: Some(p), ..
            } => {
                self.scratchpad.set_task_progress(p);
            }
            _ => {}
        }

        step
    }

    fn exec_inspect_node(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(address) = required_str(payload, "address") else {
            return self.fail_missing_param(turn, "inspect_node", "address", progress);
        };
        let action_summary = format!("inspect_node(\"{}\")", address);

        if self.scratchpad.has_failed_before(&action_summary) {
            let diag = RecoveryPlanner::exact_duplicate_failure(&action_summary);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        let norm_check = address.trim().trim_end_matches(">:");
        if !norm_check.is_empty()
            && norm_check != "."
            && !norm_check.ends_with('/')
            && self.scratchpad.is_node_active(norm_check)
        {
            let diag = RecoveryPlanner::duplicate_target("inspect_node", address);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        match self.graph.inspect_node(address) {
            Ok(node) => {
                let obs = describe_inspected_node(&node);
                self.scratchpad.mark_target_visited(norm_check);
                self.scratchpad.push_node(node);
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::inspect_node_failed(address, &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_inspect_tool(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(tool) = required_str(payload, "tool") else {
            return self.fail_missing_param(turn, "inspect_tool", "tool", progress);
        };
        let action_summary = format!("inspect_tool(\"{}\")", tool);

        if self.scratchpad.has_failed_before(&action_summary) {
            let diag = RecoveryPlanner::exact_duplicate_failure(&action_summary);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        if self.scratchpad.has_visited_target(tool.trim()) {
            let diag = RecoveryPlanner::duplicate_target("inspect_tool", tool);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        match self.tools.inspect_tool(tool) {
            Ok(ToolInspectResult::Branch(node)) => {
                let obs = format!(
                    "Loaded Level 1 tool branch ({} tools: {})",
                    node.children.len(),
                    node.children.join(", ")
                );
                self.scratchpad.mark_target_visited(tool.trim());
                self.scratchpad.push_node(node);
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Ok(ToolInspectResult::LeafUnlocked(node)) => {
                let obs = format!(
                    "Unlocked tool '{}' — schema & instructions now active in REVEALED TOOL NODES",
                    node.address
                );
                self.scratchpad.mark_target_visited(tool.trim());
                self.scratchpad.evict_tool_branch(&node.address);
                self.scratchpad.clear_error_banner();
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err_msg) => {
                let diag = RecoveryPlanner::inspect_tool_failed(tool, &err_msg);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_finish(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(summary) = required_str(payload, "summary") else {
            return self.fail_missing_param(turn, "finish", "summary", progress);
        };
        let action_summary = "finish(...)".to_owned();
        self.scratchpad.clear_error_banner();
        self.history
            .push_ok(turn, &action_summary, "Completed goal");
        DispatchStep::Finished {
            progress,
            summary: summary.to_owned(),
        }
    }

    fn exec_list_dir(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(path) = required_str(payload, "path") else {
            return self.fail_missing_param(turn, "list_dir", "path", progress);
        };
        let action_summary = format!("list_dir(\"{}\")", path);

        match self.graph.list_dir(path) {
            Ok(entries) => {
                let addr = if path == "." || path.is_empty() {
                    "workspace".to_owned()
                } else {
                    format!("{}/", path.trim_end_matches('/'))
                };
                let mut node = GraphNode::new(addr, RevealLevel::Level0Index, "folder");
                node.children = entries.clone();
                let obs = format!("Listed {} entries in '{}'", entries.len(), path);
                self.scratchpad.push_node(node);
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::tool_exec_failed("list_dir", &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_search_text(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(query) = required_str(payload, "query") else {
            return self.fail_missing_param(turn, "search_text", "query", progress);
        };
        let path_prefix = optional_str(payload, "path_prefix");
        let action_summary = format!("search_text(\"{}\")", query);

        match self.graph.search_text(query, path_prefix) {
            Ok(hits) => {
                let mut node = GraphNode::new(
                    format!("search_text::{}", query),
                    RevealLevel::Level2Leaf,
                    "search_results",
                );
                node.add_num_prop("matches", hits.len().to_string());
                let body = if hits.is_empty() {
                    "(no matches)".to_owned()
                } else {
                    hits.join("\n")
                };
                node.set_payload(Some("text"), body);
                let obs = format!("Found {} matching lines for '{}'", hits.len(), query);
                self.scratchpad.push_node(node);
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::tool_exec_failed("search_text", &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_create_file(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(path) = required_str(payload, "path") else {
            return self.fail_missing_param(turn, "create_file", "path", progress);
        };
        let Some(code) = required_str(payload, "code") else {
            return self.fail_missing_param(turn, "create_file", "code", progress);
        };
        let action_summary = format!("create_file(\"{}\")", path);

        match self.graph.create_file(path, code) {
            Ok(obs) => {
                if let Ok(node) = self.graph.inspect_node(path) {
                    self.scratchpad.push_node(node);
                }
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::tool_exec_failed("create_file", &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_follow_ref(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(target_path) = required_str(payload, "target_path") else {
            return self.fail_missing_param(turn, "follow_ref", "target_path", progress);
        };
        let target_symbol = optional_str(payload, "target_symbol");
        let address = match target_symbol {
            Some(sym) if !sym.is_empty() => format!("{}::{}", target_path, sym),
            _ => target_path.to_owned(),
        };
        let action_summary = format!("follow_ref(\"{}\")", address);

        match self.graph.inspect_node(&address) {
            Ok(node) => {
                let obs = describe_inspected_node(&node);
                self.scratchpad.push_node(node);
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::inspect_node_failed(&address, &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_patch_symbol(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(raw_path) = required_str(payload, "path") else {
            return self.fail_missing_param(turn, "patch_symbol", "path", progress);
        };
        let Some(raw_symbol) = required_str(payload, "symbol") else {
            return self.fail_missing_param(turn, "patch_symbol", "symbol", progress);
        };
        let Some(instruction) = required_str(payload, "instruction") else {
            return self.fail_missing_param(turn, "patch_symbol", "instruction", progress);
        };

        let (path, symbol) = match raw_path.trim().split_once("::") {
            Some((file_part, sym_from_path)) => {
                let sym_clean = raw_symbol.trim();
                let resolved_sym = if sym_clean.is_empty() || sym_from_path.ends_with(sym_clean) {
                    sym_from_path.trim()
                } else {
                    sym_clean
                };
                (file_part.trim(), resolved_sym)
            }
            None => {
                let file_prefix = format!("{}::", raw_path.trim());
                let sym_clean = raw_symbol
                    .trim()
                    .strip_prefix(&file_prefix)
                    .unwrap_or(raw_symbol.trim());
                (raw_path.trim(), sym_clean)
            }
        };
        let action_summary = format!("patch_symbol(\"{}::{}\")", path, symbol);

        if !self.has_inspected_symbol(path, symbol) {
            let diag = RecoveryPlanner::patch_before_read(path, symbol);
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        let sym_addr = format!("{}::{}", path, symbol);
        let target_node = match self.graph.inspect_node(&sym_addr) {
            Ok(node) => node,
            Err(err) => {
                let diag = RecoveryPlanner::inspect_node_failed(&sym_addr, &err);
                self.record_failure(turn, &action_summary, &diag);
                return DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                };
            }
        };

        let generated_code =
            match self.synthesize_symbol_patch(path, symbol, instruction, &target_node) {
                Ok(code) => code,
                Err(diag) => {
                    self.record_failure(turn, &action_summary, &diag);
                    return DispatchStep::Failed {
                        action_summary,
                        progress,
                        diagnosis: diag,
                    };
                }
            };

        match self.graph.patch_symbol(path, symbol, &generated_code) {
            Ok(obs) => {
                if let Ok(updated_node) = self.graph.inspect_node(&target_node.address) {
                    self.scratchpad.push_node(updated_node);
                }
                let full_obs = format!("{} (via coder sub-agent)", obs);
                self.history.push_ok(turn, &action_summary, &full_obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: full_obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::tool_exec_failed("patch_symbol", &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn synthesize_symbol_patch(
        &mut self,
        path: &str,
        symbol: &str,
        instruction: &str,
        target_node: &GraphNode,
    ) -> std::result::Result<String, FailureDiagnosis> {
        let system_prompt = "\
You are a focused code-editing sub-agent.\n\
Given ONE target symbol and a concise edit instruction, output ONLY the complete replacement implementation inside a single fenced code block (e.g. ```rust ... ```).\n\
Rules:\n\
- Preserve exact visibility, name, signature, and attributes unless explicitly instructed otherwise.\n\
- Output the COMPLETE replacement symbol (not a diff).\n\
- Output ONLY the fenced code block with no surrounding explanation.";

        let mut user_prompt = String::with_capacity(2048);
        let _ = writeln!(user_prompt, "File: {}", path.trim());
        let _ = writeln!(user_prompt, "Target Symbol: {}", symbol.trim());
        let _ = writeln!(user_prompt, "Edit Instruction: {}\n", instruction.trim());
        user_prompt.push_str("=== TARGET SYMBOL ===\n");
        user_prompt.push_str(&target_node.render_sfgn());

        let related: Vec<&GraphNode> = self
            .scratchpad
            .active_nodes()
            .filter(|n| n.level == RevealLevel::Level2Leaf && n.address != target_node.address)
            .collect();
        if !related.is_empty() {
            user_prompt.push_str("\n=== RELATED CONTEXT NODES ===\n");
            for rel_node in related {
                user_prompt.push_str(&rel_node.render_sfgn());
            }
        }

        if let Some(banner) = self.scratchpad.last_error_banner() {
            user_prompt.push_str("\n=== PREVIOUS ERROR DIAGNOSTIC ===\n");
            user_prompt.push_str(banner.trim());
            user_prompt.push('\n');
        }

        let messages = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(user_prompt.trim_end()),
        ];

        let resp = self
            .coder_client
            .stream_chat(&messages, None, |_| {})
            .map_err(|err| FailureDiagnosis {
                error_summary: format!("Coder sub-agent LLM error: {}", err),
                recovery_hint: Some(
                    "Verify the LLM server is reachable and retry `patch_symbol`.".to_owned(),
                ),
                recovery_banner: format!("Coder sub-agent transport error: {}", err),
            })?;

        self.pending_sub_usage.prompt_tokens += resp.usage.prompt_tokens;
        self.pending_sub_usage.completion_tokens += resp.usage.completion_tokens;
        self.pending_sub_usage.total_tokens += resp.usage.total_tokens;
        self.pending_sub_usage.cached_tokens += resp.usage.cached_tokens;

        let extracted = extract_fenced_code(&resp.content);
        if extracted.trim().is_empty() {
            return Err(FailureDiagnosis {
                error_summary: "Coder sub-agent returned empty code block".to_owned(),
                recovery_hint: Some(
                    "Retry `patch_symbol` with a simpler, more direct 1-sentence `instruction`."
                        .to_owned(),
                ),
                recovery_banner:
                    "Coder sub-agent returned an empty response before emitting code. Simplify `instruction` and retry."
                        .to_owned(),
            });
        }

        Ok(extracted)
    }

    fn exec_search_docs(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(query) = required_str(payload, "query") else {
            return self.fail_missing_param(turn, "search_docs", "query", progress);
        };
        let action_summary = format!("search_docs(\"{}\")", query);

        match self.graph.search_docs(query) {
            Ok(hits) => {
                let mut node = GraphNode::new(
                    format!("search_docs::{}", query),
                    RevealLevel::Level2Leaf,
                    "doc_search_results",
                );
                node.add_num_prop("matches", hits.len().to_string());
                let body = if hits.is_empty() {
                    "(no matching doc sections)".to_owned()
                } else {
                    hits.join("\n")
                };
                node.set_payload(Some("text"), body);
                let obs = format!("Found {} doc matches for '{}'", hits.len(), query);
                self.scratchpad.push_node(node);
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Err(err) => {
                let diag = RecoveryPlanner::tool_exec_failed("search_docs", &err);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_recall_memory(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(query) = required_str(payload, "query") else {
            return self.fail_missing_param(turn, "recall_memory", "query", progress);
        };
        let action_summary = format!("recall_memory(\"{}\")", query);

        let entries = self.memories.recall(query);
        let mut node = GraphNode::new(
            format!("recall_memory::{}", query),
            RevealLevel::Level2Leaf,
            "memory_results",
        );
        node.add_num_prop("matches", entries.len().to_string());

        let mut body = String::new();
        if entries.is_empty() {
            body.push_str("(no matching memory entries)");
        } else {
            for e in &entries {
                let _ = writeln!(
                    body,
                    "[{}] ({}/{}) {} [file={:?}, symbol={:?}]",
                    e.id, e.category, e.entry_type, e.note, e.anchor_file, e.anchor_symbol
                );
            }
        }
        node.set_payload(Some("text"), body);

        let obs = format!("Recalled {} memory entries for '{}'", entries.len(), query);
        self.scratchpad.push_node(node);
        self.history.push_ok(turn, &action_summary, &obs);
        DispatchStep::Ok {
            action_summary,
            progress,
            observation: obs,
        }
    }

    fn exec_record_memory(
        &mut self,
        turn: usize,
        payload: &JsonValue,
        progress: Option<String>,
    ) -> DispatchStep {
        let Some(category) = required_str(payload, "category") else {
            return self.fail_missing_param(turn, "record_memory", "category", progress);
        };
        let Some(entry_type) = required_str(payload, "entry_type") else {
            return self.fail_missing_param(turn, "record_memory", "entry_type", progress);
        };
        let Some(note) = required_str(payload, "note") else {
            return self.fail_missing_param(turn, "record_memory", "note", progress);
        };
        let anchor_file = optional_str(payload, "anchor_file");
        let anchor_symbol = optional_str(payload, "anchor_symbol");
        let action_summary = format!("record_memory(\"{}/{}\")", category, entry_type);

        let dedup_key = format!(
            "{}:{}:{}:{}",
            category.trim(),
            entry_type.trim(),
            anchor_file.unwrap_or("").trim(),
            anchor_symbol.unwrap_or("").trim()
        );
        if self.recorded_memories.contains(&dedup_key) {
            let diag = FailureDiagnosis {
                error_summary: format!("Already recorded memory '{}'", dedup_key),
                recovery_hint: Some(
                    "Memory is ALREADY saved. Do NOT call record_memory again; call {\"progress\":\"Done: saved memory note | Next: finish\",\"action\":\"finish\",\"summary\":\"...\"} now."
                        .to_owned(),
                ),
                recovery_banner: "Memory entry was already recorded in an earlier turn. Call `finish` if all sub-tasks are done."
                    .to_owned(),
            };
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        }

        match self
            .memories
            .record(category, entry_type, note, anchor_file, anchor_symbol)
        {
            Ok(entry) => {
                self.recorded_memories.insert(dedup_key);
                let obs = format!(
                    "Recorded memory '{}' in sfgn-memories/{}.sfgn — memory step COMPLETE (do NOT repeat; call finish if no sub-tasks remain)",
                    entry.id, entry.category
                );
                self.scratchpad.clear_error_banner();
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress: Some(format!(
                        "Done: recorded {} in sfgn-memories/{}.sfgn | Next: finish",
                        entry.id, entry.category
                    )),
                    observation: obs,
                }
            }
            Err(err_msg) => {
                let diag = FailureDiagnosis {
                    error_summary: err_msg.clone(),
                    recovery_hint: Some(
                        "Ensure `note` is non-empty and `category` is one of: architecture, invariants, bugs, notes."
                            .to_owned(),
                    ),
                    recovery_banner: format!("record_memory failed: {}", err_msg),
                };
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn exec_run_verify(&mut self, turn: usize, progress: Option<String>) -> DispatchStep {
        let action_summary = "run_verify()".to_owned();
        let Some((program, args)) = self.verify_command.split_first() else {
            let diag = FailureDiagnosis {
                error_summary: "No verification command configured".to_owned(),
                recovery_hint: None,
                recovery_banner: "No verification command configured.".to_owned(),
            };
            self.record_failure(turn, &action_summary, &diag);
            return DispatchStep::Failed {
                action_summary,
                progress,
                diagnosis: diag,
            };
        };

        let cmd_display = self.verify_command.join(" ");
        match Command::new(program)
            .args(args)
            .current_dir(self.graph.root())
            .output()
        {
            Ok(output) if output.status.success() => {
                let obs = format!("Verification passed (`{}`)", cmd_display);
                self.scratchpad.clear_error_banner();
                self.history.push_ok(turn, &action_summary, &obs);
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation: obs,
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let combined = format!("{}\n{}", stderr, stdout);
                let diag = RecoveryPlanner::verify_failed(&cmd_display, &combined);
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
            Err(io_err) => {
                let diag = FailureDiagnosis {
                    error_summary: format!("Failed to spawn '{}': {}", cmd_display, io_err),
                    recovery_hint: Some("Verify cargo toolchain is installed in PATH.".to_owned()),
                    recovery_banner: format!("Failed to run `{}`: {}", cmd_display, io_err),
                };
                self.record_failure(turn, &action_summary, &diag);
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis: diag,
                }
            }
        }
    }

    fn fail_missing_param(
        &mut self,
        turn: usize,
        action_name: &str,
        param_name: &str,
        progress: Option<String>,
    ) -> DispatchStep {
        let diag = RecoveryPlanner::missing_param(action_name, param_name);
        let action_summary = format!("{}(...)", action_name);
        self.record_failure(turn, &action_summary, &diag);
        DispatchStep::Failed {
            action_summary,
            progress,
            diagnosis: diag,
        }
    }

    fn record_failure(&mut self, turn: usize, action_summary: &str, diag: &FailureDiagnosis) {
        self.scratchpad
            .record_failure(action_summary.to_owned(), diag.recovery_banner.clone());
        self.history.push_failed(
            turn,
            action_summary,
            &diag.error_summary,
            diag.recovery_hint.clone(),
        );
    }

    fn has_inspected_symbol(&self, rel_file: &str, symbol: &str) -> bool {
        let exact = format!("{}::{}", rel_file.trim(), symbol.trim());
        if self.scratchpad.has_visited_target(&exact) {
            return true;
        }
        const PREFIXES: &[&str] = &[
            "fn ",
            "struct ",
            "enum ",
            "trait ",
            "method ",
            "mod ",
            "type ",
            "const ",
            "static ",
            "macro ",
            "class ",
            "interface ",
        ];
        for prefix in PREFIXES {
            let candidate = format!("{}::{}{}", rel_file.trim(), prefix, symbol.trim());
            if self.scratchpad.has_visited_target(&candidate) {
                return true;
            }
        }
        false
    }
}

fn describe_inspected_node(node: &GraphNode) -> String {
    match node.level {
        RevealLevel::Level0Index => format!(
            "Loaded Level 0 index '{}' ({} children)",
            node.address,
            node.children.len()
        ),
        RevealLevel::Level1Outline => format!(
            "Loaded Level 1 outline '{}' ({} children, {} relatives)",
            node.address,
            node.children.len(),
            node.relatives.len()
        ),
        RevealLevel::Level2Leaf => format!("Loaded Level 2 leaf '{}'", node.address),
    }
}

fn required_str<'a>(obj: &'a JsonValue, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(JsonValue::as_str)
}

fn optional_str<'a>(obj: &'a JsonValue, key: &str) -> Option<&'a str> {
    match obj.get(key) {
        None | Some(JsonValue::Null) => None,
        Some(val) => val.as_str(),
    }
}

fn extract_json_object_slice(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start <= end {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

fn extract_fenced_code(raw: &str) -> String {
    let without_think = match raw.rfind("</think>") {
        Some(idx) => &raw[idx + "</think>".len()..],
        None => raw,
    };
    let trimmed = without_think.trim();

    if let Some(open_idx) = trimmed.find("```") {
        let after_open = &trimmed[open_idx + 3..];
        let body_start = after_open.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after_open[body_start..];
        if let Some(close_idx) = body.rfind("```") {
            return body[..close_idx].trim_end().to_owned();
        }
        return body.trim_end().to_owned();
    }

    trimmed.to_owned()
}
