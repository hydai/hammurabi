//! ACP session — drives a single subprocess through the
//! `initialize → new → prompt → cancel` lifecycle with full JSON-RPC
//! bookkeeping.
//!
//! Designed for Hammurabi's one-shot usage pattern: each transition spawns
//! a fresh session, runs exactly one prompt, and tears it down. No pooling,
//! no `session/load` resumption.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::permission;
use super::spawn;
use super::wire::{
    parse_frame, IncomingMessage, Method, Notification, Request, Response, RpcError,
};
use crate::error::HammurabiError;

/// Timeout for ordinary request/response exchanges. `session/new` gets a
/// longer deadline below because some agents (Claude via npm wrapper) warm
/// up slowly on first run.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const NEW_SESSION_TIMEOUT_SECS: u64 = 120;

/// Shared map of in-flight request ids to the oneshot awaiting their reply.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// Shared slot for the current prompt's notification subscriber.
type NotifySlot = Arc<Mutex<Option<mpsc::UnboundedSender<Notification>>>>;

/// Configuration for launching an ACP agent subprocess.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AcpAgentDef {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Information returned from `initialize` that the caller may care about.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct InitInfo {
    pub agent_name: String,
    pub protocol_version: u64,
    pub supports_load_session: bool,
}

/// A live ACP subprocess and the bookkeeping needed to drive it.
pub struct Session {
    child: Child,
    pgid: Option<i32>,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: PendingMap,
    notify_tx: NotifySlot,
    session_id: Option<String>,
    reader_handle: Option<JoinHandle<()>>,
}

