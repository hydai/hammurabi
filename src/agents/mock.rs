//! `MockAiAgent` — prompt-substring–matching mock used by transition tests.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{AiAgent, AiInvocation, AiResult};
use crate::error::HammurabiError;

pub struct MockAiAgent {
    responses: Mutex<HashMap<String, AiResult>>,
    default_response: Mutex<Option<AiResult>>,
}

impl MockAiAgent {
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
            default_response: Mutex::new(None),
        }
    }

    pub fn set_response(&self, prompt_contains: &str, result: AiResult) {
        self.responses
            .lock()
            .unwrap()
            .insert(prompt_contains.to_string(), result);
    }

    pub fn set_default_response(&self, result: AiResult) {
        *self.default_response.lock().unwrap() = Some(result);
    }
}

#[async_trait]
impl AiAgent for MockAiAgent {
    async fn invoke(&self, invocation: AiInvocation) -> Result<AiResult, HammurabiError> {
        let responses = self.responses.lock().unwrap();
        for (key, result) in responses.iter() {
            if invocation.prompt.contains(key) {
                return Ok(result.clone());
            }
        }
        drop(responses);

        let default = self.default_response.lock().unwrap();
        if let Some(result) = default.as_ref() {
            return Ok(result.clone());
        }

        Err(HammurabiError::Ai(
            "no mock response configured".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;

    fn sample_result(content: &str) -> AiResult {
        AiResult {
            content: content.to_string(),
            session_id: Some("sess".to_string()),
            input_tokens: 10,
            output_tokens: 5,
            agent_kind: AgentKind::ClaudeCli,
            tool_summary: Vec::new(),
        }
    }

    fn sample_invocation(prompt: &str) -> AiInvocation {
        AiInvocation {
            agent_kind: AgentKind::ClaudeCli,
            model: "test".to_string(),
            max_turns: 50,
            effort: "high".to_string(),
            worktree_path: "/tmp/test".to_string(),
            prompt: prompt.to_string(),
            timeout_secs: 3600,
            stall_timeout_secs: 300,
        }
    }

    #[tokio::test]
    async fn test_mock_agent_matches_substring() {
        let agent = MockAiAgent::new();
        agent.set_response("spec", sample_result("# SPEC"));
        let result = agent
            .invoke(sample_invocation("Generate a spec for this issue"))
            .await
            .unwrap();
        assert_eq!(result.content, "# SPEC");
    }

    #[tokio::test]
    async fn test_mock_agent_no_response_errors() {
        let agent = MockAiAgent::new();
        let result = agent.invoke(sample_invocation("anything")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_agent_default_response() {
        let agent = MockAiAgent::new();
        agent.set_default_response(sample_result("default"));
        let result = agent.invoke(sample_invocation("anything")).await.unwrap();
        assert_eq!(result.content, "default");
    }
}
