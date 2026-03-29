# Hammurabi

A CLI daemon that monitors a GitHub repository's issue board, orchestrates an AI agent to draft specifications and implement solutions, with mandatory human approval at every step.

## Purpose

Automate the lifecycle of GitHub issues from idea to merged implementation while ensuring human oversight at every decision point. Issues are treated as ideas — the agent drafts a spec, the human approves, and the agent implements and opens a single PR.

## Users

Repository maintainers who want AI-assisted development with mandatory human approval gates.

## Prerequisites

- GitHub personal access token with `repo` scope (or a GitHub App installation token with equivalent permissions)
- `git` available on `PATH`
- Network access to the GitHub API (`api.github.com`); GitHub Enterprise is not supported

## Impacts

- Eliminates manual orchestration of the issue-to-implementation workflow
- Enforces human approval before any code is written or merged
- Provides per-issue token usage tracking for cost visibility

## Features

1. **Issue monitoring** -- Discover new issues by polling all open issues in the repository; new issues not yet tracked are inserted as Discovered
2. **Spec generation** -- Analyze a discovered issue and produce a spec posted as an issue comment for human review
3. **Implementation** -- Single AI agent implements the approved spec in an isolated worktree, producing one PR
4. **PR feedback loop** -- Reviewers leave comments on the PR; the daemon re-runs implementation with feedback and force-pushes, iterating until the PR is merged
5. **Approval gates** -- Block progress until human approves via `/approve` comment (spec) or PR merge (implementation)
5. **Error handling and retry** -- Transition to failed state on errors; retry via `/retry` comment or CLI
6. **CLI interface** -- Start daemon, view status, retry and reset issues
7. **Usage tracking** -- Record token usage per AI invocation for cost monitoring

## Success Criteria

- Every state transition produces a GitHub comment on the tracked issue for auditability
- No code is pushed to the repository without a preceding human approval event (`/approve` comment or PR merge)
- All AI token usage is recorded with per-issue attribution in the usage log

## Non-Goals

- CI/CD pipeline management
- Code review automation (humans review all PRs)
- Project management beyond issue lifecycle tracking
- Auto-merging PRs without human approval
- Multi-repository monitoring (the daemon watches a single repository)

## User Journeys

### New issue discovered

**Context**: A maintainer creates a GitHub issue describing a feature or bug fix and applies the tracking label.
**Action**: The daemon discovers the issue on its next poll cycle and begins spec generation.
**Outcome**: A spec is posted as a comment on the issue for human review.

### Spec approved

**Context**: The maintainer reviews the spec comment and comments `/approve`.
**Action**: The daemon detects the approval and begins implementation in an isolated worktree.
**Outcome**: A single PR is opened against the default branch for human review.

### Spec feedback

**Context**: The maintainer replies to the spec comment with feedback (any comment that is not `/approve`).
**Action**: The daemon re-runs spec generation with the feedback incorporated and posts a revised spec comment.
**Outcome**: The maintainer sees an updated spec to review.

### PR feedback

**Context**: The maintainer reviews the implementation PR and leaves a comment requesting changes.
**Action**: The daemon detects the comment from an authorized approver, re-runs implementation with the feedback, and force-pushes to update the PR.
**Outcome**: The PR is updated with revised code. The maintainer can review again, leave more feedback, or merge.

### Implementation complete

**Context**: The maintainer reviews and merges the implementation PR.
**Action**: The daemon detects the PR merge and transitions the issue to Done.
**Outcome**: The issue is marked complete. No further action required.

### Agent failure

**Context**: An AI agent fails during any active phase.
**Action**: The issue transitions to Failed; the daemon posts error details as a GitHub comment.
**Outcome**: The maintainer can retry via `/retry` comment or `hammurabi retry <number>`.

## State Machine

Each tracked issue moves through these states:

| State | Type | Description |
|-------|------|-------------|
| Discovered | Active | New issue found, pending spec generation |
| SpecDrafting | Active | AI analyzes the issue and generates a spec |
| AwaitSpecApproval | Blocking | Spec posted as issue comment, waiting for `/approve` or feedback |
| Implementing | Active | AI agent implementing the approved spec in an isolated worktree |
| AwaitPRApproval | Blocking | Implementation PR open, waiting for human merge or feedback |
| Done | Terminal | Issue fully resolved |
| Failed | Terminal (retryable) | Error occurred; retryable via `/retry` |

