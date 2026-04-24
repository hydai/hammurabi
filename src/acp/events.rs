//! Classify incoming `session/update` notifications into the typed
//! [`AgentEvent`] the rest of Hammurabi consumes.
//!
//! The spec emits a range of `sessionUpdate` kinds — `agent_message_chunk`,
//! `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`,
//! `config_option_update`, and future additions. We surface only the ones
//! that matter for progress display; the rest are silently dropped (the
//! caller can still inspect the raw notification if needed).

use serde_json::Value;

use crate::agents::AgentEvent;

/// Convert a `session/update` notification's `params` into a high-level
/// [`AgentEvent`]. Returns `None` for updates Hammurabi doesn't care about.
pub fn classify_update(params: &Value) -> Option<AgentEvent> {
    let update = params.get("update")?;
    let kind = update.get("sessionUpdate").and_then(|v| v.as_str())?;
    let tool_id = update
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    match kind {
        "agent_message_chunk" => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())?;
            Some(AgentEvent::TextDelta(text.to_string()))
        }
        "agent_thought_chunk" => Some(AgentEvent::Thinking),
        "tool_call" => {
            let title = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Some(AgentEvent::ToolStarted { id: tool_id, title })
        }
        "tool_call_update" => {
            let title = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let status = update
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if status == "completed" || status == "failed" {
                Some(AgentEvent::ToolFinished {
                    id: tool_id,
                    title,
                    ok: status == "completed",
                })
            } else {
                // In-progress updates reuse ToolStarted to refine title / args.
                Some(AgentEvent::ToolStarted { id: tool_id, title })
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentEvent;
    use serde_json::json;

    #[test]
    fn classifies_agent_message_chunk_as_text_delta() {
        let params = json!({
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"text": "Hello, world!"}
            }
        });
        match classify_update(&params) {
            Some(AgentEvent::TextDelta(t)) => assert_eq!(t, "Hello, world!"),
            other => panic!("expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn classifies_agent_thought_chunk_as_thinking() {
        let params = json!({
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "content": {"text": "considering..."}
            }
        });
        assert!(matches!(
            classify_update(&params),
            Some(AgentEvent::Thinking)
        ));
    }

    #[test]
    fn classifies_tool_call_as_tool_started() {
        let params = json!({
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tc-1",
                "title": "Edit foo.rs"
            }
        });
        match classify_update(&params) {
            Some(AgentEvent::ToolStarted { id, title }) => {
                assert_eq!(id, "tc-1");
                assert_eq!(title, "Edit foo.rs");
            }
            other => panic!("expected ToolStarted, got {:?}", other),
        }
    }

    #[test]
    fn classifies_tool_call_update_completed_as_tool_finished_ok() {
        let params = json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-2",
                "title": "Edit bar.rs",
                "status": "completed"
            }
        });
        match classify_update(&params) {
            Some(AgentEvent::ToolFinished { id, title, ok }) => {
                assert_eq!(id, "tc-2");
                assert_eq!(title, "Edit bar.rs");
                assert!(ok);
            }
            other => panic!("expected ToolFinished ok, got {:?}", other),
        }
    }

    #[test]
    fn classifies_tool_call_update_failed_as_tool_finished_not_ok() {
        let params = json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-3",
                "title": "Risky op",
                "status": "failed"
            }
        });
        match classify_update(&params) {
            Some(AgentEvent::ToolFinished { ok, .. }) => assert!(!ok),
            other => panic!("expected ToolFinished !ok, got {:?}", other),
        }
    }

    #[test]
    fn classifies_tool_call_update_in_progress_as_tool_started() {
        let params = json!({
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tc-4",
                "title": "Terminal",
                "status": "in_progress"
            }
        });
        assert!(matches!(
            classify_update(&params),
            Some(AgentEvent::ToolStarted { .. })
        ));
    }

    #[test]
    fn missing_update_key_returns_none() {
        assert!(classify_update(&json!({})).is_none());
    }

    #[test]
    fn unknown_kind_returns_none() {
        let params = json!({"update": {"sessionUpdate": "plan"}});
        assert!(classify_update(&params).is_none());
    }

    #[test]
    fn missing_tool_call_id_defaults_to_empty() {
        let params = json!({
            "update": {"sessionUpdate": "tool_call", "title": "x"}
        });
        match classify_update(&params) {
            Some(AgentEvent::ToolStarted { id, .. }) => assert!(id.is_empty()),
            other => panic!("expected ToolStarted, got {:?}", other),
        }
    }
}
