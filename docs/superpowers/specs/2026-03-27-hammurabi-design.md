# Hammurabi Design Spec

A Rust CLI daemon that monitors a GitHub repository's issues board, orchestrates Claude CLI agents to brainstorm ideas into specs and implement missions, with mandatory human-in-the-loop approval at every write step.

## Core Concept

Every GitHub issue follows a single lifecycle:

1. **Idea phase**: New issue is discovered, Claude analyzes it and drafts a `SPEC.md`, opens a PR for human review.
2. **Mission phase**: Once the spec PR is merged, the issue becomes a mission. Claude decomposes it into sub-issues, waits for human approval, then spawns agents to implement each sub-issue.
3. **Completion**: All sub-issue PRs merged, final integration PR opened for human approval.

Human approval is required before any branch write or PR merge.

## State Machine

Each tracked issue moves through these states:

```
Discovered → SpecDrafting → AwaitSpecApproval
  → Decomposing → DecompProposed → AwaitDecompApproval
  → AgentsWorking → AwaitSubPRApprovals
  → AwaitFinalApproval → Done
```

Any active state can also transition to `Failed`. Failed issues can be retried via `/retry` comment or CLI command.

### State Definitions

| State | Type | Description |
|-------|------|-------------|
| `Discovered` | Active | New issue found, pending analysis and spec generation |
| `SpecDrafting` | Active | Claude reads the issue, analyzes scope, and generates SPEC.md in one shot |
| `AwaitSpecApproval` | Blocking | Spec PR open, waiting for human to review and merge |
| `Decomposing` | Active | Claude reads the merged spec and breaks the mission into sub-tasks |
| `DecompProposed` | Active | Decomposition plan posted as an issue comment for review |
| `AwaitDecompApproval` | Blocking | Waiting for `/approve` comment; feedback re-triggers `Decomposing` |
| `AgentsWorking` | Active | Claude agents working concurrently on sub-issues in isolated worktrees; as each finishes, its PR is opened |
| `AwaitSubPRApprovals` | Blocking | All sub-issue PRs are open, waiting for human to review and merge each |
| `AwaitFinalApproval` | Blocking | Final integration PR open, waiting for human merge |
| `Done` | Terminal | Issue fully resolved |
| `Failed` | Terminal (retryable) | An error occurred; retryable via `/retry` |

### Transition Rules

- **Active states**: The daemon performs work on the next poll cycle.
- **Blocking states**: The daemon only checks for approval signals (PR merged, `/approve` comment).
- **`AgentsWorking` → `AwaitSubPRApprovals`**: Transition fires when all sub-issues have an open PR (each agent opens its own PR upon completion).
- **Failed state**: Transitions back to the previous active state on `/retry`. The `previous_state` is stored in the `issues` table.

## Architecture

```
hammurabi/
├── src/
│   ├── main.rs              # CLI entry point, arg parsing, daemon loop
│   ├── config.rs            # Config file parsing (hammurabi.toml)
│   ├── db.rs                # SQLite schema, migrations, queries
│   ├── poller.rs            # GitHub polling loop (tick-based)
│   ├── github.rs            # GitHub API client (issues, PRs, comments)
│   ├── state_machine.rs     # State definitions, transition logic
│   ├── transitions/         # One module per active state transition
│   │   ├── mod.rs
│   │   ├── spec_drafting.rs # Read issue + invoke Claude to produce SPEC.md
│   │   ├── decomposing.rs   # Invoke Claude to break mission into sub-issues
│   │   ├── agents_working.rs# Spawn Claude agents in worktrees per sub-issue
│   │   └── finalize.rs      # Create final PR combining all work
│   ├── claude.rs            # Claude CLI subprocess management
│   ├── worktree.rs          # Git worktree creation/cleanup
│   └── approval.rs          # Check for PR approvals and /approve comments
├── Cargo.toml
└── hammurabi.toml.example
```

### Core Loop (poller.rs)

```
loop {
    1. Fetch origin on the bare clone
    2. Poll GitHub for new/updated issues
    3. For each new issue → insert into SQLite as "Discovered"
    4. Load all tracked issues from SQLite
    5. For each tracked issue (bounded concurrency via tokio semaphore):
       - Read current state
       - Check if transition conditions are met
       - If yes, execute transition → update state in SQLite
    6. Sleep for poll_interval
}
```

### Key Crates

| Crate | Purpose |
|-------|---------|
| `octocrab` | GitHub API client |
| `rusqlite` | SQLite with WAL mode |
| `tokio` | Async runtime, bounded concurrency |
| `serde` + `toml` | Config parsing |
| `clap` | CLI argument parsing |
| `tracing` | Structured logging |

## Configuration

```toml
repo = "owner/repo"
poll_interval_secs = 60
max_concurrent_agents = 3
claude_cli_path = "claude"
github_token_env = "GITHUB_TOKEN"

[labels]
tracking = "hammurabi"

[claude]
model = "claude-sonnet-4-6"
max_turns = 50

[claude.spec]              # optional per-task overrides
model = "claude-opus-4-6"
max_turns = 100

[claude.decompose]
model = "claude-sonnet-4-6"
max_turns = 30

[claude.implement]
model = "claude-sonnet-4-6"
max_turns = 50
```

The `[claude]` section sets defaults. Optional per-task-type sections (`spec`, `decompose`, `implement`) override specific fields. Unset fields fall back to the defaults.

