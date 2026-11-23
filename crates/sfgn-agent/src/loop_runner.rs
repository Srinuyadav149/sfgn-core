use std::path::Path;

use sfgn_context::{
    MemoryStore, PromptAssembler, Scratchpad, SessionLogger, ToolRegistry, TurnHistory,
    WorkspaceGraph,
};
use sfgn_llm::{LlmClient, LlmConfig, TurnResponse, TurnStreamEvent};

use crate::executor::{ActionDispatcher, DispatchStep};
use crate::recovery::RecoveryPlanner;
use crate::{AgentConfig, AgentError, AgentEvent, AgentOutcome, Result};

/// Stateful interactive agent session that preserves `MemoryStore`, `ToolRegistry`, `Scratchpad`,
/// `TurnHistory`, and `SessionLogger` across multiple goals (used by both `sfgn run` and `sfgn chat`).
pub struct AgentSession {
    client: LlmClient,
    config: AgentConfig,
    dispatcher: ActionDispatcher,
    logger: SessionLogger,
    turn_counter: usize,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    total_cached_tokens: u64,
}

impl AgentSession {
    pub fn new(
        workspace_root: impl AsRef<Path>,
        llm_config: LlmConfig,
        agent_config: AgentConfig,
        initial_goal: &str,
    ) -> Result<Self> {
        let graph = WorkspaceGraph::new(&workspace_root);
        graph.ensure_fs_graph().map_err(AgentError::Workspace)?;

        let memories = MemoryStore::new(&workspace_root);
        let tools = ToolRegistry::new();
        let scratchpad = Scratchpad::new(agent_config.max_active_nodes);
        let history = TurnHistory::new();
        let logger = SessionLogger::new(&workspace_root, initial_goal);

        let dispatcher = ActionDispatcher::new(
            graph,
            memories,
            tools,
            scratchpad,
            history,
            agent_config.verify_command.clone(),
        );

        Ok(Self {
            client: LlmClient::new(llm_config),
            config: agent_config,
            dispatcher,
            logger,
            turn_counter: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            total_cached_tokens: 0,
        })
    }

