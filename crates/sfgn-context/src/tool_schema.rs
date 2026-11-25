use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use sfgn_extract_docs::compile_ebnf_to_gbnf;

use crate::node::{GraphNode, RevealLevel};

/// Specification of a single leaf tool inside the `tools>:` SFGN hierarchy.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Full SFGN tool address (e.g. `"tools/memory/record_memory"`).
    pub address: &'static str,
    /// Parent branch address (e.g. `"tools/memory/"`).
    pub branch: &'static str,
    /// JSON `"action"` discriminator (e.g. `"record_memory"`).
    pub action_name: &'static str,
    /// GBNF non-terminal name (e.g. `"record-memory"`).
    pub gbnf_rule_name: &'static str,
    /// GBNF production body for this action's JSON object (starting with `"progress"`).
    pub gbnf_rule_body: &'static str,
    /// Usage instructions revealed when `inspect_tool` opens this leaf.
    pub instructions: &'static str,
    /// Structural schema with `<placeholder>` tokens.
    pub schema_template: &'static str,
    /// Concrete valid JSON example to prevent literal-vs-placeholder confusion.
    pub example_json: &'static str,
}

impl ToolSpec {
    pub fn to_graph_node(&self) -> GraphNode {
        let mut node = GraphNode::new(self.address, RevealLevel::Level2Leaf, "tool");
        node.add_str_prop("action_name", self.action_name);
        node.add_str_prop("instructions", self.instructions);
        node.add_raw_json_prop("schema", self.schema_template);
        node.add_raw_json_prop("example", self.example_json);
        node
    }
}

/// Outcome of `inspect_tool(tool_address)`.
#[derive(Debug, Clone)]
pub enum ToolInspectResult {
    /// Opened a Level 1 tool branch (`"tools/memory/"`) listing child leaf tools.
    Branch(GraphNode),
    /// Opened and unlocked a Level 2 leaf tool (`"tools/memory/record_memory"`).
    LeafUnlocked(GraphNode),
}

