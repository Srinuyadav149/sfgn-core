pub mod client;
pub mod json;
pub mod stream;

use std::fmt;
use std::io;
use std::time::Duration;

pub use client::LlmClient;
pub use json::{JsonValue, escape_json_str, parse_json};
pub use stream::StreamDemuxer;

/// Chat message role in `/v1/chat/completions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// A single message in the OpenAI-compatible chat request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Configuration for the `/v1/chat/completions` endpoint, tuned by default for LFM3 1.7B.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Full HTTP endpoint URL (e.g. `http://127.0.0.1:9931/v1/chat/completions`).
    pub endpoint_url: String,
    /// Optional explicit model identifier (omitted when `None` so `llama-server` uses its loaded GGUF).
    pub model: Option<String>,
    /// Sampling temperature (`0.0` for deterministic tool/graph traversal).
    pub temperature: f32,
    /// Repetition penalty (`1.12` prevents `<think>` reasoning loops on LFM3 1.7B).
    pub repeat_penalty: f32,
    /// Maximum completion tokens (`1600` gives ample room for `<think>` + JSON without runaway loops).
    pub max_tokens: u32,
    /// Enable `llama-server` KV-cache prefix reuse (`cache_prompt: true`).
    pub cache_prompt: bool,
    /// Attach dynamic GBNF grammar when provided by `sfgn-context`.
    pub use_grammar: bool,
    /// TCP connection timeout.
    pub connect_timeout: Duration,
    /// Socket read timeout while waiting for prefill and token generation on local hardware.
    pub read_timeout: Duration,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoint_url: "http://127.0.0.1:9931/v1/chat/completions".to_owned(),
            model: None,
            temperature: 0.0,
            repeat_penalty: 1.12,
            max_tokens: 1600,
            cache_prompt: true,
            use_grammar: true,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(900),
        }
    }
}

/// Token usage and KV-cache statistics reported at the end of an SSE stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
}

/// Real-time events emitted during SSE streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStreamEvent<'a> {
    /// Reasoning token from `delta.reasoning_content` or inside `<think>...</think>`.
    Thinking(&'a str),
    /// Action/output token from `delta.content` outside `<think>...</think>`.
    Content(&'a str),
}

/// Completed response from a single `/v1/chat/completions` turn.
#[derive(Debug, Clone, Default)]
pub struct TurnResponse {
    /// Separated internal reasoning trace.
    pub thinking: String,
    /// Final post-`</think>` output (the raw JSON action string).
    pub content: String,
    /// Optional `finish_reason` reported by the server (`"stop"`, `"length"`, etc.).
    pub finish_reason: Option<String>,
    /// Token and KV-cache metrics.
    pub usage: TokenUsage,
    /// Wall-clock time from request send to first token.
    pub ttft: Duration,
    /// Total wall-clock duration for the turn.
    pub elapsed: Duration,
}

/// Errors produced by the HTTP/SSE transport, JSON chunk parser, or stream demuxer.
#[derive(Debug)]
pub enum LlmError {
    InvalidEndpoint(String),
    Io(io::Error),
    HttpStatus { status: u16, body: String },
    Protocol(String),
    Json(String),
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint(msg) => write!(f, "invalid LLM endpoint: {}", msg),
            Self::Io(err) => write!(f, "LLM transport I/O error: {}", err),
            Self::HttpStatus { status, body } => {
                write!(f, "LLM server returned HTTP {}: {}", status, body.trim())
            }
            Self::Protocol(msg) => write!(f, "LLM SSE protocol error: {}", msg),
            Self::Json(msg) => write!(f, "LLM JSON parse error: {}", msg),
        }
    }
}

impl std::error::Error for LlmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for LlmError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;