    /// Runs the unified `Assemble -> Stream LLM -> Parse/Dispatch -> Record -> Retry` loop
    /// until the model calls `finish`, hits `max_consecutive_errors`, or reaches `max_turns`.
    pub fn run_goal<F>(&mut self, goal: &str, mut on_event: F) -> Result<AgentOutcome>
    where
        F: FnMut(AgentEvent<'_>),
    {
        let start_turn = self.turn_counter;
        let mut consecutive_errors = 0usize;

        for _step in 0..self.config.max_turns {
            self.turn_counter += 1;
            let turn = self.turn_counter;

            on_event(AgentEvent::TurnStarted { turn });

            let (messages, gbnf) = PromptAssembler::build(
                goal,
                &self.dispatcher.graph,
                &self.dispatcher.memories,
                &self.dispatcher.tools,
                &self.dispatcher.history,
                &self.dispatcher.scratchpad,
            );

            let response = match self
                .client
                .stream_chat(&messages, Some(&gbnf), |ev| match ev {
                    TurnStreamEvent::Thinking(tok) => on_event(AgentEvent::ThinkingDelta(tok)),
                    TurnStreamEvent::Content(tok) => on_event(AgentEvent::ContentDelta(tok)),
                }) {
                Ok(resp) => resp,
                Err(llm_err) => {
                    let obs = format!("LLM_TRANSPORT_ERROR: {}", llm_err);
                    self.logger.record_turn(
                        turn,
                        false,
                        &messages,
                        &gbnf,
                        None,
                        &TurnResponse::default(),
                        &obs,
                    );
                    return Err(AgentError::Llm(llm_err));
                }
            };

            self.total_prompt_tokens += response.usage.prompt_tokens;
            self.total_completion_tokens += response.usage.completion_tokens;
            self.total_cached_tokens += response.usage.cached_tokens;

            on_event(AgentEvent::TurnStats(&response));

            // Detect `<think>` token budget exhaustion before JSON parsing
            if response.content.trim().is_empty()
                && (response.finish_reason.as_deref() == Some("length")
                    || !response.thinking.is_empty())
            {
                consecutive_errors += 1;
                let diag = RecoveryPlanner::thinking_truncated();
                let action_summary = "truncated_thinking".to_owned();
                self.dispatcher.scratchpad.record_failure(
                    format!("truncated_thinking_T{}", turn),
                    diag.recovery_banner.clone(),
                );
                self.dispatcher.history.push_failed(
                    turn,
                    &action_summary,
                    &diag.error_summary,
                    diag.recovery_hint.clone(),
                );
                let log_obs = match &diag.recovery_hint {
                    Some(h) => format!("FAILED: {} | Hint: {}", diag.error_summary, h),
                    None => format!("FAILED: {}", diag.error_summary),
                };
                self.logger
                    .record_turn(turn, false, &messages, &gbnf, None, &response, &log_obs);
                on_event(AgentEvent::ActionFailed {
                    turn,
                    action: &action_summary,
                    error: &diag.error_summary,
                    hint: diag.recovery_hint.as_deref(),
                });

                if consecutive_errors >= self.config.max_consecutive_errors {
                    return Err(AgentError::ConsecutiveErrorsExceeded {
                        consecutive_errors,
                        last_error: diag.error_summary,
                    });
                }
                continue;
            }

            match self.dispatcher.dispatch(turn, &response.content) {
                DispatchStep::Ok {
                    action_summary,
                    progress,
                    observation,
                } => {
                    consecutive_errors = 0;
                    self.logger.record_turn(
                        turn,
                        true,
                        &messages,
                        &gbnf,
                        progress.as_deref(),
                        &response,
                        &observation,
                    );
                    on_event(AgentEvent::ActionOk {
                        turn,
                        action: &action_summary,
                        observation: &observation,
                    });
                }
                DispatchStep::Failed {
                    action_summary,
                    progress,
                    diagnosis,
                } => {
                    consecutive_errors += 1;
                    let log_obs = match &diagnosis.recovery_hint {
                        Some(h) => format!("FAILED: {} | Hint: {}", diagnosis.error_summary, h),
                        None => format!("FAILED: {}", diagnosis.error_summary),
                    };
                    self.logger.record_turn(
                        turn,
                        false,
                        &messages,
                        &gbnf,
                        progress.as_deref(),
                        &response,
                        &log_obs,
                    );
                    on_event(AgentEvent::ActionFailed {
                        turn,
                        action: &action_summary,
                        error: &diagnosis.error_summary,
                        hint: diagnosis.recovery_hint.as_deref(),
                    });

                    if consecutive_errors >= self.config.max_consecutive_errors {
                        return Err(AgentError::ConsecutiveErrorsExceeded {
                            consecutive_errors,
                            last_error: diagnosis.error_summary,
                        });
                    }
                }
                DispatchStep::Finished { progress, summary } => {
                    self.logger.record_turn(
                        turn,
                        true,
                        &messages,
                        &gbnf,
                        progress.as_deref(),
                        &response,
                        &summary,
                    );
                    on_event(AgentEvent::Finished {
                        turn,
                        summary: &summary,
                    });
                    return Ok(AgentOutcome {
                        summary,
                        turns_used: self.turn_counter - start_turn,
                        total_prompt_tokens: self.total_prompt_tokens,
                        total_completion_tokens: self.total_completion_tokens,
                        total_cached_tokens: self.total_cached_tokens,
                        log_path: self.logger.log_path().to_path_buf(),
                    });
                }
            }
        }

        Err(AgentError::MaxTurnsExceeded {
            max_turns: self.config.max_turns,
        })
    }
}

/// Stateless convenience runner for single-goal CLI execution (`sfgn run "<goal>"`).
#[derive(Debug, Clone)]
pub struct AgentRunner {
    llm_config: LlmConfig,
    agent_config: AgentConfig,
}

impl AgentRunner {
    pub fn new(llm_config: LlmConfig, agent_config: AgentConfig) -> Self {
        Self {
            llm_config,
            agent_config,
        }
    }

    pub fn run<F>(
        &self,
        workspace_root: impl AsRef<Path>,
        goal: &str,
        on_event: F,
    ) -> Result<AgentOutcome>
    where
        F: FnMut(AgentEvent<'_>),
    {
        let mut session = AgentSession::new(
            workspace_root,
            self.llm_config.clone(),
            self.agent_config.clone(),
            goal,
        )?;
        session.run_goal(goal, on_event)
    }
}
