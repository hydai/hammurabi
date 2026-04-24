//! Agent abstraction layer.
//!
//! Defines the `AiAgent` trait and supporting types. Concrete implementations
//! live in sibling modules. Today: `claude_cli` (the Claude CLI). Future
//! additions (ACP etc.) plug in as further implementations of `AiAgent`.

pub mod claude_cli;
pub mod registry;

#[cfg(test)]
pub mod mock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::HammurabiError;

pub use claude_cli::ClaudeCliAgent;
pub use registry::AgentRegistry;

/// Which agent implementation should service an invocation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// Claude CLI (`claude --print --output-format stream-json ...`).
    #[default]
    ClaudeCli,
}

/// Status of a tool invocation reported by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToolStatus {
    Running,
    Completed,
    Failed,
}

/// One tool invocation observed during an agent run.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ToolInvocation {
    pub title: String,
    pub status: ToolStatus,
}

/// A streaming event surfaced during an agent run.
///
/// `ClaudeCliAgent` does not produce these (its output is collected whole);
/// the ACP adapter forwards them from `session/update` notifications.
/// Wired into `AiInvocation.events` in Phase 7.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AgentEvent {
    /// Free-form text delta from the agent.
    TextDelta(String),
    /// Agent is thinking (no content yet).
    Thinking,
    /// A tool call started (or its metadata was refined).
    ToolStarted { id: String, title: String },
    /// A tool call finished. `ok=false` means the tool itself failed.
    ToolFinished { id: String, title: String, ok: bool },
    /// Agent acknowledged a config option change (e.g. model switch).
    ConfigChanged { option_id: String, value: String },
}

/// Input to [`AiAgent::invoke`].
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AiInvocation {
    pub agent_kind: AgentKind,
    pub model: String,
    pub max_turns: u32,
    pub effort: String,
    pub worktree_path: String,
    pub prompt: String,
    pub timeout_secs: u64,
    pub stall_timeout_secs: u64,
}

/// Result of a completed agent run.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AiResult {
    pub content: String,
    pub session_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub agent_kind: AgentKind,
    pub tool_summary: Vec<ToolInvocation>,
}

/// Abstraction over an AI agent (Claude CLI, ACP, or a test mock).
#[async_trait]
pub trait AiAgent: Send + Sync {
    async fn invoke(&self, invocation: AiInvocation) -> Result<AiResult, HammurabiError>;
}
