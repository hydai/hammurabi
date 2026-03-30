# Hammurabi

A CLI daemon that monitors one or more GitHub repositories' issue boards, orchestrates AI agents to draft specifications and implement solutions, with mandatory human approval at every step.

## How It Works

```
Issue Labeled → Spec Drafted → /approve → Implementation PR → Human Merge → Done
```

1. **Discover** -- The daemon polls for issues with the `hammurabi` label (must be applied by an authorized approver)
2. **Spec** -- An AI agent analyzes the issue and repo, then posts a spec as an issue comment
3. **Approve** -- An approver replies `/approve` to proceed, or provides feedback to revise the spec
4. **Implement** -- The AI agent implements the approved spec in a git worktree and opens a PR
5. **Review** -- Reviewers can leave PR feedback; the agent revises and re-pushes to the same PR
6. **Complete** -- Once the PR is merged by a human, the issue is marked done

Human approval is required at every stage. The daemon never merges a PR or auto-approves.

## Prerequisites

- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) installed and authenticated
- GitHub personal access token with `repo` scope
- `git` on `PATH`
- Rust toolchain (to build from source)

## Install

```bash
cargo install --path .
```

## Quick Start

1. Create a config file:

```bash
cp hammurabi.toml.example hammurabi.toml
```

2. Edit `hammurabi.toml`:

```toml
repo = "your-org/your-repo"
ai_model = "claude-sonnet-4-6"
approvers = ["your-github-username"]
```

3. Set your GitHub token:

```bash
export GITHUB_TOKEN="ghp_..."
```

4. Start the daemon:

```bash
hammurabi watch your-org/your-repo
```

5. Add the `hammurabi` label to any issue to start automation.

### Multi-Repository Setup

To monitor multiple repositories from a single daemon, use the `[[repos]]` array:

```toml
ai_model = "claude-sonnet-4-6"
approvers = ["your-github-username"]

[[repos]]
repo = "your-org/repo-a"

[[repos]]
repo = "your-org/repo-b"
tracking_label = "auto"
approvers = ["another-user"]    # Override per repo
```

Then start with:

```bash
hammurabi watch
```

Each repo can override `tracking_label`, `approvers`, AI settings, `max_concurrent_agents`, and `hooks`. See [Configuration](#configuration) for all options.

## CLI Commands

| Command | Description |
|---------|-------------|
| `hammurabi watch` | Start the daemon (repos from config) |
| `hammurabi watch <owner/repo>` | Start the daemon for a single repo (overrides config) |
| `hammurabi status` | Show all tracked issues across all repos |
| `hammurabi status --repo <owner/repo>` | Show tracked issues for a specific repo |
| `hammurabi retry <issue_number>` | Retry a failed issue |
| `hammurabi retry <issue_number> --repo <owner/repo>` | Retry with repo disambiguation |
| `hammurabi reset <issue_number>` | Reset an issue to Discovered |
| `hammurabi reset <issue_number> --repo <owner/repo>` | Reset with repo disambiguation |

When an issue number exists in multiple repos, `--repo` is required to disambiguate.

## Configuration

Config is loaded from `./hammurabi.toml` or `~/.config/hammurabi/hammurabi.toml`, re-read each poll cycle for dynamic reload. Environment variables with `HAMMURABI_` prefix override any setting.

### Global Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `repo` | -- | Single GitHub repository (`owner/repo`); use this or `[[repos]]` |
| `ai_model` | *required* | Default AI model for all tasks |
| `approvers` | *required* | Default GitHub usernames authorized to approve |
| `poll_interval` | `60` | Seconds between poll cycles |
| `max_concurrent_agents` | `5` | Max parallel AI agents (per repo) |
| `tracking_label` | `hammurabi` | Default GitHub label that opts issues in |
| `stale_timeout_days` | `7` | Days before reminder on stale blocking states |
| `api_retry_count` | `3` | Max retries for GitHub API calls |
| `ai_max_turns` | `50` | Max conversation turns per AI invocation |
| `ai_timeout_secs` | `3600` | Max total seconds per AI invocation |
| `ai_stall_timeout_secs` | `0` (disabled) | Kill AI if no output for this many seconds; 0 = disabled |
| `ai_max_retries` | `2` | Auto-retries before transitioning to Failed |
| `bypass_label` | -- | Label that enables bypass mode (skips spec approval for approver-created issues) |
| `github_token` | -- | Falls back to `GITHUB_TOKEN` env var |

Per-task overrides for `ai_model`, `ai_max_turns`, `ai_effort`, `ai_timeout_secs`, and `ai_stall_timeout_secs` are supported under `[spec]` and `[implement]` sections.

### Multi-Repo Configuration

Use the `[[repos]]` array to monitor multiple repositories:

```toml
ai_model = "claude-sonnet-4-6"
approvers = ["alice"]
github_token = "ghp_..."

[[repos]]
repo = "owner/repo-a"

[[repos]]
repo = "owner/repo-b"
tracking_label = "auto"
approvers = ["bob"]
ai_model = "claude-opus-4-6"
max_concurrent_agents = 2
```

Each `[[repos]]` entry can override: `tracking_label`, `approvers`, `ai_model`, `ai_max_turns`, `ai_effort`, `ai_timeout_secs`, `ai_stall_timeout_secs`, `ai_max_retries`, `max_concurrent_agents`, `hooks`, and `spec`/`implement` task overrides.

Setting both a top-level `repo` field and `[[repos]]` in the same config file is an error.

### Workspace Hooks

Optional shell scripts executed at workspace lifecycle points:

```toml
[hooks]
after_create = "npm install"       # Runs after worktree creation (failure aborts)
before_run = "make prepare"        # Runs before AI invocation (failure aborts)
after_run = "make cleanup"         # Runs after AI invocation (failure logged, ignored)
before_remove = "echo done"        # Runs before worktree removal (failure logged, ignored)
timeout_secs = 60                  # Hook execution timeout (default: 60)
```

Hooks can be configured globally or per-repo within `[[repos]]` entries.

## State Machine

Each tracked issue moves through these states:

```
Discovered → SpecDrafting → AwaitSpecApproval → Implementing → AwaitPRApproval → Done
```

Any active state can transition to `Failed` (after exhausting `ai_max_retries` automatic retries). Failed issues can be retried via `/retry` comment or `hammurabi retry`. Issues with a `blocked` label are skipped during processing.

## Approval Gates

| Gate | Mechanism |
|------|-----------|
| Spec approval | `/approve` comment on issue by authorized approver |
| Implementation approval | PR merge by human |

Non-`/approve` replies from approvers on the issue are treated as feedback -- the daemon revises the spec. PR review comments trigger implementation revisions on the same PR.

### Bypass Mode

For trusted issues, set `bypass_label` in config (e.g., `"hammurabi-bypass"`). When an issue has this label **and** was created by a user in `approvers`, the spec approval gate is skipped — the daemon auto-approves the spec and proceeds directly to implementation. PR feedback and human merge are still required.

## Error Recovery

- AI invocations auto-retry up to `ai_max_retries` times (default: 2) before transitioning to Failed
- AI invocations are killed if they exceed `ai_timeout_secs` or stall for `ai_stall_timeout_secs`
- `/retry` on a failed issue (or `hammurabi retry <number>`) resets it to the previous active state
- `/reset` resets any issue back to Discovered state
- Transient GitHub API errors are retried automatically with exponential backoff
- The daemon reconciles state on restart
- Config is re-read each poll cycle; invalid config is logged and the previous config is kept

## License

Apache-2.0. See [LICENSE](LICENSE).