### Transition Rules

| Transition | Condition |
|------------|-----------|
| Discovered → SpecDrafting | Daemon picks up issue on poll cycle |
| SpecDrafting → AwaitSpecApproval | AI produces spec; daemon posts it as issue comment |
| AwaitSpecApproval → Implementing | Authorized approver comments `/approve` |
| AwaitSpecApproval → SpecDrafting | Authorized approver posts feedback (non-`/approve` comment) |
| Implementing → AwaitPRApproval | AI agent completes; daemon pushes branch and opens PR |
| AwaitPRApproval → Done | Implementation PR merged by human |
| AwaitPRApproval → Implementing | Authorized approver leaves comment on the PR (feedback loop) |
| AwaitPRApproval → Failed | Implementation PR closed without merge |
| Any active → Failed | Unrecoverable error during transition |
| Failed → previous active state | `/retry` comment or CLI retry command |
| Any → Discovered | `/reset` comment or CLI reset command |
| Any → Done | Issue closed externally on GitHub |

## Approval Gates

| Gate | Mechanism | Used For |
|------|-----------|----------|
| Spec approval | `/approve` comment by human | Approving the generated spec |
| Implementation approval | PR merge by human | Merging the implementation PR |
| PR feedback | Comment on the PR by human | Requesting implementation revisions |

The daemon never force-merges a PR or auto-approves a spec.