## Claude CLI Integration

Each active transition that needs AI work spawns Claude CLI as a child process:

```bash
claude --print \
  --output-format stream-json \
  --model <configured_model> \
  --max-turns <configured_max_turns> \
  --add-dir <worktree_path> \
  -p "<prompt>"
```

- Prompts are constructed per-transition with issue context injected.
- Output is parsed from stream-json for structured results (content, session ID, token usage).
- No `--dangerously-skip-permissions` — Claude runs with normal permission mode.
- The worktree's `CLAUDE.md` provides scoped context and constraints.

## Worktree Isolation

For each agent task (spec drafting, sub-issue implementation):

1. **Create**: `git worktree add .hammurabi/worktrees/<issue>-<task> -b hammurabi/<issue>-<task>` from a bare clone of the target repo.
2. **Seed**: Write a task-specific `CLAUDE.md` into the worktree root with issue context, constraints, and instructions.
3. **Run**: Spawn Claude CLI with `--add-dir` pointing to the worktree.
4. **Result**: Claude commits its work to the worktree branch.
5. **Push**: Daemon pushes the branch and opens a PR.
6. **Cleanup**: After PR merge (or failure), `git worktree remove`.

### Local Clone Management

The daemon maintains a bare clone of the target repo at `.hammurabi/repo/` (created on first run). Worktrees branch off this clone. The bare clone is fetched before each poll cycle to stay current.

## Approval Gates

### PR-Based Approvals (for code changes)

Used for: spec PRs, sub-issue implementation PRs, final integration PR.

- Daemon opens a PR with a descriptive body.
- Daemon polls PR review status via GitHub API on each cycle.
- Transition fires when the PR is merged by the human.
- The daemon never force-merges.

### Comment-Based Approvals (for planning decisions)

Used for: sub-issue decomposition approval.

- Daemon posts a comment on the parent issue listing proposed sub-issues with descriptions.
- Owner replies `/approve` to proceed, or provides textual feedback.
- If feedback is given (any reply that isn't `/approve`), the daemon re-runs decomposition with the feedback appended to the prompt and posts a revised plan.
- Daemon polls issue comments for the `/approve` command.

### Approval Polling

- Checked on each poll cycle (same interval as issue discovery).
- Last-checked comment ID and PR review ID stored in SQLite to avoid reprocessing.
- Daemon posts a GitHub comment at each state transition for visibility.

## Data Model (SQLite)

### `issues` table

| Column | Type | Description |
|--------|------|-------------|
| `id` | INTEGER PK | Internal ID |
| `github_issue_number` | INTEGER UNIQUE | GitHub issue number |
| `github_issue_title` | TEXT | Issue title |
| `state` | TEXT | Current state machine state |
| `spec_pr_number` | INTEGER NULL | PR number for the spec |
| `final_pr_number` | INTEGER NULL | PR number for final implementation |
| `worktree_path` | TEXT NULL | Path to active worktree |
| `last_comment_id` | INTEGER NULL | Last processed comment ID |
| `previous_state` | TEXT NULL | State before entering Failed (for retry) |
| `error_message` | TEXT NULL | Last error if Failed |
| `created_at` | TIMESTAMP | When first discovered |
| `updated_at` | TIMESTAMP | Last state change |

### `sub_issues` table

| Column | Type | Description |
|--------|------|-------------|
| `id` | INTEGER PK | Internal ID |
| `parent_issue_id` | INTEGER FK | References `issues.id` |
| `github_issue_number` | INTEGER | Sub-issue number on GitHub |
| `title` | TEXT | Sub-issue title |
| `state` | TEXT | pending / working / pr_open / done / failed |
| `pr_number` | INTEGER NULL | PR for this sub-issue |
| `worktree_path` | TEXT NULL | Worktree for this sub-issue |
| `session_id` | TEXT NULL | Claude session ID for resume |

### `usage_log` table

| Column | Type | Description |
|--------|------|-------------|
| `id` | INTEGER PK | Internal ID |
| `issue_id` | INTEGER FK | References `issues.id` |
| `sub_issue_id` | INTEGER NULL FK | References `sub_issues.id` |
| `transition` | TEXT | Which transition ran |
| `input_tokens` | INTEGER | Tokens in |
| `output_tokens` | INTEGER | Tokens out |
| `model` | TEXT | Model used |
| `timestamp` | TIMESTAMP | When invoked |

## Error Handling

- **Claude failure**: If Claude CLI exits non-zero or produces no usable output, the issue transitions to `Failed`. The daemon posts a GitHub comment with error details.
- **Retry**: Owner comments `/retry` on the issue, or uses `hammurabi retry <issue_number>`. This resets the issue to its previous active state.
- **Stale detection**: Issues in `Await*` states for longer than a configurable timeout (default: 7 days) get a reminder comment posted. No auto-cancellation.

## CLI Interface

```
hammurabi watch owner/repo     # start the daemon
hammurabi status               # show tracked issues and states
hammurabi retry <issue_number> # manually retry a failed issue
hammurabi reset <issue_number> # reset an issue to Discovered
```

## Observability

- Structured logging via `tracing` crate (stdout, filterable by level).
- Per-issue token usage and cost tracked in SQLite via `usage_log` table.
- `hammurabi status` prints a table of all tracked issues, their current state, and last activity.
