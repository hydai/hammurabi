//! JSON-RPC 2.0 framing for ACP.
//!
//! ACP frames are newline-delimited JSON objects. Every frame is one of:
//!
//! - **Request** — has `id` and `method`, optionally `params`. Awaits a response.
//! - **Response** — has `id` and exactly one of `result` / `error`. Matches a request.
//! - **Notification** — has `method` but no `id`. Fire-and-forget.
//!
//! We parse incoming frames into a single [`IncomingMessage`] enum so the
//! reader task can route at a glance rather than re-inspecting the raw JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The JSON-RPC 2.0 version string. Hardcoded — we never negotiate.
pub const JSONRPC_VERSION: &str = "2.0";

/// Typed ACP method names. `Other` is a forward-compat escape hatch for
/// anything an agent might emit that we do not model explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Initialize,
    SessionNew,
    SessionPrompt,
    SessionCancel,
    SessionSetConfigOption,
    SessionRequestPermission,
    SessionUpdate,
    Other(String),
}

impl Method {
    pub fn as_wire(&self) -> &str {
        match self {
            Method::Initialize => "initialize",
            Method::SessionNew => "session/new",
            Method::SessionPrompt => "session/prompt",
            Method::SessionCancel => "session/cancel",
            Method::SessionSetConfigOption => "session/set_config_option",
            Method::SessionRequestPermission => "session/request_permission",
            Method::SessionUpdate => "session/update",
            Method::Other(s) => s.as_str(),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "initialize" => Method::Initialize,
            "session/new" => Method::SessionNew,
            "session/prompt" => Method::SessionPrompt,
            "session/cancel" => Method::SessionCancel,
            "session/set_config_option" => Method::SessionSetConfigOption,
            "session/request_permission" => Method::SessionRequestPermission,
            "session/update" => Method::SessionUpdate,
            other => Method::Other(other.to_string()),
        }
    }
}

/// Outgoing request.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: u64, method: Method, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            method: method.as_wire().to_string(),
            params,
        }
    }
}

/// Outgoing response (used only when the agent asks *us* for something, e.g.
/// permission approval).
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub result: Value,
}

impl Response {
    pub fn new(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result,
        }
    }
}

/// Outgoing notification (no id, no response expected).
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    pub fn new(method: Method, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            method: method.as_wire().to_string(),
            params,
        }
    }
}

/// Structured JSON-RPC error payload.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

/// Raw shape we decode from any incoming frame before classification.
#[derive(Debug, Deserialize)]
struct RawFrame {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// Classified incoming frame. Each variant carries only the fields relevant
/// to that kind — callers do not need to re-check `id` / `method` presence.
#[derive(Debug)]
pub enum IncomingMessage {
    /// Agent is calling back (e.g. permission request).
    Request {
        id: u64,
        method: Method,
        params: Option<Value>,
    },
    /// Response to one of our outbound requests.
    Response {
        id: u64,
        outcome: Result<Value, RpcError>,
    },
    /// Fire-and-forget notification (streamed progress, etc.).
    Notification {
        method: Method,
        params: Option<Value>,
    },
}

/// Parse one newline-delimited JSON frame. Returns `Ok(None)` for empty /
/// whitespace-only lines, `Err` for malformed JSON, and
/// `Ok(Some(Malformed))` is not exposed — we prefer an explicit error.
pub fn parse_frame(line: &str) -> Result<Option<IncomingMessage>, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let raw: RawFrame =
        serde_json::from_str(trimmed).map_err(|e| format!("unparseable JSON-RPC frame: {e}"))?;

    match (raw.id, raw.method.as_deref()) {
        (Some(id), Some(method)) => Ok(Some(IncomingMessage::Request {
            id,
            method: Method::parse(method),
            params: raw.params,
        })),
        (Some(id), None) => {
            let outcome = match (raw.result, raw.error) {
                (_, Some(err)) => Err(err),
                (Some(result), None) => Ok(result),
                (None, None) => Ok(Value::Null),
            };
            Ok(Some(IncomingMessage::Response { id, outcome }))
        }
        (None, Some(method)) => Ok(Some(IncomingMessage::Notification {
            method: Method::parse(method),
            params: raw.params,
        })),
        (None, None) => Err("JSON-RPC frame has neither id nor method".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn method_round_trip() {
        for m in [
            Method::Initialize,
            Method::SessionNew,
            Method::SessionPrompt,
            Method::SessionCancel,
            Method::SessionSetConfigOption,
            Method::SessionRequestPermission,
            Method::SessionUpdate,
        ] {
            assert_eq!(Method::parse(m.as_wire()), m);
        }
    }

    #[test]
    fn method_parse_unknown_returns_other() {
        match Method::parse("session/subscribe") {
            Method::Other(s) => assert_eq!(s, "session/subscribe"),
            m => panic!("expected Other, got {:?}", m),
        }
    }

    #[test]
    fn request_serialization_omits_missing_params() {
        let req = Request::new(1, Method::Initialize, None);
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("params"));
        assert!(s.contains("\"method\":\"initialize\""));
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
    }

    #[test]
    fn request_serialization_includes_params() {
        let req = Request::new(3, Method::SessionNew, Some(json!({"cwd": "/tmp"})));
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"cwd\":\"/tmp\""));
    }

    #[test]
    fn parse_frame_classifies_response_ok() {
        let line = r#"{"jsonrpc":"2.0","id":5,"result":{"sessionId":"abc"}}"#;
        match parse_frame(line).unwrap().unwrap() {
            IncomingMessage::Response { id, outcome } => {
                assert_eq!(id, 5);
                let v = outcome.unwrap();
                assert_eq!(v.get("sessionId").and_then(|v| v.as_str()), Some("abc"));
            }
            other => panic!("expected Response, got {:?}", other),
        }
    }

    #[test]
    fn parse_frame_classifies_response_error() {
        let line =
            r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"Method not found"}}"#;
        match parse_frame(line).unwrap().unwrap() {
            IncomingMessage::Response { id, outcome } => {
                assert_eq!(id, 5);
                let err = outcome.unwrap_err();
                assert_eq!(err.code, -32601);
                assert_eq!(err.message, "Method not found");
            }
            other => panic!("expected Response, got {:?}", other),
        }
    }

    #[test]
    fn parse_frame_classifies_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk"}}}"#;
        match parse_frame(line).unwrap().unwrap() {
            IncomingMessage::Notification { method, params } => {
                assert_eq!(method, Method::SessionUpdate);
                assert!(params.is_some());
            }
            other => panic!("expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn parse_frame_classifies_request() {
        let line = r#"{"jsonrpc":"2.0","id":42,"method":"session/request_permission","params":{}}"#;
        match parse_frame(line).unwrap().unwrap() {
            IncomingMessage::Request { id, method, .. } => {
                assert_eq!(id, 42);
                assert_eq!(method, Method::SessionRequestPermission);
            }
            other => panic!("expected Request, got {:?}", other),
        }
    }

    #[test]
    fn parse_frame_empty_line_is_none() {
        assert!(parse_frame("").unwrap().is_none());
        assert!(parse_frame("   \n").unwrap().is_none());
    }

    #[test]
    fn parse_frame_rejects_malformed_json() {
        assert!(parse_frame("not json").is_err());
    }

    #[test]
    fn parse_frame_rejects_missing_id_and_method() {
        let line = r#"{"jsonrpc":"2.0"}"#;
        assert!(parse_frame(line).is_err());
    }
}
