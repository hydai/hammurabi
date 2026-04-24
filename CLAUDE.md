# Hammurabi

Guidance for AI assistants working on this repository. End-user
documentation lives in `README.md`, `install.md`, and
`getting-started.md`. Contributor-facing architecture notes and the
full module layout are in `docs/architecture.md`.

## Project overview

Hammurabi is a Rust CLI daemon that monitors one or more GitHub
repositories' issue boards and orchestrates an AI agent to automate
the issue lifecycle (spec drafting → approval → implementation →
self-review) with mandatory human approval at every gate. The default
agent is the Claude CLI; `agent_kind = "acp-claude" | "acp-gemini" |
"acp-codex"` opts a repo or individual task into an Agent Client
Protocol subprocess.

In addition to GitHub label polling, Hammurabi accepts ideas from chat
intake sources — today Discord (behind the `discord` Cargo feature),
designed so additional platforms slot in behind the same
`DiscordClient` / `Publisher` shape.

## Build & test

```bash
cargo build --release               # Always use release builds
cargo build --release --features discord
cargo test
```

## Architectural invariants (must preserve)

- **Pure state machine** (`src/state_machine.rs`) — `(State, Event) →
  Vec<SideEffect>` with no I/O. The exhaustive test suite there is
  the authoritative spec; runtime dispatches through
  `src/transitions/*` in lockstep.
- **All external deps behind traits** — `GitHubClient`,
  `DiscordClient`, `AiAgent`, `WorktreeManager`, `Publisher`. Tests
  use mocks declared in `#[cfg(test)] mod mock` within each module.
- **Canonical issue identity is `(source, repo, external_id)`** —
  `source` is `SourceKind::{GitHub, Discord}`; `external_id` is the
  GitHub issue number or the Discord thread snowflake. SQLite enforces
  `UNIQUE(source, repo, external_id)`.
- **`rusqlite` is used, not `sqlx`** — synchronous access wrapped in
  `Mutex`, per design spec. Don't switch.
- **Secrets use manual `Debug`** that redacts to `<redacted>` so
  tokens never reach logs (`bot_token`, `github_token`, App private
  keys).
- **`agent_kind` values are kebab-case at the serde boundary** (see
  `#[serde(rename_all = "kebab-case")]` on `AgentKind` in
  `src/agents/mod.rs`). Docs, config examples, and deploy templates
  must use kebab-case to stay loadable.
- **State machine tests must be exhaustive** — one test per valid
  transition. Adding a state means adding both a test and a
  transition module.

## Agent and intake extension points

- **New agent kind**: extend `AgentKind` (`src/agents/mod.rs`),
  register it in `poller::build_agent_registry`, and — for ACP —
  supply an `AcpAgentDef` default in `src/agents/acp.rs`.
- **New intake source**: follow the `DiscordClient` trait shape and
  define a new `SourceEntry` variant. A Discord-sourced row starts
  with `source = Discord`, `external_id = <thread_id>`, and
  `github_issue_number = 0` until `/confirm` opens the GitHub issue.
- **Discord runtime** is gated behind the `discord` Cargo feature.
  Default builds compile and skip `[[sources]]` entries with a
  warning. With the feature on, `SerenityDiscordClient` talks to the
  Discord REST API; the runtime uses tick-based polling today (a real-
  time Gateway path slots in behind the same trait without changing
  the config shape).

## Conventions

- Integration tests under `tests/` use `#[path = ...]` to pull in
  internal modules.
- `cargo test` must be green before any commit (a pre-commit hook
  enforces this).
- Run `lineguard <path>` before committing to catch whitespace /
  encoding issues.
- Follow Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`,
  `chore:`). One concern per commit.
- Release builds only; the repo's `Cargo.toml` disables debug info for
  dependencies in dev builds via `[profile.dev.package."*"]`.

## Where else to look

- **Module layout, state graph, trait contracts**: `docs/architecture.md`.
- **Full config reference**: `hammurabi.toml.example` (one commented
  file covering every field, intake source, and agent kind).
- **CLI usage**: `README.md` has a command table; `install.md`
  documents config discovery, secrets, and authentication modes.
- **Deployment**: `deploy/docker/README.md` and
  `deploy/helm/hammurabi/README.md`.
