# Hammurabi

A CLI daemon that monitors a GitHub repository's issue board, orchestrates AI agents to draft specifications and implement solutions, with mandatory human approval at every write step.

## Purpose

Automate the lifecycle of GitHub issues from idea to merged implementation while ensuring human oversight at every decision point. The system eliminates manual orchestration of AI-assisted development without sacrificing control.

## Users

Repository maintainers who want AI-assisted development with mandatory human approval gates.

## Impacts

- Eliminates manual orchestration of the issue-to-implementation workflow
- Enforces human approval before any code is written, merged, or decomposed
- Provides per-issue token usage tracking for cost visibility

## Features

1. **Issue monitoring** -- Discover new issues on a tracked repository by polling
2. **Spec generation** -- Analyze a discovered issue and produce a SPEC.md via pull request
3. **Mission decomposition** -- Break an approved spec into sub-issues, post plan for approval
4. **Agent implementation** -- Spawn isolated AI agents to implement each sub-issue, each producing a PR
5. **Approval gates** -- Block progress until human approves via PR merge or `/approve` comment
6. **Error handling and retry** -- Transition to failed state on errors; retry via `/retry` comment or CLI
7. **CLI interface** -- Start daemon, view status, retry and reset issues
8. **Usage tracking** -- Record token usage per AI invocation for cost monitoring

## Non-Goals

- CI/CD pipeline management
- Code review automation (humans review all PRs)
- Project management beyond issue lifecycle tracking
- Auto-merging PRs without human approval

## User Journeys

### New issue discovered

**Context**: A maintainer creates a GitHub issue describing a feature or bug fix.
**Action**: The daemon discovers the issue on its next poll cycle and begins spec generation.
**Outcome**: A PR containing a SPEC.md is opened against the repository for human review.

### Spec approved

**Context**: The maintainer reviews and merges the spec PR.
**Action**: The daemon detects the merge and decomposes the spec into sub-issues, posting the plan as an issue comment.
**Outcome**: The maintainer sees a numbered list of proposed sub-issues with descriptions.

### Decomposition approved

**Context**: The maintainer reviews the proposed sub-issues and comments `/approve`.
**Action**: The daemon creates GitHub sub-issues and spawns isolated AI agents to implement each.
**Outcome**: Each sub-issue gets its own PR opened when its agent completes.

### Decomposition feedback

**Context**: The maintainer replies to the decomposition plan with feedback (not `/approve`).
**Action**: The daemon re-runs decomposition with the feedback incorporated and posts a revised plan.
**Outcome**: The maintainer sees an updated proposal to review.

### All sub-issues implemented

**Context**: All sub-issue PRs have been merged by the maintainer.
**Action**: The daemon opens a final integration PR combining all work.
**Outcome**: The maintainer reviews and merges the final PR to complete the issue.

### Agent failure

**Context**: An AI agent fails during any active phase.
**Action**: The issue transitions to Failed; the daemon posts error details as a GitHub comment.
**Outcome**: The maintainer can retry via `/retry` comment or `hammurabi retry <number>`.

## State Machine

Each tracked issue moves through these states:

| State | Type | Description |
|-------|------|-------------|
| Discovered | Active | New issue found, pending spec generation |
| SpecDrafting | Active | AI analyzes the issue and generates a SPEC.md |
| AwaitSpecApproval | Blocking | Spec PR open, waiting for human merge |
| Decomposing | Active | AI breaks the approved spec into sub-tasks |
| DecompProposed | Active | Decomposition plan posted as issue comment |
| AwaitDecompApproval | Blocking | Waiting for `/approve`; feedback re-triggers Decomposing |
| AgentsWorking | Active | AI agents working concurrently on sub-issues in isolated worktrees |
| AwaitSubPRApprovals | Blocking | All sub-issue PRs open, waiting for human merge of each |
| AwaitFinalApproval | Blocking | Final integration PR open, waiting for human merge |
| Done | Terminal | Issue fully resolved |
| Failed | Terminal (retryable) | Error occurred; retryable via `/retry` |

### Transition Rules

| Transition | Condition |
|------------|-----------|
| Active states advance | Daemon performs work on next poll cycle |
| Blocking states advance | Daemon detects approval signal (PR merged or `/approve` comment) |
| AgentsWorking to AwaitSubPRApprovals | All sub-issues have an open PR |
| AwaitDecompApproval to Decomposing | Feedback (non-`/approve` reply) received |
| Any active to Failed | Unrecoverable error during transition |
| Failed to previous active state | `/retry` comment or CLI retry command |

