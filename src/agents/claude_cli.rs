//! `ClaudeCliAgent` — drives the Claude CLI (`claude --print ...`) and parses
//! its `stream-json` output.

use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{timeout, Duration, Instant};

use super::{AgentKind, AiAgent, AiInvocation, AiResult};
use crate::error::HammurabiError;

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

        let mut child = tokio::process::Command::new("claude")
            .current_dir(&invocation.worktree_path)
            .arg("--print")
            .arg("--dangerously-skip-permissions")
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| HammurabiError::Ai(format!("failed to spawn claude: {}", e)))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HammurabiError::Ai("failed to capture stdout".to_string()))?;

        let overall_deadline = Instant::now() + Duration::from_secs(invocation.timeout_secs);
        let stall_enabled = invocation.stall_timeout_secs > 0;
        let stall_duration = Duration::from_secs(invocation.stall_timeout_secs);
        let mut reader = BufReader::new(stdout).lines();
        let mut collected_lines = Vec::new();

        loop {
            let remaining = overall_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = child.kill().await;
                return Err(HammurabiError::AiTimeout(format!(
                    "agent exceeded total timeout of {}s",
                    invocation.timeout_secs
                )));
            }

            let line_timeout = if stall_enabled {
                stall_duration.min(remaining)
            } else {
                remaining
            };

            match timeout(line_timeout, reader.next_line()).await {
                Ok(Ok(Some(line))) => {
                    collected_lines.push(line);
                }
                Ok(Ok(None)) => {
                    break;
                }
                Ok(Err(e)) => {
                    let _ = child.kill().await;
                    return Err(HammurabiError::Ai(format!(
                        "error reading claude output: {}",
                        e
                    )));
                }
                Err(_) => {
                    let _ = child.kill().await;
                    if Instant::now() >= overall_deadline {
                        return Err(HammurabiError::AiTimeout(format!(
                            "agent exceeded total timeout of {}s",
                            invocation.timeout_secs
                        )));
                    } else {
                        return Err(HammurabiError::AiTimeout(format!(
                            "agent stalled — no output for {}s",
                            invocation.stall_timeout_secs
                        )));
                    }
                }
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|e| HammurabiError::Ai(format!("failed to wait for claude: {}", e)))?;

        if !status.success() {
            let stderr_output = if let Some(mut stderr) = child.stderr.take() {
                let mut buf = String::new();
                let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut buf).await;
                buf
            } else {
                String::new()
            };
            return Err(HammurabiError::Ai(format!(
                "claude exited with status {}: {}",
                status, stderr_output
            )));
        }

        let full_output = collected_lines.join("\n");
        parse_stream_json(&full_output)
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

        if let Some(sid) = parsed.get("session_id").and_then(|v| v.as_str()) {
            session_id = Some(sid.to_string());
        }

        if let Some(msg_type) = parsed.get("type").and_then(|v| v.as_str()) {
            match msg_type {
                "assistant" => {
                    let content = parsed
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .or_else(|| parsed.get("content"));
                    if let Some(arr) = content.and_then(|c| c.as_array()) {
                        for block in arr {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                content_parts.push(text.to_string());
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

        let usage = parsed
            .get("message")
            .and_then(|m| m.get("usage"))
            .or_else(|| parsed.get("usage"));
        if let Some(usage) = usage {
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
        agent_kind: AgentKind::ClaudeCli,
        tool_summary: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stream_json_assistant_message() {
        let output = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello, world!"}],"usage":{"input_tokens":100,"output_tokens":50}},"session_id":"sess-123"}"#;
        let result = parse_stream_json(output).unwrap();
        assert_eq!(result.content, "Hello, world!");
        assert_eq!(result.session_id, Some("sess-123".to_string()));
        assert_eq!(result.input_tokens, 100);
        assert_eq!(result.output_tokens, 50);
        assert_eq!(result.agent_kind, AgentKind::ClaudeCli);
        assert!(result.tool_summary.is_empty());
    }

    #[test]
    fn test_parse_stream_json_assistant_flat_format() {
        let output = r#"{"type":"assistant","content":[{"type":"text","text":"Hello!"}],"usage":{"input_tokens":10,"output_tokens":5},"session_id":"s1"}"#;
        let result = parse_stream_json(output).unwrap();
        assert_eq!(result.content, "Hello!");
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
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Part 1"}],"usage":{"input_tokens":50,"output_tokens":25}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":" Part 2"}],"usage":{"input_tokens":100,"output_tokens":50}}}"#,
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
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Valid content"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "another bad line",
        ]
        .join("\n");

        let result = parse_stream_json(&output).unwrap();
        assert_eq!(result.content, "Valid content");
    }

    #[test]
    fn test_token_aggregation() {
        let output = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"A"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            r#"{"type":"result","session_id":"s1","usage":{"input_tokens":100,"output_tokens":50}}"#,
        ]
        .join("\n");

        let result = parse_stream_json(&output).unwrap();
        assert_eq!(result.input_tokens, 100);
        assert_eq!(result.output_tokens, 50);
    }
}