/// Unified `tools>:` SFGN registry managing progressive tool discovery and dynamic GBNF compilation.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    branches: BTreeMap<&'static str, &'static str>,
    specs: Vec<ToolSpec>,
    unlocked_actions: BTreeSet<String>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut branches = BTreeMap::new();
        branches.insert(
            "tools/fs/",
            "List directories, search workspace text, or create new files",
        );
        branches.insert(
            "tools/code/",
            "Follow cross-file code portals or patch inspected AST symbols",
        );
        branches.insert(
            "tools/docs/",
            "Search sections and tables across Markdown/config docs",
        );
        branches.insert(
            "tools/memory/",
            "Query or persist cross-session notes in sfgn-memories/",
        );
        branches.insert(
            "tools/verify/",
            "Run workspace verification (cargo check / cargo test)",
        );

        let specs = vec![
            ToolSpec {
                address: "tools/fs/list_dir",
                branch: "tools/fs/",
                action_name: "list_dir",
                gbnf_rule_name: "list-dir",
                gbnf_rule_body: r#"list-dir ::= "{\"progress\":" json-str ",\"action\":\"list_dir\",\"path\":" json-str "}""#,
                instructions: "List files and subdirectories inside a workspace directory path (e.g. \"src\" or \".\").",
                schema_template: r#"{"progress":"<done | next>","action":"list_dir","path":"<dir_path>"}"#,
                example_json: r#"{"progress":"Done: booted | Next: list src/","action":"list_dir","path":"src"}"#,
            },
            ToolSpec {
                address: "tools/fs/search_text",
                branch: "tools/fs/",
                action_name: "search_text",
                gbnf_rule_name: "search-text",
                gbnf_rule_body: r#"search-text ::= "{\"progress\":" json-str ",\"action\":\"search_text\",\"query\":" json-str ",\"path_prefix\":" opt-str "}""#,
                instructions: "Search for a literal substring across workspace text files, optionally scoped to path_prefix (or null).",
                schema_template: r#"{"progress":"<done | next>","action":"search_text","query":"<substring>","path_prefix":"<dir_or_null>"}"#,
                example_json: r#"{"progress":"Done: unlocked search_text | Next: find encode_flat_rel_path","action":"search_text","query":"encode_flat_rel_path","path_prefix":"src"}"#,
            },
            ToolSpec {
                address: "tools/fs/create_file",
                branch: "tools/fs/",
                action_name: "create_file",
                gbnf_rule_name: "create-file",
                gbnf_rule_body: r#"create-file ::= "{\"progress\":" json-str ",\"action\":\"create_file\",\"path\":" json-str ",\"code\":" json-str "}""#,
                instructions: "Create a new workspace file at path with full UTF-8 content and hydrate its SFGN graph.",
                schema_template: r#"{"progress":"<done | next>","action":"create_file","path":"<rel_file_path>","code":"<file_content>"}"#,
                example_json: r#"{"progress":"Done: inspected module | Next: create helper.rs","action":"create_file","path":"src/helper.rs","code":"pub fn ping() -> bool { true }\n"}"#,
            },
            ToolSpec {
                address: "tools/code/follow_ref",
                branch: "tools/code/",
                action_name: "follow_ref",
                gbnf_rule_name: "follow-ref",
                gbnf_rule_body: r#"follow-ref ::= "{\"progress\":" json-str ",\"action\":\"follow_ref\",\"target_path\":" json-str ",\"target_symbol\":" opt-str "}""#,
                instructions: "Jump directly along a >> relative or portal to inspect target_path (and optional target_symbol, or null).",
                schema_template: r#"{"progress":"<done | next>","action":"follow_ref","target_path":"<rel_file_path>","target_symbol":"<symbol_or_null>"}"#,
                example_json: r#"{"progress":"Done: found portal | Next: inspect src/lib.rs","action":"follow_ref","target_path":"src/lib.rs","target_symbol":null}"#,
            },
            ToolSpec {
                address: "tools/code/patch_symbol",
                branch: "tools/code/",
                action_name: "patch_symbol",
                gbnf_rule_name: "patch-symbol",
                gbnf_rule_body: r#"patch-symbol ::= "{\"progress\":" json-str ",\"action\":\"patch_symbol\",\"path\":" json-str ",\"symbol\":" json-str ",\"instruction\":" json-str "}""#,
                instructions: "Delegate a targeted symbol edit on disk to the code-synthesis sub-agent. You MUST inspect `<path>::<symbol>` via inspect_node first, then pass `<path>` (file only), `<symbol>` copied verbatim from the outline (e.g. \"fn truncate_single_line\"), and a 1-sentence `instruction`.",
                schema_template: r#"{"progress":"<done | next>","action":"patch_symbol","path":"<rel_file_path>","symbol":"<symbol_from_outline>","instruction":"<one_sentence_edit_instruction>"}"#,
                example_json: r#"{"progress":"Done: read fn add | Next: patch fn add","action":"patch_symbol","path":"src/math.rs","symbol":"fn add","instruction":"Return a.saturating_add(b) instead of a + b."}"#,
            },
            ToolSpec {
                address: "tools/docs/search_docs",
                branch: "tools/docs/",
                action_name: "search_docs",
                gbnf_rule_name: "search-docs",
                gbnf_rule_body: r#"search-docs ::= "{\"progress\":" json-str ",\"action\":\"search_docs\",\"query\":" json-str "}""#,
                instructions: "Search section headings and content across Markdown documentation files in the workspace.",
                schema_template: r#"{"progress":"<done | next>","action":"search_docs","query":"<search_terms>"}"#,
                example_json: r#"{"progress":"Done: unlocked search_docs | Next: search lazy hydration","action":"search_docs","query":"lazy hydration"}"#,
            },
            ToolSpec {
                address: "tools/memory/recall_memory",
                branch: "tools/memory/",
                action_name: "recall_memory",
                gbnf_rule_name: "recall-memory",
                gbnf_rule_body: r#"recall-memory ::= "{\"progress\":" json-str ",\"action\":\"recall_memory\",\"query\":" json-str "}""#,
                instructions: "Search persistent notes in sfgn-memories/ matching query.",
                schema_template: r#"{"progress":"<done | next>","action":"recall_memory","query":"<keywords>"}"#,
                example_json: r#"{"progress":"Done: unlocked recall_memory | Next: check existing notes","action":"recall_memory","query":"flat_path"}"#,
            },
            ToolSpec {
                address: "tools/memory/record_memory",
                branch: "tools/memory/",
                action_name: "record_memory",
                gbnf_rule_name: "record-memory",
                gbnf_rule_body: r#"record-memory ::= "{\"progress\":" json-str ",\"action\":\"record_memory\",\"category\":" json-str ",\"entry_type\":" json-str ",\"note\":" json-str ",\"anchor_file\":" opt-str ",\"anchor_symbol\":" opt-str "}""#,
                instructions: "Call {\"progress\":\"...\",\"action\":\"record_memory\", ...} directly to save a note in sfgn-memories/<category>.sfgn. Categories: architecture, invariants, bugs, notes. Entry types: summary, rule, insight.",
                schema_template: r#"{"progress":"<done | next>","action":"record_memory","category":"<category>","entry_type":"<entry_type>","note":"<concise_note>","anchor_file":"<path_or_null>","anchor_symbol":"<symbol_or_null>"}"#,
                example_json: r#"{"progress":"Done: inspected encode_flat_rel_path | Next: save summary note","action":"record_memory","category":"notes","entry_type":"summary","note":"Encodes relative path components separated by colons.","anchor_file":"src/flat_path.rs","anchor_symbol":"fn encode_flat_rel_path"}"#,
            },
            ToolSpec {
                address: "tools/verify/run_verify",
                branch: "tools/verify/",
                action_name: "run_verify",
                gbnf_rule_name: "run-verify",
                gbnf_rule_body: r#"run-verify ::= "{\"progress\":" json-str ",\"action\":\"run_verify\"}""#,
                instructions: "Run workspace verification (`cargo check --workspace`) to verify code edits compile cleanly.",
                schema_template: r#"{"progress":"<done | next>","action":"run_verify"}"#,
                example_json: r#"{"progress":"Done: patched symbol | Next: verify build","action":"run_verify"}"#,
            },
        ];

        Self {
            branches,
            specs,
            unlocked_actions: BTreeSet::new(),
        }
    }

    /// Returns the Level 0 root `tools>:` node listing only top-level tool branches.
    pub fn root_node(&self) -> GraphNode {
        let mut root = GraphNode::new("tools", RevealLevel::Level0Index, "folder");
        root.children = self.branches.keys().map(|b| (*b).to_owned()).collect();
        root
    }

    /// Inspects a tool branch (`"tools/memory/"`) or leaf tool (`"tools/memory/record_memory"`),
    /// unlocking the leaf tool in GBNF and `REVEALED TOOL NODES` if matched.
    pub fn inspect_tool(&mut self, raw_target: &str) -> Result<ToolInspectResult, String> {
        let trimmed = raw_target.trim().trim_end_matches(">:");
        let normalized = if trimmed.starts_with("tools/") {
            trimmed.to_owned()
        } else {
            format!("tools/{}", trimmed.trim_start_matches('/'))
        };

        let branch_cand = if normalized.ends_with('/') {
            normalized.clone()
        } else {
            format!("{}/", normalized)
        };

        if let Some(&summary) = self.branches.get(branch_cand.as_str()) {
            let mut node = GraphNode::new(&branch_cand, RevealLevel::Level1Outline, "branch");
            node.add_str_prop("summary", summary);
            node.children = self
                .specs
                .iter()
                .filter(|s| s.branch == branch_cand)
                .map(|s| s.address.to_owned())
                .collect();
            return Ok(ToolInspectResult::Branch(node));
        }

        let leaf_cand = normalized.trim_end_matches('/');
        if let Some(spec) = self.specs.iter().find(|s| s.address == leaf_cand) {
            self.unlocked_actions.insert(spec.action_name.to_owned());
            return Ok(ToolInspectResult::LeafUnlocked(spec.to_graph_node()));
        }

        if let Some((prefix, _)) = leaf_cand.rsplit_once('/') {
            let parent_branch = format!("{}/", prefix);
            if self.branches.contains_key(parent_branch.as_str()) {
                let valid_leaves: Vec<&str> = self
                    .specs
                    .iter()
                    .filter(|s| s.branch == parent_branch)
                    .map(|s| s.address)
                    .collect();
                return Err(format!(
                    "Unknown tool '{}'. Valid leaf tools in {}: {}",
                    raw_target,
                    parent_branch,
                    valid_leaves.join(", ")
                ));
            }
        }

        let valid_branches: Vec<&str> = self.branches.keys().copied().collect();
        Err(format!(
            "Unknown tool address '{}'. Valid tool branches under tools>: {}",
            raw_target,
            valid_branches.join(", ")
        ))
    }

    /// Returns `true` if `action_name` is a core primitive or has been unlocked via `inspect_tool`.
    pub fn is_action_unlocked(&self, action_name: &str) -> bool {
        matches!(action_name, "inspect_node" | "inspect_tool" | "finish")
            || self.unlocked_actions.contains(action_name)
    }

    /// Finds the `tools/<branch>/<name>` address for a locked action to generate a recovery hint.
    pub fn tool_address_for_action(&self, action_name: &str) -> Option<&'static str> {
        self.specs
            .iter()
            .find(|s| s.action_name == action_name)
            .map(|s| s.address)
    }

    /// Returns all currently unlocked leaf tool nodes for rendering in `REVEALED TOOL NODES`.
    pub fn revealed_tool_nodes(&self) -> Vec<GraphNode> {
        self.specs
            .iter()
            .filter(|s| self.unlocked_actions.contains(s.action_name))
            .map(ToolSpec::to_graph_node)
            .collect()
    }

    /// Compiles the active JSON action GBNF grammar from the 3 core primitives plus unlocked tools.
    pub fn compile_gbnf(&self) -> String {
        let mut ebnf = String::from("Action = \"inspect-node\" | \"inspect-tool\" | \"finish\"");
        for spec in &self.specs {
            if self.unlocked_actions.contains(spec.action_name) {
                let _ = write!(ebnf, " | \"{}\"", spec.gbnf_rule_name);
            }
        }
        ebnf.push_str(" ;\n");

        let compiled_action_rule = compile_ebnf_to_gbnf(&ebnf)
            .ok()
            .and_then(|gbnf| {
                gbnf.lines()
                    .find(|l| l.starts_with("action ::="))
                    .map(|l| l.replace('"', ""))
            })
            .unwrap_or_else(|| {
                let mut fallback = String::from("action ::= inspect-node | inspect-tool | finish");
                for spec in &self.specs {
                    if self.unlocked_actions.contains(spec.action_name) {
                        let _ = write!(fallback, " | {}", spec.gbnf_rule_name);
                    }
                }
                fallback
            });

        let mut gbnf = String::with_capacity(1280);
        gbnf.push_str("root ::= think-block action\n");
        gbnf.push_str("think-block ::= \"<think>\" ([^<] | \"<\" [^/])* \"</think>\\n\"\n");
        gbnf.push_str(&compiled_action_rule);
        gbnf.push('\n');
        gbnf.push_str(
            "inspect-node ::= \"{\\\"progress\\\":\" json-str \",\\\"action\\\":\\\"inspect_node\\\",\\\"address\\\":\" json-str \"}\"\n",
        );
        gbnf.push_str(
            "inspect-tool ::= \"{\\\"progress\\\":\" json-str \",\\\"action\\\":\\\"inspect_tool\\\",\\\"tool\\\":\" json-str \"}\"\n",
        );
        gbnf.push_str(
            "finish       ::= \"{\\\"progress\\\":\" json-str \",\\\"action\\\":\\\"finish\\\",\\\"summary\\\":\" json-str \"}\"\n",
        );

        for spec in &self.specs {
            if self.unlocked_actions.contains(spec.action_name) {
                gbnf.push_str(spec.gbnf_rule_body);
                gbnf.push('\n');
            }
        }

        gbnf.push_str(
            "json-str ::= \"\\\"\" ([^\"\\\\\\x00-\\x1F] | \"\\\\\" ([\"\\\\/bfnrt] | \"u\" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]))* \"\\\"\"\n",
        );
        gbnf.push_str("opt-str  ::= \"null\" | json-str\n");
        gbnf
    }
}
