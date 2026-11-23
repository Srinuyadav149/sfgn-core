pub mod executor;
pub mod loop_runner;
pub mod recovery;

use std::fmt;
use std::path::PathBuf;

use sfgn_llm::{LlmError, TurnResponse};

pub use executor::{ActionDispatcher, DispatchStep};
pub use loop_runner::{AgentRunner, AgentSession};
pub use recovery::{FailureDiagnosis, RecoveryPlanner};

/// Runtime bounds and verification settings for [`AgentRunner`].
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Hard upper bound on turns per goal before aborting.
    pub max_turns: usize,
    /// Circuit-breaker cap on consecutive `[FAILED]` turns before aborting.
    pub max_consecutive_errors: usize,
    /// Number of recently inspected SFGN nodes kept in `ACTIVE SFGN NODES`.
    pub max_active_nodes: usize,
    /// Command executed by `tools/verify/run_verify` (defaults to `cargo check --workspace --message-format=short`).
    pub verify_command: Vec<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            max_consecutive_errors: 3,
            max_active_nodes: 4,
            verify_command: vec![
                "cargo".to_owned(),
                "check".to_owned(),
                "--workspace".to_owned(),
                "--message-format=short".to_owned(),
            ],
        }
    }
}

/// Real-time lifecycle events emitted during agent execution for CLI streaming/logging.
#[derive(Debug, Clone)]
pub enum AgentEvent<'a> {
    TurnStarted {
        turn: usize,
    },
    ThinkingDelta(&'a str),
    ContentDelta(&'a str),
    TurnStats(&'a TurnResponse),
    ActionOk {
        turn: usize,
        action: &'a str,
        observation: &'a str,
    },
    ActionFailed {
        turn: usize,
        action: &'a str,
        error: &'a str,
        hint: Option<&'a str>,
    },
    Finished {
        turn: usize,
        summary: &'a str,
    },
}

/// Final summary and cumulative token metrics returned when `finish` is called.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub summary: String,
    pub turns_used: usize,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_cached_tokens: u64,
    pub log_path: PathBuf,
}

/// Errors that terminate an agent run.
#[derive(Debug)]
pub enum AgentError {
    Llm(LlmError),
    MaxTurnsExceeded {
        max_turns: usize,
    },
    ConsecutiveErrorsExceeded {
        consecutive_errors: usize,
        last_error: String,
    },
    Workspace(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Llm(err) => write!(f, "{}", err),
            Self::MaxTurnsExceeded { max_turns } => {
                write!(f, "agent reached maximum turn limit ({} turns)", max_turns)
            }
            Self::ConsecutiveErrorsExceeded {
                consecutive_errors,
                last_error,
            } => write!(
                f,
                "agent aborted after {} consecutive failed turns (last error: {})",
                consecutive_errors, last_error
            ),
            Self::Workspace(msg) => write!(f, "workspace initialization error: {}", msg),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Llm(err) => Some(err),
            _ => None,
        }
    }
}

impl From<LlmError> for AgentError {
    fn from(err: LlmError) -> Self {
        Self::Llm(err)
    }
}

pub type Result<T> = std::result::Result<T, AgentError>;
