//! A minimal, scripted ACP agent used as a subprocess for Hammurabi's
//! integration tests. Never intended to be a real agent — it just replies
//! to the spec methods Hammurabi calls with canned outputs.
//!
//! Behavior is selected via the `HAMMURABI_FAKE_SCENARIO` env var:
//!
//! | value              | effect                                                              |
//! |--------------------|---------------------------------------------------------------------|
//! | `happy` (default)  | Replies OK, streams two text chunks, reports usage                  |
//! | `permission`       | Emits a `session/request_permission` and requires it be auto-allowed |
//! | `tool_calls`       | Streams a tool_call + completed tool_call_update around text         |
//! | `stall`            | Responds to initialize, then goes silent                             |
//! | `timeout`          | Never responds (exceeds overall timeout)                             |
//! | `error`            | Returns JSON-RPC error on session/new                                |
//! | `no_content`       | Completes the prompt with empty content                              |
//! | `crash`            | Exits immediately after initialize                                   |
//!
//! Written fresh for this test harness; does not derive from any external code.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

fn main() {
    let scenario = std::env::var("HAMMURABI_FAKE_SCENARIO").unwrap_or_else(|_| "happy".to_string());

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut handled_initialize = false;
    let mut handled_session_new = false;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = req.get("id").and_then(|v| v.as_u64());
        let method = req
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match method.as_str() {
            "initialize" => {
                if let Some(id) = id {
                    reply_ok(
                        &mut out,
                        id,
                        json!({
                            "agentInfo": {"name": "fake-acp-agent"},
                            "protocolVersion": 1,
                            "agentCapabilities": {"loadSession": false}
                        }),
                    );
                }
                handled_initialize = true;
                if scenario == "crash" {
                    std::process::exit(0);
                }
            }

            "session/new" => {
                if scenario == "error" {
                    if let Some(id) = id {
                        reply_err(&mut out, id, -32001, "fake agent refused new session");
                    }
                    continue;
                }
                if let Some(id) = id {
                    reply_ok(
                        &mut out,
                        id,
                        json!({
                            "sessionId": "fake-session-1",
                            "configOptions": []
                        }),
                    );
                }
                handled_session_new = true;
            }

            "session/set_config_option" => {
                // Always accept.
                if let Some(id) = id {
                    reply_ok(&mut out, id, json!({}));
                }
            }

            "session/prompt" => {
                if !handled_initialize || !handled_session_new {
                    if let Some(id) = id {
                        reply_err(&mut out, id, -32000, "prompt before initialize/session_new");
                    }
                    continue;
                }

                match scenario.as_str() {
                    "happy" => happy_prompt(&mut out, id.unwrap_or(0)),
                    "permission" => permission_prompt(&mut out, id.unwrap_or(0)),
                    "tool_calls" => tool_call_prompt(&mut out, id.unwrap_or(0)),
                    "stall" => {
                        // Respond to nothing; caller's stall timeout trips.
                        std::thread::sleep(Duration::from_secs(600));
                    }
                    "timeout" => {
                        // Same as stall but with no output at all.
                        std::thread::sleep(Duration::from_secs(600));
                    }
                    "no_content" => no_content_prompt(&mut out, id.unwrap_or(0)),
                    _ => happy_prompt(&mut out, id.unwrap_or(0)),
                }
            }

            "session/cancel" => {
                // Notification; no response expected.
            }

            _ => {
                // Unknown request: reply with error if there's an id.
                if let Some(id) = id {
                    reply_err(&mut out, id, -32601, "method not implemented in fake");
                }
            }
        }
    }
}

fn write_line(out: &mut impl Write, value: &Value) {
    let s = serde_json::to_string(value).expect("serialize JSON");
    let _ = out.write_all(s.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn reply_ok(out: &mut impl Write, id: u64, result: Value) {
    write_line(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn reply_err(out: &mut impl Write, id: u64, code: i64, message: &str) {
    write_line(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message}
        }),
    );
}

fn send_update(out: &mut impl Write, update: Value) {
    write_line(
        out,
        &json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"update": update}
        }),
    );
}

fn happy_prompt(out: &mut impl Write, prompt_id: u64) {
    send_update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": "Hello from "}
        }),
    );
    send_update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": "the fake agent."}
        }),
    );
    reply_ok(
        out,
        prompt_id,
        json!({
            "stopReason": "end_turn",
            "usage": {"inputTokens": 42, "outputTokens": 17}
        }),
    );
}

fn permission_prompt(out: &mut impl Write, prompt_id: u64) {
    // Issue a permission request first. We pick a fixed id unlikely to
    // clash with the client's ids. Ignore the reply (if the client is
    // correctly auto-allowing, it will send one; otherwise we just
    // continue anyway).
    let perm_id = 999_000_001u64;
    write_line(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": perm_id,
            "method": "session/request_permission",
            "params": {
                "toolCall": {"title": "Run risky command"},
                "options": [
                    {"optionId": "yes", "kind": "allow_always"},
                    {"optionId": "no", "kind": "reject_once"}
                ]
            }
        }),
    );

    // Give the client time to respond.
    std::thread::sleep(Duration::from_millis(50));

    send_update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": "Permission granted, result here."}
        }),
    );
    reply_ok(
        out,
        prompt_id,
        json!({"stopReason": "end_turn", "usage": {"inputTokens": 10, "outputTokens": 5}}),
    );
}

fn tool_call_prompt(out: &mut impl Write, prompt_id: u64) {
    send_update(
        out,
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "tc-alpha",
            "title": "Read foo.rs"
        }),
    );
    send_update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": "Reading... "}
        }),
    );
    send_update(
        out,
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "tc-alpha",
            "title": "Read foo.rs",
            "status": "completed"
        }),
    );
    send_update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": "done."}
        }),
    );
    reply_ok(
        out,
        prompt_id,
        json!({"stopReason": "end_turn", "usage": {"inputTokens": 20, "outputTokens": 10}}),
    );
}

fn no_content_prompt(out: &mut impl Write, prompt_id: u64) {
    // No chunks at all — just a completion.
    reply_ok(
        out,
        prompt_id,
        json!({"stopReason": "end_turn", "usage": {"inputTokens": 1, "outputTokens": 0}}),
    );
}

// Prevent unused warnings when HashMap isn't used in certain scenarios.
#[allow(dead_code)]
fn _ensure_hashmap_link() -> HashMap<String, String> {
    HashMap::new()
}
