use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sfgn_core::{NodeHandle, SfgnBuilder};
use sfgn_llm::{ChatMessage, Role, TurnResponse};

pub const LOGS_GRAPH_DIR: &str = "sfgn-logs";

#[derive(Debug, Clone)]
struct LoggedTurn {
    turn: usize,
    status: String,
    user_prompt: String,
    gbnf: String,
    progress: Option<String>,
    action_json: String,
    observation: String,
    thinking: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    elapsed_ms: u64,
}

/// Persists complete turn-by-turn agent audit traces (full input prompts, active GBNF grammar,
/// model `<think>` stream, `"progress"` checkpoint, JSON action, observation, and token stats)
/// in `sfgn-logs/<session_id>.sfgn`.
#[derive(Debug, Clone)]
pub struct SessionLogger {
    log_path: PathBuf,
    goal: String,
    system_prompt: Option<String>,
    turns: Vec<LoggedTurn>,
}

impl SessionLogger {
    pub fn new(workspace_root: impl AsRef<Path>, goal: impl Into<String>) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let session_id = format!("session-{}", ts);
        let log_path = workspace_root
            .as_ref()
            .join(LOGS_GRAPH_DIR)
            .join(format!("{}.sfgn", session_id));

        Self {
            log_path,
            goal: goal.into(),
            system_prompt: None,
            turns: Vec::new(),
        }
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub fn record_turn(
        &mut self,
        turn: usize,
        ok: bool,
        messages: &[ChatMessage],
        gbnf: &str,
        progress: Option<&str>,
        response: &TurnResponse,
        observation: &str,
    ) {
        if self.system_prompt.is_none() {
            let sys = messages
                .iter()
                .filter(|m| m.role == Role::System)
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !sys.is_empty() {
                self.system_prompt = Some(sys);
            }
        }

        let mut user_prompt = String::new();
        for msg in messages.iter().filter(|m| m.role != Role::System) {
            if !user_prompt.is_empty() {
                user_prompt.push_str("\n\n");
            }
            if msg.role == Role::Assistant {
                let _ = writeln!(user_prompt, "[assistant]\n{}", msg.content);
            } else {
                user_prompt.push_str(&msg.content);
            }
        }

        self.turns.push(LoggedTurn {
            turn,
            status: if ok {
                "OK".to_owned()
            } else {
                "FAILED".to_owned()
            },
            user_prompt,
            gbnf: gbnf.to_owned(),
            progress: progress.map(ToOwned::to_owned),
            action_json: response.content.clone(),
            observation: observation.to_owned(),
            thinking: response.thinking.clone(),
            prompt_tokens: response.usage.prompt_tokens,
            completion_tokens: response.usage.completion_tokens,
            cached_tokens: response.usage.cached_tokens,
            elapsed_ms: response.elapsed.as_millis() as u64,
        });
        let _ = self.flush();
    }

    fn flush(&self) -> Result<(), String> {
        if let Some(parent) = self.log_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create '{}': {}", parent.display(), e))?;
        }

        let mut builder = SfgnBuilder::new();
        builder.set_root_prop("kind", "agent_session");
        builder.set_root_prop("goal", self.goal.as_str());
        builder.set_root_prop("total_turns", self.turns.len() as u64);
        if let Some(sys) = &self.system_prompt {
            builder.add_multiline(NodeHandle::ROOT, "system_prompt", Some("text"), sys);
        }

        for t in &self.turns {
            let label = format!("T{}", t.turn);
            let h = builder.add_node(NodeHandle::ROOT, &label);
            builder.add_prop(h, "status", t.status.as_str());
            if let Some(prog) = &t.progress {
                builder.add_prop(h, "progress", prog.as_str());
            }
            builder.add_prop(h, "prompt_tokens", t.prompt_tokens);
            builder.add_prop(h, "completion_tokens", t.completion_tokens);
            builder.add_prop(h, "cached_tokens", t.cached_tokens);
            builder.add_prop(h, "elapsed_ms", t.elapsed_ms);
            builder.add_multiline(h, "user_prompt", Some("text"), &t.user_prompt);
            if !t.gbnf.is_empty() {
                builder.add_multiline(h, "gbnf", Some("gbnf"), &t.gbnf);
            }
            if !t.thinking.is_empty() {
                builder.add_multiline(h, "thinking", Some("text"), &t.thinking);
            }
            builder.add_prop(h, "action", t.action_json.as_str());
            builder.add_prop(h, "observation", t.observation.as_str());
        }

        let rendered = builder
            .build()
            .map_err(|e| format!("failed to compile session log SFGN: {}", e))?;
        fs::write(&self.log_path, &rendered)
            .map_err(|e| format!("failed to write '{}': {}", self.log_path.display(), e))?;
        Ok(())
    }
}
