# Installing Hammurabi

This guide covers everything you need to get a Hammurabi binary running
on your system. For your first end-to-end walk-through (labelling an
issue, approving a spec, merging a PR), continue with
[`getting-started.md`](getting-started.md) once you've finished here.

## Prerequisites

| Requirement        | Why                                                                                    |
|--------------------|----------------------------------------------------------------------------------------|
| `git` on `PATH`    | Hammurabi clones the target repo as a bare clone and drives `git worktree` per issue.  |
| GitHub credentials | A Personal Access Token **or** a GitHub App installation. See [Authentication](#authentication) below. |
| An AI agent CLI    | Defaults to Claude Code. Alternatives below.                                           |
| Rust toolchain     | Only if you build from source. 1.83+ recommended.                                      |

### AI agent CLIs

Pick **one** (per task, per repo, or globally — see `agent_kind` in
[`hammurabi.toml.example`](hammurabi.toml.example)):

| `agent_kind`   | Install                                                                                                      |
|----------------|--------------------------------------------------------------------------------------------------------------|
| `claude-cli`   | The [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code); `claude` must be on `PATH`.           |
| `acp-claude`   | `npm i -g @anthropic-ai/claude-code @agentclientprotocol/claude-agent-acp`, then `export CLAUDE_CODE_EXECUTABLE=$(which claude)`. |
| `acp-gemini`   | `npm i -g @google/gemini-cli`                                                                                |
| `acp-codex`    | `npm i -g @openai/codex @zed-industries/codex-acp`                                                           |

ACP (Agent Client Protocol) kinds require POSIX process-group
signalling for clean teardown — fully supported on macOS / Linux.
Windows compiles but degrades ACP process cleanup to a single-process
kill.

### Authentication

**Personal Access Token (simplest).** Create a classic PAT with the
`repo` scope. Export it as `GITHUB_TOKEN` or place it in
`hammurabi.toml` under `github_token`.

**GitHub App (recommended for teams).** Acts as a bot with a `[bot]`
badge on PRs and comments. Configure via `[github_app]` in the config:

```toml
[github_app]
app_id = 123456
private_key_path = "/path/to/your-app.private-key.pem"
installation_id = 78901234
```

The installation must have permissions: Read/Write on Issues, Pull
Requests, Contents, and Metadata.

## Install methods

### From source (cargo)

```bash
cargo install --path .
```

Add `--features discord` to enable the Discord intake runtime:

```bash
cargo install --path . --features discord
```

Without the feature, `[[sources]]` Discord entries log a warning and
are skipped — `[[repos]]` polling still works.

### Docker

Four image variants, one per bundled agent CLI. Images are versioned
(e.g. `v0.1.2`) by CI; **no moving `:latest` tag is published.**

```bash
docker run -d --name hammurabi \
  -e GITHUB_TOKEN=ghp_xxxx \
  -v $(pwd)/hammurabi.toml:/etc/hammurabi/hammurabi.toml:ro \
  -v hammurabi-data:/var/lib/hammurabi \
  ghcr.io/hydai/hammurabi-claude:v0.1.2
```

| Image                            | Bundled agent CLI                                                    | Covers `agent_kind`          |
|----------------------------------|----------------------------------------------------------------------|------------------------------|
| `ghcr.io/hydai/hammurabi-base`   | none (extend via `FROM`)                                             | any, once you install one    |
| `ghcr.io/hydai/hammurabi-claude` | `@anthropic-ai/claude-code` + `@agentclientprotocol/claude-agent-acp`| `claude-cli`, `acp-claude`   |
| `ghcr.io/hydai/hammurabi-gemini` | `@google/gemini-cli`                                                 | `acp-gemini`                 |
| `ghcr.io/hydai/hammurabi-codex`  | `@openai/codex` + `@zed-industries/codex-acp`                        | `acp-codex`                  |

Every image runs as UID 1000, uses `tini` as PID 1, expects the config
at `/etc/hammurabi/hammurabi.toml`, and keeps mutable state under
`/var/lib/hammurabi`. Full details (build args, multi-arch, extending
the base image) are in
[`deploy/docker/README.md`](deploy/docker/README.md).

### Kubernetes (Helm)

```bash
helm install hammurabi oci://ghcr.io/hydai/charts/hammurabi \
  --namespace hammurabi --create-namespace \
  --set agent=acp-claude \
  --set secrets.data.github_token=ghp_xxx
```

The chart is singleton-only (`replicas: 1`, `strategy: Recreate`) and
uses a ReadWriteOnce PVC — Hammurabi's PID lock, SQLite WAL, and git
worktrees are incompatible with horizontal scaling. Full values
reference is in
[`deploy/helm/hammurabi/README.md`](deploy/helm/hammurabi/README.md).

## Configuration discovery

Hammurabi searches for its config in this order (first match wins):

1. `--config <path-or-url>` CLI flag — accepts a local path **or** an
   `https://` URL (1 MiB body cap, 30 s total timeout).
2. `HAMMURABI_CONFIG_PATH` env var — same shape as the flag.
3. `./hammurabi.toml` in the current working directory.
4. `$HOME/.config/hammurabi/hammurabi.toml`.

Config is re-read on every poll cycle, so edits take effect on the next
cycle without a restart. Invalid config is logged and the previous
config is retained.

Mutable state — SQLite database, bare clone, worktrees, the daemon PID
lock — lives under `--data-dir <path>` or `HAMMURABI_DATA_DIR`
(default: `./.hammurabi`).

## Secrets

Every string-valued config field supports `${VAR}` interpolation, with
`$$` as an escape for literal `$`. Unknown variables resolve to the
empty string.

Secret-bearing fields also have a `*_file` sibling that reads from a
file:

| Inline field                    | File alternative                         |
|---------------------------------|------------------------------------------|
| `github_token`                  | `github_token_file`                      |
| `github_app.private_key_path`   | `github_app.private_key_file` (alias)    |
| `[[sources]].bot_token`         | `[[sources]].bot_token_file`             |

Precedence: `*_file` wins, then inline after `${VAR}` expansion. The
two forms are mutually exclusive — setting both is a load-time error.

Set `HAMMURABI_SECRETS_STRICT=1` to reject any `*_file` path containing
`..` traversal.

## Verifying the install

```bash
hammurabi --version       # confirm the binary is on PATH
hammurabi status          # lists tracked issues (empty on a fresh install)
```

Start the daemon against a test repo with a disposable config:

```bash
cp hammurabi.toml.example hammurabi.toml
$EDITOR hammurabi.toml    # set repo, approvers, ai_model
export GITHUB_TOKEN="ghp_..."
hammurabi watch
```

Stop it with `Ctrl-C`. The SIGINT handler drains any active ACP
subprocesses (1.5 s SIGTERM → SIGKILL) and releases the PID lock
cleanly.

## Next step

Head to [`getting-started.md`](getting-started.md) for a guided
walk-through of labelling an issue and reaching a merged PR.
