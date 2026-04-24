# Hammurabi architecture

Contributor-facing design notes. User-facing documentation lives in
[`README.md`](../README.md), [`install.md`](../install.md), and
[`getting-started.md`](../getting-started.md). Full config reference is
`hammurabi.toml.example`.

## Goals

Hammurabi automates the lifecycle of a GitHub issue — from idea to merged
PR — under mandatory human approval gates. Each transition is an
auditable GitHub comment; no code is pushed without a preceding
`/approve` (spec) or merge (PR).

The daemon is built to:

- drive several AI agents behind one trait (`AiAgent`) and pick between
  them per task, per repo, or globally;
- monitor multiple GitHub repos from a single process;
- accept ideas from chat platforms (Discord today, more later) and turn
  them into GitHub issues via the same pipeline;
- persist state in SQLite so restarts are idempotent.

## Non-goals

- CI/CD pipeline management.
- Code review automation — humans always review PRs.
- Auto-merging PRs, ever (bypass mode skips only the spec gate).
- Per-repo database files — one shared SQLite DB is simpler.
- Per-repo authentication — all repos share one GitHub token or App.
- Dynamic repo addition without a poll cycle — config is re-read each
  cycle, so new `[[repos]]` entries take effect on the next cycle, not
  instantly.

## State machine

```
Discovered → SpecDrafting → AwaitSpecApproval → Implementing → Reviewing → AwaitPRApproval → Done
```

| State                 | Type                    | Description                                                                 |
|-----------------------|-------------------------|-----------------------------------------------------------------------------|
| `Discovered`          | Active                  | New issue found, pending spec generation.                                   |
| `SpecDrafting`        | Active                  | AI analyses the issue and writes a spec.                                    |
| `AwaitSpecApproval`   | Blocking                | Spec posted as issue comment, waiting for `/approve` or feedback.           |
| `Implementing`        | Active                  | AI agent implementing the approved spec in an isolated worktree.            |
| `Reviewing`           | Active (bounded loop)   | Agent re-reads its own diff, pushes follow-up fixes, bounded by `review_max_iterations` (default 2, minimum 1). |
| `AwaitPRApproval`     | Blocking                | Implementation PR open, waiting for human merge or feedback.                |
| `Done`                | Terminal                | Issue fully resolved.                                                       |
| `Failed`              | Terminal (retryable)    | Unrecoverable error; retryable via `/retry`.                                |

`src/state_machine.rs` captures the authoritative `(State, Event) →
Vec<SideEffect>` transition specification. The runtime currently
dispatches directly to the per-state modules under `src/transitions/`
rather than through the state-machine function; the two are kept in
lockstep by tests. Every valid transition has a matching test.

Any active state may transition to `Failed` after exhausting
`ai_max_retries`. `/retry` returns a Failed issue to its previous active
state; `/reset` forces any issue back to `Discovered`.

## Identity model

A tracked issue is identified by the triple `(source, repo, external_id)`:

- `source` is `SourceKind::{GitHub, Discord}` (`src/models.rs`).
- `repo` is `owner/name` of the GitHub repository.
- `external_id` is the GitHub issue number for `source = GitHub`, or the
  Discord thread snowflake for `source = Discord`.

Discord-originated rows start with `github_issue_number = 0` and carry
the pending spec thread; `/confirm` creates the real GitHub issue and
backfills the number. The SQLite table uses `UNIQUE(source, repo,
external_id)` so the same number in different repos and the same source
across repos don't collide.

## Intake sources

| Source    | Discovery                                                      | Identity at creation                         |
|-----------|----------------------------------------------------------------|----------------------------------------------|
| GitHub    | `OctocrabClient::list_tracked_issues` per poll cycle.           | `(GitHub, repo, issue_number)`.              |
| Discord   | `discord_intake_once` after each repo's `poll_cycle`; requires the `discord` Cargo feature. | `(Discord, repo, thread_id)`; issue opens on `/confirm`. |

Both feed the same state machine. `SourceEntry` in `Config` points each
intake at a configured `[[repos]]` entry.

## Trait boundaries

All external dependencies are behind traits so tests can substitute
mocks in `#[cfg(test)] mod mock` blocks.

| Trait                | Purpose                                                                  | Concrete impl                               |
|----------------------|--------------------------------------------------------------------------|---------------------------------------------|
| `GitHubClient`       | Issue / PR / comment / label / branch operations.                        | `OctocrabClient` (octocrab + PAT or App).   |
| `DiscordClient`      | Thread, message, and react operations.                                   | `SerenityDiscordClient` (feature `discord`). |
| `AiAgent`            | Invoke an AI session with streamed events, produce an `AiResult`.        | `ClaudeCliAgent`, `AcpAgent`.               |
| `WorktreeManager`    | Bare clone + per-issue `git worktree` lifecycle.                         | `GitWorktreeManager`.                       |
| `Publisher`          | Post / edit progress on the appropriate surface.                         | `GithubPublisher`, `DiscordPublisher`, `MultiplexPublisher`. |

