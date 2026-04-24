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

- An AI agent that Hammurabi can drive. Defaults to the [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code); see [Agent selection (ACP)](#agent-selection-acp) to route through Gemini or Codex instead
- GitHub personal access token with `repo` scope
- `git` on `PATH`
- Rust toolchain (to build from source)

## Install

Three ways to run Hammurabi:

**Native binary (from source):**

```bash
cargo install --path .
```

**Docker** (pre-built images with bundled agent CLIs — see [Running in Docker](#running-in-docker)):

```bash
docker run --rm -v $(pwd)/hammurabi.toml:/etc/hammurabi/hammurabi.toml:ro \
  -v hammurabi-data:/var/lib/hammurabi \
  -e GITHUB_TOKEN=$GITHUB_TOKEN \
  ghcr.io/hydai/hammurabi-claude:latest
```

**Kubernetes** (Helm chart or raw manifests — see [Running on Kubernetes](#running-on-kubernetes)):

```bash
helm install hammurabi oci://ghcr.io/hydai/charts/hammurabi \
  --namespace hammurabi --create-namespace \
  --values my-values.yaml
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

Config path resolution (highest precedence first):

1. `--config <path-or-url>` CLI flag. Accepts an `https://` URL as well as a local file path.
2. `HAMMURABI_CONFIG_PATH` env var (same shape as the flag).
3. `./hammurabi.toml` in the current working directory.
4. `$HOME/.config/hammurabi/hammurabi.toml`.

Config is re-read each poll cycle for dynamic reload. Environment variables with `HAMMURABI_` prefix override individual scalar settings (e.g. `HAMMURABI_POLL_INTERVAL=30`).

Mutable state — SQLite database, git worktrees, the daemon lock file — lives under `--data-dir <path>` (or `HAMMURABI_DATA_DIR`). Default is `./.hammurabi`.

Every string-valued setting supports `${VAR}` interpolation anywhere in the value. `$$` escapes a literal `$`. Unknown variables resolve to empty strings. Secret-bearing fields (`github_token`, `github_app.private_key_path`, Discord `bot_token`) also have `*_file` siblings that read from a file on disk — intended for K8s Secret volume mounts, but works anywhere.

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

### Agent selection (ACP)

By default Hammurabi drives the Claude CLI. You can instead (or additionally) route tasks through the **Agent Client Protocol** (ACP) and use Gemini or Codex. Agent selection follows a three-level precedence: per-task override → per-repo default → global default → `claude-cli`.

| `agent_kind` | Driver | Install |
|--------------|--------|---------|
| `claude-cli` (default) | `claude --print --output-format stream-json ...` | Claude Code CLI (see Prerequisites) |
| `acp-claude` | ACP via `claude-agent-acp` | `npm i -g @agentclientprotocol/claude-agent-acp @anthropic-ai/claude-code` and `export CLAUDE_CODE_EXECUTABLE=$(which claude)` |
| `acp-gemini` | ACP via `gemini --acp` | `npm i -g @google/gemini-cli` |
| `acp-codex` | ACP via `codex-acp` | `npm i -g @zed-industries/codex-acp @openai/codex` |

Example: use Gemini for spec drafting, Claude for implementation, Codex for review.

```toml
repo = "owner/repo"
ai_model = "claude-sonnet-4-6"     # ignored for ACP kinds unless they honor `model` configOption
approvers = ["alice"]
github_token = "ghp_..."

agent_kind = "acp-claude"          # global default

[spec]
agent_kind = "acp-gemini"

[review]
agent_kind = "acp-codex"
```

Override a subprocess invocation (command / args / env) per ACP kind:

```toml
[agents.acp_gemini]
command = "gemini"
args = ["--acp"]
env = { GEMINI_API_KEY = "${GEMINI_API_KEY}" }
```

When an ACP agent runs, Hammurabi mirrors its tool-call events as a live progress comment on the underlying issue, collapsed under `<details>` once the run finishes. The Claude CLI path is unchanged and produces no streaming events.

**Seed files.** Before each run Hammurabi writes the task-specific instructions as an agent-native file: `CLAUDE.md` for `claude-cli` / `acp-claude`, `GEMINI.md` for `acp-gemini`, `AGENTS.md` for `acp-codex`.

**Trust model.** Hammurabi auto-approves every `session/request_permission` call from the agent, matching the existing `--dangerously-skip-permissions` posture. Only enable ACP agents you would trust under that posture. `max_turns` and `ai_effort` are Claude-CLI specific and are silently ignored by ACP kinds.

**Platform.** ACP kinds require POSIX process-group signalling for clean teardown. The crate compiles on Windows (tree-kill is degraded to a single-process kill) but ACP runs are only fully supported on macOS / Linux.

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

## Running in Docker

Four image variants, one per supported `agent_kind`:

| Image                          | Bundled agent CLI                              | Covers `agent_kind`               |
| ------------------------------ | ---------------------------------------------- | --------------------------------- |
| `ghcr.io/hydai/hammurabi-base`   | none (extend via `FROM`)                       | any, once you install an agent    |
| `ghcr.io/hydai/hammurabi-claude` | `@anthropic-ai/claude-code` + `@agentclientprotocol/claude-agent-acp` | `claude_cli`, `acp_claude`        |
| `ghcr.io/hydai/hammurabi-gemini` | `@google/gemini-cli`                           | `acp_gemini`                      |
| `ghcr.io/hydai/hammurabi-codex`  | `@openai/codex` + `@zed-industries/codex-acp`  | `acp_codex`                       |

Every image runs as UID 1000, uses `tini` as PID 1, expects the config at `/etc/hammurabi/hammurabi.toml`, and keeps state under `/var/lib/hammurabi`.

### Quick start

```bash
docker run -d --name hammurabi \
  -e GITHUB_TOKEN=ghp_xxxx \
  -v $(pwd)/hammurabi.toml:/etc/hammurabi/hammurabi.toml:ro \
  -v hammurabi-data:/var/lib/hammurabi \
  ghcr.io/hydai/hammurabi-claude:latest

docker exec hammurabi hammurabi status
docker stop --time 30 hammurabi   # graceful SIGTERM drain
```

### Build from source

```bash
docker build -f deploy/docker/Dockerfile.claude -t hammurabi-claude:dev .
```

Per-variant build args pin agent CLI versions — see [`deploy/docker/README.md`](deploy/docker/README.md).

### Extending the base image

```dockerfile
FROM ghcr.io/hydai/hammurabi-base:latest
USER root
RUN apt-get update && apt-get install -y --no-install-recommends my-agent-cli \
 && apt-get clean && rm -rf /var/lib/apt/lists/*
USER 1000:1000
```

## Running on Kubernetes

Hammurabi ships two installation paths. Both assume **one replica** (the PID lock file, SQLite DB, and RWO PVC enforce singleton semantics) with `strategy: Recreate` — rolling updates aren't possible.

### Helm chart

```bash
helm install hammurabi oci://ghcr.io/hydai/charts/hammurabi \
  --namespace hammurabi --create-namespace \
  --set agent=acp_claude \
  --set secrets.data.github_token=ghp_xxx
```

Values surface the `agent` toggle (drives image selection + `agent_kind` in the rendered TOML), persistence size, `config.raw` (literal TOML, Helm-templated) vs `config.url` (remote HTTPS), and secret injection (both envFrom and projected files). See [`deploy/helm/hammurabi/README.md`](deploy/helm/hammurabi/README.md) for the full value reference.

### Raw manifests

```bash
# Edit deploy/k8s/configmap.yaml, secret.yaml, deployment.yaml image tag first.
kubectl apply -k deploy/k8s/
```

### Security posture

All variants default to: non-root (UID 1000), `readOnlyRootFilesystem: true`, `capabilities: drop: [ALL]`, `seccompProfile: RuntimeDefault`, no privilege escalation. The daemon only speaks outbound (GitHub, Discord, AI providers, ACP child stdios); no cluster-API access, no ServiceAccount, no Ingress.

### Graceful shutdown

The daemon installs SIGTERM/SIGINT handlers that:

1. Stop the poll loop at the next cycle boundary.
2. Fan SIGTERM out to every live ACP subprocess group (1.5 s follow-up SIGKILL).
3. Release the PID lock file cleanly.

The K8s default `terminationGracePeriodSeconds: 30` is enough for clean drain in typical cases. A second SIGTERM/SIGINT exits with 130 without waiting.

## Container configuration

| Knob                      | Purpose                                                                        |
| ------------------------- | ------------------------------------------------------------------------------ |
| `HAMMURABI_DATA_DIR`      | Mutable state location (SQLite, lock, worktrees). Set to the PVC mount.        |
| `HAMMURABI_CONFIG_PATH`   | Alternative to `--config`; accepts a path or `https://` URL.                   |
| `HAMMURABI_SECRETS_STRICT` | When `1`, rejects `*_file` paths containing `..` traversal. Off by default. |
| `CLAUDE_CODE_EXECUTABLE`  | Claude variant only — points `claude-agent-acp` at the underlying CLI. Pre-set. |
| `RUST_LOG`                | Standard tracing-subscriber filter. Default: `info`.                           |

**Secrets** can be injected two ways, both simultaneously supported:

- **Env var** (simple): reference inside the TOML via `github_token = "${GITHUB_TOKEN}"`.
- **File mount** (preferred for K8s): reference via `github_token_file = "/var/run/secrets/hammurabi/github_token"`. Avoids tokens showing up in `/proc/<pid>/environ` on a compromised container.

**Hooks** (`[hooks]`) run under `bash -c` and inherit the daemon's full environment. The image ships `bash`, `git`, `gh`, `ripgrep`, `curl`, and `ca-certificates`. Hooks see `GITHUB_TOKEN` and any Discord bot tokens — treat accordingly; don't shell out to untrusted scripts.

## License

Apache-2.0. See [LICENSE](LICENSE).
