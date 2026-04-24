//! `AcpAgent` — `AiAgent` implementation backed by a single ACP session.
//!
//! One call to [`AiAgent::invoke`] maps to one subprocess: spawn the agent,
//! run `initialize` + `session/new`, best-effort set the model, stream the
//! single prompt, and tear everything down on return. No pooling, no
//! resumption — Hammurabi's transitions are inherently one-shot.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::{timeout, Instant};

use super::{AgentKind, AiAgent, AiInvocation, AiResult, ToolInvocation, ToolStatus};
use crate::acp::events::classify_update;
use crate::acp::registry::{AcpSessionRegistry, SessionGuard};
use crate::acp::session::{AcpAgentDef, Session};
use crate::agents::AgentEvent;
use crate::error::HammurabiError;

/// Default subprocess invocation for a given ACP kind. Phase 5 will let the
/// user override these via `[agents.*]` config sections.
pub fn default_agent_def(kind: AgentKind) -> AcpAgentDef {
    match kind {
        AgentKind::AcpClaude => AcpAgentDef {
            command: "claude-agent-acp".to_string(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
        },
        AgentKind::AcpGemini => AcpAgentDef {
            command: "gemini".to_string(),
            args: vec!["--acp".to_string()],
            env: std::collections::HashMap::new(),
        },
        AgentKind::AcpCodex => AcpAgentDef {
            command: "codex-acp".to_string(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
        },
        AgentKind::ClaudeCli => {
            panic!("default_agent_def does not apply to ClaudeCli; use ClaudeCliAgent directly")
        }
    }
}

pub struct AcpAgent {
    kind: AgentKind,
    def: AcpAgentDef,
    /// Shared registry of live ACP PGIDs. Populated via the constructor so
    /// production code gets fan-out shutdown; tests can leave it `None` and
    /// keep the existing per-session teardown behavior.
    session_registry: Option<Arc<AcpSessionRegistry>>,
}

impl AcpAgent {
    pub fn new(kind: AgentKind, def: AcpAgentDef) -> Self {
        assert!(
            kind.is_acp(),
            "AcpAgent requires an ACP AgentKind, got {kind:?}"
        );
        Self {
            kind,
            def,
            session_registry: None,
        }
    }

    pub fn with_registry(mut self, registry: Arc<AcpSessionRegistry>) -> Self {
        self.session_registry = Some(registry);
        self
    }

    /// Expose the underlying command for logs / error messages.
    #[allow(dead_code)]
    pub fn command(&self) -> &str {
        &self.def.command
    }
}

#[async_trait]
impl AiAgent for AcpAgent {
    async fn invoke(&self, invocation: AiInvocation) -> Result<AiResult, HammurabiError> {
        let worktree = Path::new(&invocation.worktree_path);
        if !worktree.exists() {
            return Err(HammurabiError::Ai(format!(
                "worktree does not exist: {}",
                invocation.worktree_path
            )));
        }

        // 1. Spawn and handshake.
        let mut session = Session::start(&self.def, worktree).await?;
        // Register the PGID so a daemon-level shutdown can fan SIGTERM
        // out to every live ACP child. The guard unregisters on any exit
        // path from this function.
        let _registry_guard = self
            .session_registry
            .as_ref()
            .zip(session.pgid())
            .map(|(reg, pgid)| SessionGuard::new(reg.clone(), pgid));
        let _init = session.initialize().await?;
        let session_id = session.new_session(worktree).await?;

        // 2. Best-effort model pin. Errors are logged inside, not propagated.
        if !invocation.model.is_empty() {
            session
                .set_config_option("model", &invocation.model)
                .await?;
        }

        // 3. Send the prompt and stream notifications.
        let (mut rx, prompt_id) = session.prompt(&invocation.prompt).await?;

        let completion_method = format!("__prompt_completion:{prompt_id}");
        let overall_deadline = Instant::now() + Duration::from_secs(invocation.timeout_secs);
        let stall_enabled = invocation.stall_timeout_secs > 0;
        let stall_duration = Duration::from_secs(invocation.stall_timeout_secs);

        let mut content = String::new();
        let mut tools: Vec<ToolInvocation> = Vec::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut completion_error: Option<String> = None;

        loop {
            let remaining = overall_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = session.cancel().await;
                session.end_prompt().await;
                return Err(HammurabiError::AiTimeout(format!(
                    "agent exceeded total timeout of {}s",
                    invocation.timeout_secs
                )));
            }
            let poll_for = if stall_enabled {
                stall_duration.min(remaining)
            } else {
                remaining
            };

            let notif = match timeout(poll_for, rx.recv()).await {
                Ok(Some(n)) => n,
                Ok(None) => {
                    // Reader dropped / agent died without completing the turn.
                    session.end_prompt().await;
                    return Err(HammurabiError::Acp(
                        "ACP agent closed connection before completing prompt".to_string(),
                    ));
                }
                Err(_) => {
                    // Either stall or overall timeout.
                    let _ = session.cancel().await;
                    session.end_prompt().await;
                    if Instant::now() >= overall_deadline {
                        return Err(HammurabiError::AiTimeout(format!(
                            "agent exceeded total timeout of {}s",
                            invocation.timeout_secs
                        )));
                    }
                    return Err(HammurabiError::AiTimeout(format!(
                        "agent stalled — no output for {}s",
                        invocation.stall_timeout_secs
                    )));
                }
            };

            if notif.method == completion_method {
                if let Some(params) = notif.params.as_ref() {
                    if let Some(err_obj) = params.get("error") {
                        let msg = err_obj
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error")
                            .to_string();
                        completion_error = Some(msg);
                    } else if let Some(result) = params.get("result") {
                        if let Some(usage) = result.get("usage") {
                            if let Some(t) = usage.get("inputTokens").and_then(|v| v.as_u64()) {
                                input_tokens = t;
                            }
                            if let Some(t) = usage.get("outputTokens").and_then(|v| v.as_u64()) {
                                output_tokens = t;
                            }
                        }
                    }
                }
                break;
            }

            if let Some(params) = notif.params.as_ref() {
                if let Some(event) = classify_update(params) {
                    if let Some(tx) = invocation.events.as_ref() {
                        // The receiver may already be gone (aggregator shut
                        // down, e.g. caller dropped the channel). Ignore send
                        // errors — they never affect the agent run itself.
                        let _ = tx.send(event.clone());
                    }
                    apply_event(&event, &mut content, &mut tools);
                }
            }
        }

        session.end_prompt().await;

        if let Some(msg) = completion_error {
            return Err(HammurabiError::Acp(msg));
        }

        if content.is_empty() {
            return Err(HammurabiError::Ai(
                "ACP agent produced no content output".to_string(),
            ));
        }

        Ok(AiResult {
            content,
            session_id: Some(session_id),
            input_tokens,
            output_tokens,
            agent_kind: self.kind,
            tool_summary: tools,
        })
    }
}

/// Accumulate a streamed event into the in-progress result.
fn apply_event(event: &AgentEvent, content: &mut String, tools: &mut Vec<ToolInvocation>) {
    match event {
        AgentEvent::TextDelta(t) => content.push_str(t),
        AgentEvent::Thinking => {}
        AgentEvent::ToolStarted { id, title } => {
            if let Some(existing) = tools
                .iter_mut()
                .find(|t| t.title == *id || t.title == *title)
            {
                existing.status = ToolStatus::Running;
                if !title.is_empty() {
                    existing.title = title.clone();
                }
            } else if !title.is_empty() {
                tools.push(ToolInvocation {
                    title: title.clone(),
                    status: ToolStatus::Running,
                });
            }
        }
        AgentEvent::ToolFinished { id: _, title, ok } => {
            let status = if *ok {
                ToolStatus::Completed
            } else {
                ToolStatus::Failed
            };
            if let Some(existing) = tools.iter_mut().find(|t| t.title == *title) {
                existing.status = status;
            } else if !title.is_empty() {
                tools.push(ToolInvocation {
                    title: title.clone(),
                    status,
                });
            }
        }
        AgentEvent::ConfigChanged { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentEvent;

    #[test]
    fn apply_event_accumulates_text_deltas() {
        let mut content = String::new();
        let mut tools = Vec::new();
        apply_event(
            &AgentEvent::TextDelta("Hello, ".into()),
            &mut content,
            &mut tools,
        );
        apply_event(
            &AgentEvent::TextDelta("world!".into()),
            &mut content,
            &mut tools,
        );
        assert_eq!(content, "Hello, world!");
        assert!(tools.is_empty());
    }

    #[test]
    fn apply_event_tracks_tool_lifecycle() {
        let mut content = String::new();
        let mut tools = Vec::new();
        apply_event(
            &AgentEvent::ToolStarted {
                id: "t1".into(),
                title: "Read foo.rs".into(),
            },
            &mut content,
            &mut tools,
        );
        apply_event(
            &AgentEvent::ToolFinished {
                id: "t1".into(),
                title: "Read foo.rs".into(),
                ok: true,
            },
            &mut content,
            &mut tools,
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].title, "Read foo.rs");
        assert_eq!(tools[0].status, ToolStatus::Completed);
    }

    #[test]
    fn default_agent_def_for_each_acp_kind() {
        assert_eq!(
            default_agent_def(AgentKind::AcpClaude).command,
            "claude-agent-acp"
        );
        assert_eq!(default_agent_def(AgentKind::AcpGemini).command, "gemini");
        assert_eq!(
            default_agent_def(AgentKind::AcpGemini).args,
            vec!["--acp".to_string()]
        );
        assert_eq!(default_agent_def(AgentKind::AcpCodex).command, "codex-acp");
    }

    #[test]
    #[should_panic(expected = "does not apply to ClaudeCli")]
    fn default_agent_def_panics_for_claude_cli() {
        let _ = default_agent_def(AgentKind::ClaudeCli);
    }
}