Only users listed in the `approvers` configuration may trigger approval or feedback. `/approve` comments from users not in the list are ignored. PR merges by unauthorized users are accepted (GitHub's own permission model governs who can merge).

For spec approval, any reply from an authorized approver that is not `/approve` is treated as feedback. The daemon re-runs spec generation with the feedback appended and posts a revised spec. If multiple feedback comments arrive while spec generation is in progress, the daemon uses only the most recent non-`/approve` comment from an authorized approver as feedback when the next spec generation cycle begins. Earlier unprocessed comments are skipped.

For PR feedback, any comment from an authorized approver on the implementation PR triggers a revision cycle. The daemon transitions back to Implementing with the feedback, re-runs the AI agent, and force-pushes the updated branch. The PR updates automatically. The same "most recent comment wins" rule applies: if multiple comments arrive during re-implementation, only the latest from an approver is used. The daemon tracks the last processed PR comment ID to avoid re-processing.

If the implementation PR is closed without merge, the issue transitions to Failed.

## Issue Discovery

The daemon polls all open issues in the repository on each cycle. Only issues carrying the configured `tracking_label` are tracked; issues without the label are ignored. Maintainers apply the label manually to issues they want the daemon to automate. When the daemon first discovers a labeled issue not yet in SQLite, it inserts it as Discovered.

## Branch Naming and Targets

All PRs target the repository's default branch. The spec phase uses a temporary worktree on `hammurabi/<issue_number>-spec` for AI exploration (no PR is created). The implementation phase works on a branch named `hammurabi/<issue_number>-impl` which becomes the PR branch.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| AI agent exits with error or produces no output | Issue transitions to Failed; error details posted as GitHub comment |
| PR closed without merge | Issue transitions to Failed; retryable via `/retry` to regenerate the artifact |
| GitHub API transient error (rate limit, network) | Daemon retries within the current poll cycle with exponential backoff; logs a warning |
| GitHub API persistent error | After `api_retry_count` consecutive failures (default: 3), the affected issue transitions to Failed |
| Daemon restart | Daemon resumes from SQLite state on startup and reconciles each tracked issue against GitHub before the first poll cycle (see Restart Reconciliation below) |
| Issue closed or deleted externally | Tracked issue transitions to Done; daemon posts no further comments |
| Worktree already exists | Daemon removes the stale worktree and recreates it |
| Retry requested | `/retry` comment on the failed issue, or `hammurabi retry <number>`, resets to previous active state. `/retry` comments on non-Failed issues are ignored |
| Stale blocking state | Issues in blocking states beyond configurable timeout (default: 7 days) receive a reminder comment; no auto-cancellation |
| Concurrent daemon instance | Only one daemon instance may run per repository. If a second instance is started for the same repository, it exits with an error |
| Branch already exists on remote | The daemon deletes and recreates the branch; `hammurabi/*` branches are daemon-managed |

### Restart Reconciliation

On startup, the daemon reconciles each tracked issue against GitHub before the first poll cycle:

| State Type | Reconciliation |
|------------|----------------|
| Active (Discovered, SpecDrafting, Implementing) | Re-execute the transition on the next poll cycle; active transitions are idempotent |
| AwaitSpecApproval | Check for new comments since `last_comment_id`; process `/approve` or feedback accordingly |
| AwaitPRApproval | Check if the implementation PR was merged while stopped; if merged, advance to Done. Also check for new PR comments for feedback |
| Failed | Remain in Failed; no automatic retry |
| Done | No action |

The daemon also detects issues closed or deleted externally during downtime and transitions them to Done.

## Configuration

The daemon reads configuration from `hammurabi.toml`. Search order: current working directory first, then `~/.config/hammurabi/hammurabi.toml`. The first file found wins. Environment variables override individual parameters using the prefix `HAMMURABI_` (e.g., `HAMMURABI_POLL_INTERVAL=30`).

| Parameter | Description | Default |
|-----------|-------------|---------|
| repo | GitHub repository (owner/repo format) | Required |
| poll_interval | Seconds between poll cycles | 60 |
| tracking_label | GitHub label that opts issues into daemon tracking; only labeled issues are processed | hammurabi |
| stale_timeout_days | Days before a blocking state gets a reminder | 7 |
| api_retry_count | Consecutive GitHub API failures before transitioning to Failed | 3 |
| ai_model | Default AI model for all tasks | Required |
| ai_max_turns | Default max conversation turns per AI invocation | 50 |
| ai_effort | Default AI effort level | high |
| approvers | GitHub usernames authorized to approve (PR merges and `/approve` comments) | Required |
| github_token | GitHub authentication token | None (falls back to `GITHUB_TOKEN` env var) |

Per-task-type overrides (spec, implement) are supported for ai_model, ai_max_turns, and ai_effort.

## Authentication

The daemon authenticates with GitHub using a personal access token (or GitHub App installation token) with `repo` scope. The token is resolved in this order:

1. `GITHUB_TOKEN` environment variable (takes precedence)
2. `github_token` field in `hammurabi.toml`

If neither is set, the daemon exits with an error on startup.

## CLI Commands

| Command | Description |
|---------|-------------|
| `hammurabi watch <repo>` | Start the daemon, monitoring the specified repository |
| `hammurabi status` | Display all tracked issues with current state and last activity |
| `hammurabi retry <issue_number>` | Reset a failed issue to its previous active state |
| `hammurabi reset <issue_number>` | Reset an issue to Discovered state |

`hammurabi status` displays a table with these columns: Issue #, Title (truncated to 50 characters), State, Age (time since discovery), and Last Activity (time since last state change). Rows are sorted by state priority: Failed first, then Active states, then Blocking states, then Done.

## Data Model

**Issues**: Each tracked GitHub issue persists its GitHub issue number, title, current state, spec comment ID, spec content, implementation PR number, last processed comment ID (for issue comments), last processed PR comment ID (for PR feedback), previous state (for retry), error message, worktree path, and timestamps.

**Usage log**: Each AI invocation records its parent issue, transition name, input and output token counts, model used, and timestamp.

## Agent Isolation

Each AI agent task runs in an isolated git worktree branching from the target repository. After the agent completes, the daemon pushes the branch and opens a PR. Worktrees are cleaned up after PR merge or failure.

## Agent Contracts

The daemon places a task-specific context file in the worktree root before invoking the agent. Each task type defines what the agent receives and what it must produce.

| Task | Agent Receives | Agent Produces |
|------|---------------|----------------|
| Spec drafting | Issue title, issue body, optional prior feedback, access to repository contents | SPEC.md in the worktree (content extracted and posted as issue comment) |
| Implementation | Full spec content, original issue title and body, optional PR review feedback, access to repository contents | Code changes committed to the worktree branch |

Prompt construction and formatting are implementation details. The contract defines what information flows in and what artifact comes out.

## Observability

- Structured logging at five levels (error, warn, info, debug, trace); default level: info
- Per-issue token usage tracked in the usage log
- `hammurabi status` provides a summary table of all tracked issues

## Terminology

| Term | Definition |
|------|------------|
| Discovered issue | A newly found GitHub issue, not yet analyzed (corresponds to Discovered state) |
| Approval gate | A point where progress blocks until a human explicitly approves |
| Worktree isolation | Running each AI agent in its own git worktree to prevent interference |
| Poll cycle | One iteration of the daemon's main loop: fetch, check, transition, sleep |
