//! Integration tests for the ACP client, driven against the scripted
//! `fake-acp-agent` helper binary. Exercises the full Session lifecycle
//! plus the `AcpAgent` adapter without requiring any real agent install.

#[path = "../src/acp/mod.rs"]
mod acp;
#[path = "../src/agents/mod.rs"]
mod agents;
#[path = "../src/env_expand.rs"]
mod env_expand;
#[path = "../src/error.rs"]
mod error;

use std::collections::HashMap;
use std::sync::Arc;

use acp::session::AcpAgentDef;
use agents::acp::AcpAgent;
use agents::{AgentKind, AiAgent, AiInvocation};

/// Path to the fake-acp-agent binary compiled by this crate. Populated by
/// Cargo for integration tests when a `[[bin]]` target is defined.
const FAKE_AGENT_BIN: &str = env!("CARGO_BIN_EXE_fake-acp-agent");

fn fake_def(scenario: &str) -> AcpAgentDef {
    let mut env = HashMap::new();
    env.insert("HAMMURABI_FAKE_SCENARIO".to_string(), scenario.to_string());
    AcpAgentDef {
        command: FAKE_AGENT_BIN.to_string(),
        args: Vec::new(),
        env,
    }
}

fn make_worktree(suffix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hammurabi-acp-test-{suffix}"));
    std::fs::create_dir_all(&dir).expect("create tmp worktree");
    dir
}

fn make_invocation(worktree: &std::path::Path, kind: AgentKind) -> AiInvocation {
    AiInvocation {
        agent_kind: kind,
        model: "fake-model".to_string(),
        max_turns: 10,
        effort: "high".to_string(),
        worktree_path: worktree.to_str().unwrap().to_string(),
        prompt: "Run the scenario.".to_string(),
        timeout_secs: 10,
        stall_timeout_secs: 0,
        events: None,
    }
}

#[tokio::test]
async fn happy_path_returns_accumulated_text_and_usage() {
    let worktree = make_worktree("happy");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("happy")));
    let result = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect("happy path succeeds");

    assert_eq!(result.content, "Hello from the fake agent.");
    assert_eq!(result.input_tokens, 42);
    assert_eq!(result.output_tokens, 17);
    assert_eq!(result.agent_kind, AgentKind::AcpClaude);
    assert_eq!(result.session_id.as_deref(), Some("fake-session-1"));
    assert!(result.tool_summary.is_empty());

    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn permission_request_is_auto_allowed() {
    let worktree = make_worktree("permission");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("permission")));
    let result = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect("permission scenario succeeds with auto-allow");

    assert!(result.content.contains("Permission granted"));
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn tool_calls_are_captured_in_summary() {
    let worktree = make_worktree("tool_calls");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("tool_calls")));
    let result = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect("tool_calls scenario succeeds");

    assert_eq!(result.content, "Reading... done.");
    assert_eq!(result.tool_summary.len(), 1);
    assert_eq!(result.tool_summary[0].title, "Read foo.rs");
    assert_eq!(result.tool_summary[0].status, agents::ToolStatus::Completed);
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn session_new_error_propagates() {
    let worktree = make_worktree("error");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("error")));
    let err = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect_err("session/new error surfaces");

    let msg = format!("{err}");
    assert!(
        msg.contains("fake agent refused"),
        "unexpected error message: {msg}"
    );
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn overall_timeout_triggers_ai_timeout() {
    let worktree = make_worktree("timeout");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("timeout")));
    let mut inv = make_invocation(&worktree, AgentKind::AcpClaude);
    inv.timeout_secs = 2;
    let err = agent.invoke(inv).await.expect_err("timeout scenario fails");

    match err {
        error::HammurabiError::AiTimeout(msg) => {
            assert!(msg.contains("timeout"), "got: {msg}");
        }
        other => panic!("expected AiTimeout, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn stall_timeout_triggers_ai_timeout() {
    let worktree = make_worktree("stall");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("stall")));
    let mut inv = make_invocation(&worktree, AgentKind::AcpClaude);
    inv.timeout_secs = 60;
    inv.stall_timeout_secs = 2;
    let err = agent.invoke(inv).await.expect_err("stall scenario fails");

    match err {
        error::HammurabiError::AiTimeout(_) => {}
        other => panic!("expected AiTimeout, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn agent_crash_surfaces_as_acp_error() {
    let worktree = make_worktree("crash");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("crash")));
    // `crash` scenario exits after initialize; session/new will see the
    // closed stdin and fail.
    let err = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect_err("crash scenario fails");

    let msg = format!("{err}");
    // Either "ACP connection closed" or "channel closed" / timeout wording
    // is acceptable — we just want a clean error, not a panic.
    assert!(
        msg.to_lowercase().contains("closed") || msg.to_lowercase().contains("connection"),
        "unexpected error: {msg}"
    );
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn missing_binary_surfaces_config_error() {
    let worktree = make_worktree("missing-binary");
    let mut def = fake_def("happy");
    def.command = "/nonexistent/path/to/no-such-acp-agent".to_string();
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, def));
    let err = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect_err("missing binary fails");

    let msg = format!("{err}");
    assert!(
        msg.contains("not found"),
        "missing-binary error should mention 'not found': {msg}"
    );
    let _ = std::fs::remove_dir_all(&worktree);
}

#[tokio::test]
async fn no_content_is_rejected() {
    let worktree = make_worktree("no_content");
    let agent = Arc::new(AcpAgent::new(AgentKind::AcpClaude, fake_def("no_content")));
    let err = agent
        .invoke(make_invocation(&worktree, AgentKind::AcpClaude))
        .await
        .expect_err("no_content should error");

    let msg = format!("{err}");
    assert!(
        msg.contains("no content"),
        "expected empty-content error, got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&worktree);
}
