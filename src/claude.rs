use async_trait::async_trait;
use std::path::Path;

use crate::error::HammurabiError;

#[derive(Debug, Clone)]
pub struct AiInvocation {
    pub model: String,
    pub max_turns: u32,
    pub effort: String,
    pub worktree_path: String,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct AiResult {
    pub content: String,
    pub session_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[async_trait]
pub trait AiAgent: Send + Sync {
    async fn invoke(&self, invocation: AiInvocation) -> Result<AiResult, HammurabiError>;
}

pub struct ClaudeCliAgent;

impl ClaudeCliAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AiAgent for ClaudeCliAgent {
    async fn invoke(&self, invocation: AiInvocation) -> Result<AiResult, HammurabiError> {
        let worktree = Path::new(&invocation.worktree_path);
        if !worktree.exists() {
            return Err(HammurabiError::Ai(format!(
                "worktree does not exist: {}",
                invocation.worktree_path
            )));
        }

        let output = tokio::process::Command::new("claude")
            .current_dir(&invocation.worktree_path)
            .arg("--print")
            .arg("--verbose")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--model")
            .arg(&invocation.model)
            .arg("--max-turns")
            .arg(invocation.max_turns.to_string())
            .arg("--effort")
            .arg(&invocation.effort)
            .arg("-p")
            .arg(&invocation.prompt)
            .output()
            .await
            .map_err(|e| HammurabiError::Ai(format!("failed to spawn claude: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HammurabiError::Ai(format!(
                "claude exited with status {}: {}",
                output.status, stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_stream_json(&stdout)
    }
}

pub fn parse_stream_json(output: &str) -> Result<AiResult, HammurabiError> {
    let mut content_parts: Vec<String> = Vec::new();
    let mut session_id: Option<String> = None;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!("unparseable stream-json line: {}", line);
                continue;
            }
        };

        // Extract session ID from message_start or result
        if let Some(sid) = parsed
            .get("session_id")
            .and_then(|v| v.as_str())
        {
            session_id = Some(sid.to_string());
        }

        // Extract content from assistant messages
        if let Some(msg_type) = parsed.get("type").and_then(|v| v.as_str()) {
            match msg_type {
                "assistant" => {
                    if let Some(content) = parsed.get("content") {
                        if let Some(arr) = content.as_array() {
                            for block in arr {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    content_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                "result" => {
                    if let Some(result) = parsed.get("result") {
                        if let Some(text) = result.as_str() {
                            if !text.is_empty() {
                                content_parts.push(text.to_string());
                            }
                        }
                    }
                    if let Some(sid) = parsed.get("session_id").and_then(|v| v.as_str()) {
                        session_id = Some(sid.to_string());
                    }
                }
                _ => {}
            }
        }

        // Extract usage from any message that has it
        if let Some(usage) = parsed.get("usage") {
            if let Some(inp) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                input_tokens = inp;
            }
            if let Some(out) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                output_tokens = out;
            }
        }
    }

    let content = content_parts.join("");

    if content.is_empty() {
        return Err(HammurabiError::Ai(
            "claude produced no content output".to_string(),
        ));
    }

    Ok(AiResult {
        content,
        session_id,
        input_tokens,
        output_tokens,
    })
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

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

            Err(HammurabiError::Ai("no mock response configured".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stream_json_assistant_message() {
        let output = r#"{"type":"assistant","content":[{"type":"text","text":"Hello, world!"}],"usage":{"input_tokens":100,"output_tokens":50},"session_id":"sess-123"}"#;
        let result = parse_stream_json(output).unwrap();
        assert_eq!(result.content, "Hello, world!");
        assert_eq!(result.session_id, Some("sess-123".to_string()));
        assert_eq!(result.input_tokens, 100);
        assert_eq!(result.output_tokens, 50);
    }

    #[test]
    fn test_parse_stream_json_result_message() {
        let output = r#"{"type":"result","result":"Final output here","session_id":"sess-456","usage":{"input_tokens":200,"output_tokens":150}}"#;
        let result = parse_stream_json(output).unwrap();
        assert_eq!(result.content, "Final output here");
        assert_eq!(result.session_id, Some("sess-456".to_string()));
        assert_eq!(result.input_tokens, 200);
        assert_eq!(result.output_tokens, 150);
    }

    #[test]
    fn test_parse_stream_json_multiple_lines() {
        let output = [
            r#"{"type":"assistant","content":[{"type":"text","text":"Part 1"}],"usage":{"input_tokens":50,"output_tokens":25}}"#,
            r#"{"type":"assistant","content":[{"type":"text","text":" Part 2"}],"usage":{"input_tokens":100,"output_tokens":50}}"#,
            r#"{"type":"result","session_id":"sess-789","usage":{"input_tokens":150,"output_tokens":75}}"#,
        ]
        .join("\n");

        let result = parse_stream_json(&output).unwrap();
        assert_eq!(result.content, "Part 1 Part 2");
        assert_eq!(result.session_id, Some("sess-789".to_string()));
        assert_eq!(result.input_tokens, 150);
        assert_eq!(result.output_tokens, 75);
    }

    #[test]
    fn test_parse_stream_json_empty_output() {
        let output = "";
        let result = parse_stream_json(output);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_stream_json_no_content() {
        let output = r#"{"type":"system","message":"starting"}"#;
        let result = parse_stream_json(output);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_stream_json_unparseable_lines_skipped() {
        let output = [
            "not json at all",
            r#"{"type":"assistant","content":[{"type":"text","text":"Valid content"}],"usage":{"input_tokens":10,"output_tokens":5}}"#,
            "another bad line",
        ]
        .join("\n");

        let result = parse_stream_json(&output).unwrap();
        assert_eq!(result.content, "Valid content");
    }

    #[test]
    fn test_token_aggregation() {
        let output = [
            r#"{"type":"assistant","content":[{"type":"text","text":"A"}],"usage":{"input_tokens":10,"output_tokens":5}}"#,
            r#"{"type":"result","session_id":"s1","usage":{"input_tokens":100,"output_tokens":50}}"#,
        ]
        .join("\n");

        let result = parse_stream_json(&output).unwrap();
        // Last usage values win (they represent cumulative totals)
        assert_eq!(result.input_tokens, 100);
        assert_eq!(result.output_tokens, 50);
    }

    #[tokio::test]
    async fn test_mock_agent() {
        let agent = mock::MockAiAgent::new();
        agent.set_response(
            "spec",
            AiResult {
                content: "# SPEC\n\nContent here".to_string(),
                session_id: Some("sess-1".to_string()),
                input_tokens: 500,
                output_tokens: 300,
            },
        );

        let result = agent
            .invoke(AiInvocation {
                model: "claude-sonnet-4-6".to_string(),
                max_turns: 50,
                effort: "high".to_string(),
                worktree_path: "/tmp/test".to_string(),
                prompt: "Generate a spec for this issue".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(result.content, "# SPEC\n\nContent here");
        assert_eq!(result.input_tokens, 500);
    }

    #[tokio::test]
    async fn test_mock_agent_no_response() {
        let agent = mock::MockAiAgent::new();
        let result = agent
            .invoke(AiInvocation {
                model: "claude-sonnet-4-6".to_string(),
                max_turns: 50,
                effort: "high".to_string(),
                worktree_path: "/tmp/test".to_string(),
                prompt: "something".to_string(),
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_agent_default_response() {
        let agent = mock::MockAiAgent::new();
        agent.set_default_response(AiResult {
            content: "default".to_string(),
            session_id: None,
            input_tokens: 10,
            output_tokens: 5,
        });

        let result = agent
            .invoke(AiInvocation {
                model: "claude-sonnet-4-6".to_string(),
                max_turns: 50,
                effort: "high".to_string(),
                worktree_path: "/tmp/test".to_string(),
                prompt: "anything".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(result.content, "default");
    }
}
