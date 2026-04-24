# Hammurabi

## Project Overview

Hammurabi is a Rust CLI daemon that monitors one or more GitHub repositories' issue boards and orchestrates an AI agent to automate the issue lifecycle (spec drafting → approval → implementation) with mandatory human approval at every step. The default agent is the Claude CLI; `agent_kind = "acp-claude" | "acp-gemini" | "acp-codex"` opts a repo or individual task into an Agent Client Protocol subprocess instead.

In addition to GitHub label polling, Hammurabi can accept ideas from chat intake sources — today Discord, designed so additional platforms slot in behind the same `DiscordClient` / `Publisher` shape. A Discord-originated intake creates a thread, drafts the spec interactively, and opens a GitHub issue on `/confirm`; the existing implementation/review/PR pipeline then runs unchanged.

## Build & Test

```bash
cargo build --release    # Always use release builds
cargo test               # Run all unit + integration tests
```

## Architecture

- **Pure state machine** (`src/state_machine.rs`) -- all transitions are `(State, Event) -> Vec<SideEffect>` with no I/O
- **Trait-based abstractions** -- `GitHubClient`, `DiscordClient`, `AiAgent`, `WorktreeManager`, `Publisher` traits enable mock-based testing
- **Multi-repo + multi-intake support** -- `Config` holds `Vec<RepoConfig>` (GitHub label polling) plus `Vec<SourceEntry>` (chat intakes like Discord); canonical issue identity is `(source, repo, external_id)` where `external_id` is the GitHub issue number or Discord thread snowflake
- **Database** (`src/db.rs`) -- SQLite with WAL mode, wrapped in `Mutex` for thread safety; `UNIQUE(source, repo, external_id)` scopes rows across sources
- **Publisher** (`src/publisher.rs`) -- source-agnostic progress abstraction. `GithubPublisher` posts issue comments; `DiscordPublisher` posts thread messages; `MultiplexPublisher` fans out. `TransitionContext::publisher_for(issue)` picks the right one at the call site
- **Transitions** (`src/transitions/`) -- one module per active state, each performing the actual work; `spec_drafting` branches on `issue.source`
- **Poller** (`src/poller.rs`) -- main daemon loop that iterates over all configured repos each cycle; `discord_intake_once` handles new @mentions for a configured Discord channel

## Key Conventions

- All external dependencies are behind traits (GitHub, Discord, AI, git worktrees, Publisher)
- Test with mocks in `#[cfg(test)] mod mock` blocks within each module
- State machine tests must be exhaustive -- one test per valid transition
- Integration tests live in `tests/` and use `#[path = ...]` to import modules
- `rusqlite` is used (not sqlx) per design spec -- synchronous access wrapped in `Mutex`
- Agents live behind `AgentKind` + `AgentRegistry` — add a new kind by extending `src/agents/mod.rs::AgentKind`, registering it in `poller::build_agent_registry`, and (for ACP) supplying an `AcpAgentDef` default in `src/agents/acp.rs`
- Intake sources live behind `DiscordClient` (and peer traits for future platforms) — a Discord-sourced row has `source=Discord`, `external_id=<thread_id>`, and `github_issue_number=0` until `/confirm` opens the GitHub issue
- Secrets in config (`bot_token`, etc.) expand via `${VAR}` from the environment and use a manual `Debug` impl so they never leak into logs

## File Layout

```
src/
├── main.rs              # CLI entry point
├── access.rs            # AllowUsers enum (List | All) + RawAccess deserializer
├── config.rs            # TOML config + env overrides (Config + RepoConfig + DiscordChannelConfig,
│                        # supports [[repos]] array, [[sources]] kind="discord" blocks,
│                        # agent_kind selection, [agents.*] subprocess overrides, ${VAR} expansion)
├── db.rs                # SQLite schema + CRUD; UNIQUE(source, repo, external_id) identity
├── discord.rs           # DiscordClient trait + DiscordMessage/DiscordThreadRef types + MockDiscordClient
├── models.rs            # IssueState, SourceKind, TrackedIssue (with is_discord_pending / external_id_u64)
├── state_machine.rs     # Pure transition function
├── github.rs            # GitHubClient trait + OctocrabClient
├── publisher.rs         # Publisher trait + GithubPublisher / DiscordPublisher / MultiplexPublisher
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
├── approval.rs          # Approval gate checking (GitHub /approve + Discord /confirm, /revise, /cancel)
├── hooks.rs             # Workspace lifecycle hooks (after_create, before_run, after_run, before_remove)
├── prompts.rs           # AI prompt templates
├── poller.rs            # Daemon main loop + Discord intake (discord_intake_once, ensure_github_issue,
│                        # handle_await_spec_approval_discord)
├── lock.rs              # PID-based lock file
├── error.rs             # Error types (HammurabiError::{Ai, AiTimeout, Acp, ...})
└── transitions/
    ├── mod.rs           # TransitionContext, run_ai_lifecycle, seed_filename,
    │                    # publisher_for(issue) / thread_id_for(issue)
    ├── progress.rs      # Live status-message aggregator (Publisher-backed, source-agnostic)
    ├── spec_drafting.rs # Source-aware: GitHub fetches issue body; Discord uses thread pitch
    ├── implementing.rs
    ├── reviewing.rs
    └── completion.rs

tests/
├── integration_lifecycle.rs         # Happy-path GitHub-sourced issue lifecycle
├── integration_error_handling.rs    # Failure/retry paths
├── integration_discord_lifecycle.rs # Discord intake → spec refine → /confirm → merge → Done
├── acp_client_integration.rs        # Drives the fake-acp-agent binary end-to-end
└── support/
    └── fake_acp_agent/main.rs       # Scripted fake ACP agent (CARGO_BIN_EXE_fake-acp-agent)
```

## Discord intake status

Phase 4 ships the complete Discord-sourced lifecycle under mocks — the
state machine, approval grammar, intake handler, and end-to-end test are
all in-tree. The Serenity-backed real-runtime wiring (`SerenityDiscordClient`
+ a Gateway task spawned from `run_daemon`) is a future PR: the
`[[sources]]` config currently parses and validates, but the poll loop
does not yet drive `discord_intake_once` against a live token. See the
plan at `~/.claude/plans/analyze-tmp-openab-i-would-ticklish-squid.md`
for the staged rollout.
