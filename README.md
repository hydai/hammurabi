# Hammurabi

A CLI daemon that monitors a GitHub repository's issue board, orchestrates AI agents to draft specifications and implement solutions, with mandatory human approval at every step.

## How It Works

```
Issue Created → Spec PR → Human Review → Decomposition → Human Approval → Agent PRs → Human Review → Done
```

1. **Discover** -- The daemon polls for issues with the `hammurabi` label
2. **Spec** -- An AI agent analyzes the issue and opens a PR with a `SPEC.md`
3. **Decompose** -- After the spec PR is merged, the AI breaks it into sub-tasks and posts a plan for approval
4. **Implement** -- On `/approve`, isolated AI agents implement each sub-task in parallel, each opening its own PR
5. **Complete** -- Once all sub-PRs are merged by a human, the issue is marked done

Human approval is required at every stage. The daemon never merges a PR or auto-approves a plan.

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

## CLI Commands

| Command | Description |
|---------|-------------|
| `hammurabi watch <owner/repo>` | Start the daemon |
| `hammurabi status` | Show all tracked issues |
| `hammurabi retry <issue_number>` | Retry a failed issue |
| `hammurabi reset <issue_number>` | Reset an issue to Discovered |

## Configuration

Config is loaded from `./hammurabi.toml` or `~/.config/hammurabi/hammurabi.toml`. Environment variables with `HAMMURABI_` prefix override any setting.

| Parameter | Default | Description |
|-----------|---------|-------------|
| `repo` | *required* | GitHub repository (`owner/repo`) |
| `ai_model` | *required* | Default AI model for all tasks |
| `approvers` | *required* | GitHub usernames authorized to approve |
| `poll_interval` | `60` | Seconds between poll cycles |
| `max_concurrent_agents` | `3` | Max parallel AI agents |
| `tracking_label` | `hammurabi` | GitHub label that opts issues in |
| `stale_timeout_days` | `7` | Days before reminder on stale blocking states |
| `api_retry_count` | `3` | Consecutive API failures before marking Failed |
| `ai_max_turns` | `50` | Max conversation turns per AI invocation |
| `github_token` | -- | Falls back to `GITHUB_TOKEN` env var |

Per-task overrides for `ai_model` and `ai_max_turns` are supported under `[spec]`, `[decompose]`, and `[implement]` sections.

## State Machine

Each tracked issue moves through these states:

```
Discovered → SpecDrafting → AwaitSpecApproval
  → Decomposing → AwaitDecompApproval
  → AgentsWorking → AwaitSubPRApprovals
  → Done
```

Any state can transition to `Failed`. Failed issues can be retried via `/retry` comment or `hammurabi retry`.

## Approval Gates

| Gate | Mechanism |
|------|-----------|
| Spec and implementation PRs | PR merge by human |
| Decomposition plan | `/approve` comment by authorized approver |

Non-`/approve` replies from approvers are treated as feedback -- the daemon re-runs decomposition with the feedback incorporated.

## Error Recovery

- `/retry` on a failed issue (or `hammurabi retry <number>`) resets it to the previous active state
- Partial agent failures: `/retry` re-runs only the failed sub-issues
- Transient GitHub API errors are retried automatically with exponential backoff
- The daemon reconciles state on restart

## License

Apache-2.0. See [LICENSE](LICENSE).
