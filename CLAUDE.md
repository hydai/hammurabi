# Hammurabi

## Project Overview

Hammurabi is a Rust CLI daemon that monitors one or more GitHub repositories' issue boards and orchestrates an AI agent to automate the issue lifecycle (spec drafting → approval → implementation) with mandatory human approval at every step. The default agent is the Claude CLI; `agent_kind = "acp-claude" | "acp-gemini" | "acp-codex"` opts a repo or individual task into an Agent Client Protocol subprocess instead.

## Build & Test

```bash
cargo build --release    # Always use release builds
cargo test               # Run all unit + integration tests
```

## Architecture

- **Pure state machine** (`src/state_machine.rs`) -- all transitions are `(State, Event) -> Vec<SideEffect>` with no I/O
- **Trait-based abstractions** -- `GitHubClient`, `AiAgent`, `WorktreeManager` traits enable mock-based testing
- **Multi-repo support** -- `Config` holds a `Vec<RepoConfig>`, each repo gets its own GitHub client + worktree manager
- **Database** (`src/db.rs`) -- SQLite with WAL mode, wrapped in `Mutex` for thread safety; `repo` column scopes issues
- **Transitions** (`src/transitions/`) -- one module per active state, each performing the actual work
- **Poller** (`src/poller.rs`) -- main daemon loop that iterates over all configured repos each cycle

## Key Conventions

- All external dependencies are behind traits (GitHub, AI, git worktrees)
- Test with mocks in `#[cfg(test)] mod mock` blocks within each module
- State machine tests must be exhaustive -- one test per valid transition
- Integration tests live in `tests/` and use `#[path = ...]` to import modules
- `rusqlite` is used (not sqlx) per design spec -- synchronous access wrapped in `Mutex`
- Agents live behind `AgentKind` + `AgentRegistry` — add a new kind by extending `src/agents/mod.rs::AgentKind`, registering it in `poller::build_agent_registry`, and (for ACP) supplying an `AcpAgentDef` default in `src/agents/acp.rs`

## File Layout

```
src/
├── main.rs              # CLI entry point
├── config.rs            # TOML config + env overrides (Config + RepoConfig, supports [[repos]] array,
│                        # agent_kind selection, [agents.*] subprocess overrides)
├── db.rs                # SQLite schema + CRUD
├── models.rs            # IssueState, data structs
├── state_machine.rs     # Pure transition function
├── github.rs            # GitHubClient trait + OctocrabClient
├── agents/              # AiAgent trait + concrete impls + AgentRegistry
│   ├── mod.rs           # trait, AiInvocation, AiResult, AgentKind, AgentEvent
│   ├── claude_cli.rs    # ClaudeCliAgent (streaming stdout with timeout/stall detection)
│   ├── acp.rs           # AcpAgent (drives acp::Session, forwards events)
│   ├── registry.rs      # AgentRegistry — dispatch by kind
│   └── mock.rs          # MockAiAgent (test only)
├── acp/                 # Minimal ACP (Agent Client Protocol) client — original, not ported
│   ├── mod.rs
│   ├── wire.rs          # JSON-RPC framing + typed Method enum
│   ├── session.rs       # One-shot session: spawn → initialize → new → prompt → cancel
│   ├── events.rs        # session/update -> AgentEvent classifier
│   ├── permission.rs    # auto-allow policy for session/request_permission
│   └── spawn.rs         # cross-platform child spawn + process-group kill
├── worktree.rs          # WorktreeManager trait + GitWorktreeManager
├── approval.rs          # Approval gate checking
├── hooks.rs             # Workspace lifecycle hooks (after_create, before_run, after_run, before_remove)
├── prompts.rs           # AI prompt templates
├── poller.rs            # Daemon main loop (builds AgentRegistry, concurrent processing)
├── lock.rs              # PID-based lock file
├── error.rs             # Error types (HammurabiError::{Ai, AiTimeout, Acp, ...})
└── transitions/
    ├── mod.rs           # TransitionContext, run_ai_lifecycle, seed_filename
    ├── progress.rs      # Live GitHub-comment aggregator for ACP AgentEvents
    ├── spec_drafting.rs
    ├── implementing.rs
    ├── reviewing.rs
    └── completion.rs

tests/
├── integration_lifecycle.rs         # Happy-path issue lifecycle with MockAiAgent
├── integration_error_handling.rs    # Failure/retry paths
├── acp_client_integration.rs        # Drives the fake-acp-agent binary end-to-end
└── support/
    └── fake_acp_agent/main.rs       # Scripted fake ACP agent (CARGO_BIN_EXE_fake-acp-agent)
```
