//! Agent Client Protocol (ACP) client.
//!
//! A minimal client that speaks the JSON-RPC-over-stdio dialect defined at
//! <https://agentclientprotocol.com>. We implement only the subset Hammurabi
//! needs to drive spec-compliant agents (Claude via `claude-agent-acp`,
//! Gemini via `gemini --acp`, Codex via `codex-acp`):
//!
//! - outbound: `initialize`, `session/new`, `session/prompt`,
//!   `session/cancel`, `session/set_config_option`
//! - inbound : `session/request_permission` (auto-allow),
//!   `session/update` (streamed progress notifications)
//!
//! Deliberately **not** included: session pooling, `session/load` resumption,
//! image content blocks, MCP wiring, and any non-spec fallbacks (e.g. kiro's
//! `models`/`modes` shape). Those can be added later if real agents demand
//! them.
//!
//! The module was written from the spec rather than ported from any existing
//! implementation.

pub mod events;
pub mod permission;
pub mod session;
pub mod spawn;
pub mod wire;
