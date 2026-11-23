use std::fmt::Write;

use sfgn_llm::ChatMessage;

use crate::history::TurnHistory;
use crate::knowledge::WorkspaceGraph;
use crate::memory::MemoryStore;
use crate::persona::static_system_prompt;
use crate::scratchpad::Scratchpad;
use crate::tool_schema::ToolRegistry;

/// KV-cache-friendly prompt assembler ordering sections from static to volatile.
#[derive(Debug, Default, Clone, Copy)]
pub struct PromptAssembler;

impl PromptAssembler {
    pub fn build(
        goal: &str,
        graph: &WorkspaceGraph,
        memories: &MemoryStore,
        tools: &ToolRegistry,
        history: &TurnHistory,
        scratchpad: &Scratchpad,
    ) -> (Vec<ChatMessage>, String) {
        let mut user = String::with_capacity(3072);
        let _ = writeln!(user, "Goal: {}\n", goal.trim());

        if let Some(progress) = scratchpad.task_progress() {
            user.push_str("=== TASK PROGRESS (LATEST CHECKPOINT) ===\n");
            let _ = writeln!(user, "{}\n", progress);
        }

        // 1. Level 0 Roots
        user.push_str("=== LEVEL 0 ROOTS ===\n");
        if let Ok(ws_root) = graph.workspace_root_node() {
            user.push_str(&ws_root.render_sfgn());
            user.push('\n');
        }
        user.push_str(&memories.root_node().render_sfgn());
        user.push('\n');
        user.push_str(&tools.root_node().render_sfgn());
        user.push('\n');

        // 2. Unlocked Tool Leaf Schemas + Instructions + Examples
        let revealed_tools = tools.revealed_tool_nodes();
        if !revealed_tools.is_empty() {
            user.push_str(
                "=== REVEALED TOOL NODES (UNLOCKED — CALL THEIR \"action_name\" DIRECTLY) ===\n",
            );
            for tool_node in &revealed_tools {
                user.push_str(&tool_node.render_sfgn());
                user.push('\n');
            }
        }

        // 3. Turn History
        user.push_str("=== HISTORY (COMPLETED STEPS — DO NOT REPEAT) ===\n");
        user.push_str(&history.render(12));
        user.push('\n');

        // 4. Immediate Recovery Banner if the last turn failed
        if let Some(banner) = scratchpad.last_error_banner() {
            user.push_str("=== ACTION REQUIRED: RECOVER FROM PREVIOUS ERROR ===\n");
            user.push_str(banner.trim());
            user.push_str("\n\n");
        }

        // 5. Active SFGN Nodes
        let active: Vec<_> = scratchpad.active_nodes().collect();
        if !active.is_empty() {
            user.push_str("=== ACTIVE SFGN NODES ===\n");
            for node in active {
                let _ = writeln!(user, "# [{}]", node.level.label());
                user.push_str(&node.render_sfgn());
                user.push('\n');
            }
        }

        let messages = vec![
            ChatMessage::system(static_system_prompt()),
            ChatMessage::user(user.trim_end()),
        ];
        let gbnf = tools.compile_gbnf();
        (messages, gbnf)
    }
}