## Approval Gates

| Gate | Mechanism | Used For |
|------|-----------|----------|
| Code changes | PR merge by human | Spec PRs, sub-issue PRs, final integration PR |
| Planning decisions | `/approve` comment by human | Decomposition approval |

The daemon never force-merges a PR or auto-approves a plan.

For comment-based approvals, any reply that is not `/approve` is treated as feedback. The daemon re-runs the planning step with the feedback appended and posts a revised proposal.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| AI agent exits with error or produces no output | Issue transitions to Failed; error details posted as GitHub comment |
| Retry requested | `/retry` comment or `hammurabi retry <number>` resets to previous active state |
| Stale blocking state | Issues in blocking states beyond configurable timeout (default: 7 days) receive a reminder comment; no auto-cancellation |

## Configuration

| Parameter | Description | Default |
|-----------|-------------|---------|
| repo | GitHub repository (owner/repo format) | Required |
| poll_interval | Seconds between poll cycles | 60 |
| max_concurrent_agents | Maximum parallel AI agent invocations | 3 |
| tracking_label | GitHub label to identify tracked issues | hammurabi |
| stale_timeout_days | Days before a blocking state gets a reminder | 7 |
| ai_model | Default AI model for all tasks | Configurable |
| ai_max_turns | Default max conversation turns per AI invocation | 50 |

Per-task-type overrides (spec, decompose, implement) are supported for ai_model and ai_max_turns.

## CLI Commands

| Command | Description |
|---------|-------------|
| `hammurabi watch <repo>` | Start the daemon, monitoring the specified repository |
| `hammurabi status` | Display all tracked issues with current state and last activity |
| `hammurabi retry <issue_number>` | Reset a failed issue to its previous active state |
| `hammurabi reset <issue_number>` | Reset an issue to Discovered state |

## Data Model

**Issues**: Each tracked GitHub issue persists its GitHub issue number, title, current state, spec PR number, final PR number, active worktree path, last processed comment ID, previous state (for retry), error message, and timestamps.

**Sub-issues**: Each sub-issue tracks its parent issue, GitHub issue number, title, sub-issue state (pending, working, pr_open, done, failed), PR number, worktree path, and AI session ID for resume.

**Usage log**: Each AI invocation records its parent issue, sub-issue (if applicable), transition name, input and output token counts, model used, and timestamp.

## Agent Isolation

Each AI agent task runs in an isolated git worktree branching from the target repository. After the agent completes, the daemon pushes the branch and opens a PR. Worktrees are cleaned up after PR merge or failure.

## Agent Contracts

The daemon places a task-specific context file in the worktree root before invoking the agent. Each task type defines what the agent receives and what it must produce.

| Task | Agent Receives | Agent Produces |
|------|---------------|----------------|
| Spec drafting | Issue title, issue body, access to repository contents | SPEC.md committed to the worktree branch |
| Decomposition | Merged SPEC.md content, original issue title and body, prior feedback (if re-running after feedback) | Ordered list of sub-issues, each with a title and scope description |
| Implementation | Sub-issue title and body, parent SPEC.md content, access to repository contents | Code changes committed to the worktree branch |

For decomposition, the daemon parses the agent's structured output into discrete sub-issues and posts them as a numbered list on the parent GitHub issue. Each entry includes a title and scope description sufficient for independent implementation.

Prompt construction and formatting are implementation details. The contract defines what information flows in and what artifact comes out.

## Observability

- Structured logging with configurable verbosity levels
- Per-issue token usage tracked in the usage log
- `hammurabi status` provides a summary table of all tracked issues

## Terminology

| Term | Definition |
|------|------------|
| Idea | A newly discovered GitHub issue, not yet analyzed |
| Mission | An issue with an approved spec, ready for decomposition and implementation |
| Sub-issue | A discrete task decomposed from a mission, implemented independently |
| Approval gate | A point where progress blocks until a human explicitly approves |
| Worktree isolation | Running each AI agent in its own git worktree to prevent interference |
| Poll cycle | One iteration of the daemon's main loop: fetch, check, transition, sleep |
