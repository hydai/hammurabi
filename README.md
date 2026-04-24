# Hammurabi

A CLI daemon that monitors one or more GitHub repositories' issue
boards, orchestrates AI agents to draft specifications and implement
solutions, and keeps a mandatory human approval gate at every step.

## How It Works

```
Issue Labeled → Spec Drafted → /approve → Implementation PR → Self-Review → Human Merge → Done
```

1. **Discover** — The daemon polls for issues carrying the `hammurabi`
   label (must be applied by an authorised approver).
2. **Spec** — An AI agent analyses the issue and repo, then posts a
   spec as an issue comment.
3. **Approve** — An approver replies `/approve`, or provides feedback
   to revise the spec.
4. **Implement** — The agent implements the approved spec in a git
   worktree and opens a PR.
5. **Self-review** — The agent re-reads its own diff and pushes
   follow-up fixes (bounded by `review_max_iterations`, default 2).
6. **Review** — Human reviewers can leave PR feedback; the agent
   revises and re-pushes to the same PR.
7. **Complete** — Once the PR is merged by a human, the issue is
   marked done.

Human approval is required at every stage. The daemon never merges a
PR or auto-approves.

## Documentation map

| You want to …                             | Start here                                                                      |
|-------------------------------------------|---------------------------------------------------------------------------------|
| Install Hammurabi for the first time      | [`install.md`](install.md)                                                      |
| Walk through your first issue → merged PR | [`getting-started.md`](getting-started.md)                                      |
| See every config option                   | [`hammurabi.toml.example`](hammurabi.toml.example) (the source of truth)        |
| Understand the internals / contribute     | [`docs/architecture.md`](docs/architecture.md), [`CLAUDE.md`](CLAUDE.md)        |
| Deploy with Docker                        | [`deploy/docker/README.md`](deploy/docker/README.md)                            |
| Deploy on Kubernetes                      | [`deploy/helm/hammurabi/README.md`](deploy/helm/hammurabi/README.md)            |

## Quick start (from source)

```bash
cargo install --path .

cp hammurabi.toml.example hammurabi.toml
$EDITOR hammurabi.toml                # set repo, approvers, ai_model
export GITHUB_TOKEN="ghp_..."

hammurabi watch
```

Label any issue in the configured repo with `hammurabi` (applying the
label yourself, as an approver) to kick off the lifecycle. Full
tutorial in [`getting-started.md`](getting-started.md).

## CLI commands

| Command                                              | Description                                                     |
|------------------------------------------------------|-----------------------------------------------------------------|
| `hammurabi watch`                                    | Start the daemon against every `[[repos]]` entry in the config. |
| `hammurabi watch <owner/repo>`                       | Start the daemon against a single repo (overrides config).      |
| `hammurabi status`                                   | List tracked issues across all repos.                           |
| `hammurabi status --repo <owner/repo>`               | List tracked issues for one repo.                               |
| `hammurabi retry <issue_number> [--repo <o/r>]`      | Reset a `Failed` issue to its previous active state.            |
| `hammurabi reset <issue_number> [--repo <o/r>]`      | Force any issue back to `Discovered`.                           |

Global flags: `--config <path-or-url>` (local file or `https://` URL,
1 MiB / 30 s cap) and `--data-dir <path>` (overrides
`HAMMURABI_DATA_DIR`, default `./.hammurabi`).

Where an issue number exists in multiple repos, `--repo` is required
to disambiguate.

## Configuration at a glance

The canonical, fully-commented reference is
[`hammurabi.toml.example`](hammurabi.toml.example). The headline
sections:

- **Global scalars** — `ai_model`, `approvers`, `poll_interval`,
  `max_concurrent_agents`, `tracking_label`, `bypass_label`, AI
  timeouts, `review_max_iterations`.
- **Authentication** — either `github_token` (PAT) or `[github_app]`
  (App installation). Both support `${VAR}` interpolation and a
  `*_file` sibling for K8s Secret mounts.
- **Agent selection (ACP)** — `agent_kind` at global, per-repo, and
  per-task level; choose between `claude-cli` (default), `acp-claude`,
  `acp-gemini`, `acp-codex`. Override the subprocess invocation per
  kind under `[agents.acp_*]`.
- **Per-task overrides** — `[spec]`, `[implement]`, `[review]` tables
  can override `ai_model`, `ai_max_turns`, `ai_effort`, timeouts, and
  `agent_kind`.
- **Multi-repo** — `[[repos]]` array; each entry inherits globals and
  can locally override any scalar plus `[hooks]`, `[review]`,
  `[spec]`, `[implement]`.
- **Workspace hooks** — `[hooks]` with `after_create`, `before_run`,
  `after_run`, `before_remove`. Run under `sh -c` with the worktree as
  CWD. `after_create` and `before_run` failures are fatal; the other
  two are logged but non-fatal.
- **Chat intake sources** — `[[sources]]` with `kind = "discord"`
  opens a thread-based spec-drafting flow behind the `discord` Cargo
  feature.

Config discovery order: `--config` → `HAMMURABI_CONFIG_PATH` →
`./hammurabi.toml` → `$HOME/.config/hammurabi/hammurabi.toml`. Config
is re-read every poll cycle.

## State machine

```
Discovered → SpecDrafting → AwaitSpecApproval → Implementing → Reviewing → AwaitPRApproval → Done
```

Any active state can move to `Failed` after `ai_max_retries`
consecutive AI errors. `/retry` (or `hammurabi retry`) returns a
`Failed` issue to its previous state; `/reset` forces any issue back to
`Discovered`. Full transition table and invariants are in
[`docs/architecture.md`](docs/architecture.md).

### Approval gates

| Gate                   | Trigger                                                   |
|------------------------|-----------------------------------------------------------|
| Spec approval          | `/approve` comment from an approver on the issue.         |
| Spec revision          | Any other comment from an approver on the issue.          |
| PR revision            | Any comment from an approver on the implementation PR.    |
| Implementation merge   | PR merge by a human (Hammurabi never force-merges).       |

**Bypass mode.** Set `bypass_label = "hammurabi-bypass"`; issues
carrying this label **and** created by a user in `approvers` skip the
spec approval gate. The PR feedback and human-merge gates still apply.

## Running in Docker

Four image variants cover the four agent kinds. Images are versioned
(e.g. `v0.1.2`) by CI — **no moving `:latest` tag is published.**

```bash
docker run -d --name hammurabi \
  -e GITHUB_TOKEN=ghp_xxxx \
  -v $(pwd)/hammurabi.toml:/etc/hammurabi/hammurabi.toml:ro \
  -v hammurabi-data:/var/lib/hammurabi \
  ghcr.io/hydai/hammurabi-claude:v0.1.2
```

Full variant table, build-time args, multi-arch builds, and how to
extend `hammurabi-base` with a custom agent live in
[`deploy/docker/README.md`](deploy/docker/README.md).

## Running on Kubernetes

Hammurabi ships a Helm chart and raw manifests. Both assume **one
replica** — the PID lock file, SQLite WAL, and RWO PVC make the
daemon singleton-only.

```bash
helm install hammurabi oci://ghcr.io/hydai/charts/hammurabi \
  --namespace hammurabi --create-namespace \
  --set agent=acp-claude \
  --set secrets.data.github_token=ghp_xxx
```

Full values reference (persistence, config modes, secret injection,
probes, security posture) is in
[`deploy/helm/hammurabi/README.md`](deploy/helm/hammurabi/README.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