`TransitionContext::publisher_for(issue)` picks the right `Publisher`
at the call site based on `issue.source`, so the same transition code
posts to GitHub comments for GitHub-originated issues and Discord
thread messages for Discord-originated ones. `MultiplexPublisher` is
the fan-out used when a single run writes to both surfaces.

## Agent registry

`AgentKind` (`src/agents/mod.rs`) has four variants, all kebab-case in
config:

| `agent_kind`     | Implementation                                   | Notes                                         |
|------------------|--------------------------------------------------|-----------------------------------------------|
| `claude-cli`     | `ClaudeCliAgent` (`claude --output-format stream-json`). | Default. Honours `max_turns`, `ai_effort`.     |
| `acp-claude`     | `AcpAgent` driving `claude-agent-acp`.           | ACP over stdio; `max_turns`/`ai_effort` ignored. |
| `acp-gemini`     | `AcpAgent` driving `gemini --acp`.               | Native ACP support.                            |
| `acp-codex`      | `AcpAgent` driving `codex-acp`.                  | ACP wrapper over `codex`.                      |

Selection precedence: per-task → per-repo → global → `claude-cli`. The
registry is built once at startup by `poller::build_agent_registry`,
applying any `[agents.acp_claude|acp_gemini|acp_codex]` command / args /
env overrides from config.

## Module layout

```
src/
├── main.rs              CLI entry point, subcommand dispatch.
├── access.rs            AllowUsers enum (List | All); RawAccess deserialiser.
├── config.rs            TOML model + loader (local path / https URL / env).
├── db.rs                SQLite schema + CRUD; UNIQUE(source, repo, external_id).
├── discord.rs           DiscordClient trait + types; MockDiscordClient.
├── discord_serenity.rs  Serenity-backed runtime (feature: discord).
├── env_expand.rs        Shared ${VAR} expansion for config strings.
├── models.rs            IssueState, SourceKind, TrackedIssue.
├── state_machine.rs     Authoritative (State, Event) → SideEffect spec.
├── github.rs            GitHubClient trait + OctocrabClient (PAT or App).
├── publisher.rs         Publisher trait + Github/Discord/Multiplex impls.
├── agents/              AiAgent trait + ClaudeCli/Acp impls + AgentRegistry.
├── acp/                 Minimal ACP (JSON-RPC over stdio) client.
├── worktree.rs          WorktreeManager trait + GitWorktreeManager.
├── approval.rs          /approve, /retry (GitHub) and /confirm, /revise, /cancel (Discord).
├── hooks.rs             after_create / before_run / after_run / before_remove under sh -c.
├── prompts.rs           AI prompt templates per task.
├── poller.rs            Main daemon loop; per-repo cycles; Discord intake.
├── lock.rs              PID-based singleton lock.
├── error.rs             HammurabiError::{Ai, AiTimeout, Acp, ...}.
└── transitions/         One module per active state edge.

tests/
├── integration_lifecycle.rs         GitHub happy path.
├── integration_error_handling.rs    Failure and retry.
├── integration_discord_lifecycle.rs Discord /confirm pipeline.
├── acp_client_integration.rs        Drives the fake-acp-agent binary.
└── support/fake_acp_agent/main.rs   Scripted ACP agent for tests.
```

## Persistence & reconciliation

SQLite in WAL mode, wrapped in `Mutex` for thread safety. On startup the
daemon reconciles every row against GitHub (and Discord, for pending
threads) before the first poll cycle:

- Active states re-execute on the next cycle — transitions are
  idempotent.
- `AwaitSpecApproval` / `AwaitPRApproval` check for comments / merges
  since `last_comment_id` / `last_pr_comment_id`.
- `Failed` stays `Failed`; no auto-retry.
- Externally-closed issues advance to `Done`.

## Contributor invariants

- All external deps behind traits; concrete impls are thin.
- Transitions are pure over `(State, Event) → Vec<SideEffect>`; I/O is
  performed by the per-state `run` module or by side-effect executors.
- `state_machine.rs` covers every legal transition with an exhaustive
  test; adding a state means adding test cases, not just arms.
- Mocks live in `#[cfg(test)] mod mock` inside the owning module — no
  parallel test-only modules.
- `rusqlite` synchronous API wrapped in `Mutex`, not `sqlx`. This is a
  deliberate simplicity choice — don't switch.
- `UNIQUE(source, repo, external_id)` is the identity invariant; any
  new intake source must fit this shape.
- Secret-bearing fields use a manual `Debug` impl that redacts to
  `<redacted>` so tokens never hit logs.
- `agent_kind` values are kebab-case at the serde boundary (see
  `#[serde(rename_all = "kebab-case")]` on `AgentKind`). Docs and
  examples must use kebab-case to stay loadable.

## References

- Default agent behaviour: [Claude Code CLI docs](https://docs.anthropic.com/en/docs/claude-code).
- ACP protocol: [agentclientprotocol.com](https://agentclientprotocol.com).
- Deployment: [`deploy/docker/README.md`](../deploy/docker/README.md),
  [`deploy/helm/hammurabi/README.md`](../deploy/helm/hammurabi/README.md).
- Agent-facing guidance: [`CLAUDE.md`](../CLAUDE.md).