impl Session {
    /// Spawn the subprocess and start the background reader task.
    pub async fn start(def: &AcpAgentDef, cwd: &Path) -> Result<Self, HammurabiError> {
        let cwd_str = cwd
            .to_str()
            .ok_or_else(|| HammurabiError::Config(format!("non-UTF8 worktree path: {cwd:?}")))?;

        let (mut child, pgid) = spawn::spawn_child(&def.command, &def.args, cwd_str, &def.env)?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HammurabiError::Acp("failed to capture ACP agent stdout".to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HammurabiError::Acp("failed to capture ACP agent stdin".to_string()))?;
        let stdin = Arc::new(Mutex::new(stdin));

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: NotifySlot = Arc::new(Mutex::new(None));

        let reader_handle = spawn_reader(stdout, stdin.clone(), pending.clone(), notify_tx.clone());

        tracing::info!(
            command = %def.command,
            args = ?def.args,
            cwd = cwd_str,
            "ACP agent spawned"
        );

        Ok(Self {
            child,
            pgid,
            stdin,
            next_id: AtomicU64::new(1),
            pending,
            notify_tx,
            session_id: None,
            reader_handle: Some(reader_handle),
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a request and await its response. Used for all typed exchanges
    /// except `session/prompt` (which uses its own streaming path).
    async fn call(&self, method: Method, params: Option<Value>) -> Result<Value, HammurabiError> {
        let id = self.next_id();
        let timeout_secs = match method {
            Method::SessionNew => NEW_SESSION_TIMEOUT_SECS,
            _ => DEFAULT_REQUEST_TIMEOUT_SECS,
        };
        let method_str = method.as_wire().to_string();

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let req = Request::new(id, method, params);
        let line = serde_json::to_string(&req)
            .map_err(|e| HammurabiError::Acp(format!("serialize {method_str}: {e}")))?;
        self.send_line(&line).await?;

        let resp = timeout(Duration::from_secs(timeout_secs), rx)
            .await
            .map_err(|_| HammurabiError::Acp(format!("timeout waiting for {method_str} response")))?
            .map_err(|_| HammurabiError::Acp(format!("channel closed awaiting {method_str}")))?;

        resp.map_err(|e| HammurabiError::Acp(format!("{method_str}: {e}")))
    }

    async fn send_line(&self, line: &str) -> Result<(), HammurabiError> {
        tracing::debug!(line = line, "acp_send");
        let mut w = self.stdin.lock().await;
        write_all_or_map_closed(&mut w, line.as_bytes()).await?;
        write_all_or_map_closed(&mut w, b"\n").await?;
        w.flush().await.map_err(map_closed)?;
        Ok(())
    }

    /// Perform the ACP `initialize` handshake.
    pub async fn initialize(&mut self) -> Result<InitInfo, HammurabiError> {
        let result = self
            .call(
                Method::Initialize,
                Some(json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {
                        "name": "hammurabi",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
            )
            .await?;

        let agent_name = result
            .get("agentInfo")
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_string();
        let protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        let supports_load_session = result
            .get("agentCapabilities")
            .and_then(|c| c.get("loadSession"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        tracing::info!(
            agent = %agent_name,
            version = protocol_version,
            load_session = supports_load_session,
            "ACP initialize complete"
        );

        Ok(InitInfo {
            agent_name,
            protocol_version,
            supports_load_session,
        })
    }

    /// Create a fresh session. Stores and returns the agent-assigned id.
    pub async fn new_session(&mut self, cwd: &Path) -> Result<String, HammurabiError> {
        let cwd_str = cwd
            .to_str()
            .ok_or_else(|| HammurabiError::Config(format!("non-UTF8 worktree path: {cwd:?}")))?;
        let result = self
            .call(
                Method::SessionNew,
                Some(json!({"cwd": cwd_str, "mcpServers": []})),
            )
            .await?;

        let session_id = result
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                HammurabiError::Acp("session/new response missing sessionId".to_string())
            })?
            .to_string();

        tracing::info!(session_id = %session_id, "ACP session created");
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    /// Attempt to set a configuration option (typically the model). Best-effort:
    /// if the agent rejects the call, we log and return `Ok(())` so the
    /// caller can continue with whatever default the agent chose.
    pub async fn set_config_option(&mut self, id: &str, value: &str) -> Result<(), HammurabiError> {
        let session_id = self.require_session_id()?;
        let params = json!({
            "sessionId": session_id,
            "configId": id,
            "value": value,
        });
        match self
            .call(Method::SessionSetConfigOption, Some(params))
            .await
        {
            Ok(_) => {
                tracing::info!(id, value, "ACP set_config_option ok");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(id, value, error = %e, "ACP set_config_option failed, continuing");
                Ok(())
            }
        }
    }

    /// Send a text-only prompt and return a notification receiver plus the
    /// request id. The caller drains the receiver until a
    /// [`Notification`] with `method = prompt_completion(id)` arrives, or a
    /// channel close indicates agent death.
    pub async fn prompt(
        &mut self,
        text: &str,
    ) -> Result<(mpsc::UnboundedReceiver<Notification>, u64), HammurabiError> {
        let session_id = self.require_session_id()?;

        let (tx, rx) = mpsc::unbounded_channel();
        *self.notify_tx.lock().await = Some(tx);

        let id = self.next_id();
        let req = Request::new(
            id,
            Method::SessionPrompt,
            Some(json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}],
            })),
        );

        // Install a oneshot so the reader task still removes the id from
        // `pending` when the response arrives. We drop the receiver; the
        // reader task also pushes a synthetic completion notification onto
        // the mpsc channel so the caller sees the turn end.
        let (completion_tx, _completion_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, completion_tx);

        let line = serde_json::to_string(&req)
            .map_err(|e| HammurabiError::Acp(format!("serialize prompt: {e}")))?;
        self.send_line(&line).await?;

        Ok((rx, id))
    }

    /// Tear down the current notification subscriber after a prompt run.
    pub async fn end_prompt(&mut self) {
        *self.notify_tx.lock().await = None;
    }

    /// Ask the agent to abandon the in-flight prompt (spec: `session/cancel`
    /// is a fire-and-forget notification, no response id).
    pub async fn cancel(&self) -> Result<(), HammurabiError> {
        let session_id = match &self.session_id {
            Some(id) => id.clone(),
            None => return Ok(()),
        };
        let notif = Notification::new(
            Method::SessionCancel,
            Some(json!({"sessionId": session_id})),
        );
        let line = serde_json::to_string(&notif)
            .map_err(|e| HammurabiError::Acp(format!("serialize cancel: {e}")))?;
        self.send_line(&line).await
    }

    fn require_session_id(&self) -> Result<String, HammurabiError> {
        self.session_id.clone().ok_or_else(|| {
            HammurabiError::Acp("session/new must be called before this method".to_string())
        })
    }

    /// Test accessor: is the reader task still alive?
    #[cfg(test)]
    pub fn reader_alive(&self) -> bool {
        self.reader_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // On Unix, kill the whole subtree. On Windows, fall back to the
        // tokio Child kill API (best-effort).
        if let Some(pgid) = self.pgid {
            spawn::kill_subtree(Some(pgid));
        } else {
            let _ = self.child.start_kill();
        }
        // Detach the reader task; it will exit when stdout closes.
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
    }
}

/// Write all bytes; remap broken-pipe / unexpected-EOF into a semantic
/// ACP "connection closed" error so callers can distinguish agent death
/// from generic IO failures.
async fn write_all_or_map_closed(w: &mut ChildStdin, bytes: &[u8]) -> Result<(), HammurabiError> {
    w.write_all(bytes).await.map_err(map_closed)
}

fn map_closed(e: std::io::Error) -> HammurabiError {
    match e.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof => {
            HammurabiError::Acp("ACP connection closed".to_string())
        }
        _ => HammurabiError::Io(e),
    }
}

/// Spawn the background reader task. Its job:
///
/// 1. Parse every incoming frame.
/// 2. If the agent asked for permission, auto-respond via [`permission::build_response`].
/// 3. If the frame is a response to one of our outbound requests, resolve
///    the matching oneshot and (for streaming prompts) forward a synthetic
///    completion notification so the caller sees the turn end.
/// 4. If the frame is a vanilla notification, forward it to the current
///    subscriber (if any).
fn spawn_reader(
    stdout: tokio::process::ChildStdout,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
    notify_tx: NotifySlot,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("ACP reader error: {e}");
                    break;
                }
            }
            tracing::debug!(line = line.trim(), "acp_recv");

            let frame = match parse_frame(&line) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(line = line.trim(), error = %e, "ACP frame parse failed");
                    continue;
                }
            };

            match frame {
                IncomingMessage::Request { id, method, params } => {
                    if method == Method::SessionRequestPermission {
                        let outcome = permission::build_response(params.as_ref());
                        let resp = Response::new(id, outcome);
                        if let Ok(s) = serde_json::to_string(&resp) {
                            let mut w = stdin.lock().await;
                            let _ = w.write_all(s.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                            let _ = w.flush().await;
                        }
                    } else {
                        tracing::warn!(method = ?method, "ACP agent sent unsupported request");
                    }
                }
                IncomingMessage::Response { id, outcome } => {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        // Publish a synthetic completion notification so a
                        // streaming prompt consumer can detect end-of-turn
                        // and extract usage stats without needing a second
                        // channel.
                        if let Some(n) = notify_tx.lock().await.as_ref() {
                            let params = match &outcome {
                                Ok(v) => Some(serde_json::json!({"result": v})),
                                Err(e) => Some(serde_json::json!({
                                    "error": {"code": e.code, "message": e.message}
                                })),
                            };
                            let completion = Notification {
                                jsonrpc: super::wire::JSONRPC_VERSION,
                                method: format!("__prompt_completion:{id}"),
                                params,
                            };
                            let _ = n.send(completion);
                        }
                        let _ = tx.send(outcome);
                    } else {
                        tracing::warn!(id, "ACP response for unknown id");
                    }
                }
                IncomingMessage::Notification { method: _, params } => {
                    if let Some(n) = notify_tx.lock().await.as_ref() {
                        let notif = Notification {
                            jsonrpc: super::wire::JSONRPC_VERSION,
                            method: "session/update".to_string(),
                            params,
                        };
                        let _ = n.send(notif);
                    }
                }
            }
        }

        // Stream ended. Fail any in-flight requests.
        let mut map = pending.lock().await;
        for (_, tx) in map.drain() {
            let _ = tx.send(Err(RpcError {
                code: -1,
                message: "ACP connection closed".to_string(),
            }));
        }
        // Close the current subscriber.
        *notify_tx.lock().await = None;
    })
}

/// Returns true if a notification is the synthetic prompt-completion marker
/// produced by the reader task for the given request id.
#[allow(dead_code)]
pub fn is_prompt_completion(notif: &Notification, id: u64) -> bool {
    notif.method == format!("__prompt_completion:{id}")
}
