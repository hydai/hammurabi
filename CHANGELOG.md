# Changelog
## 0.1.2 (2026-04-24)

### Features

- add ACP client module
- add AcpAgent and fake-agent test harness
- per-repo and per-task agent_kind selection
- per-agent instruction file seeding
- stream ACP tool calls as live GitHub comments
- add DiscordClient trait, config, and publishers
- end-to-end Discord-sourced lifecycle
- serenity-backed runtime behind `discord` feature
- add --config and --data-dir flags for container-friendly paths
- fetch hammurabi.toml from https:// URL
- single ${VAR} expansion funnel across all string fields
- *_file sibling fields for Secret-as-file mounts
- graceful SIGTERM/SIGINT shutdown with ACP subprocess fanout
- per-agent Dockerfiles (base/claude/gemini/codex)
- Kubernetes raw manifests and Helm chart
- container image + helm chart publishing workflows

### Fixes

- use Template struct syntax for knope CreatePullRequest fields
- cfg-gate libc::kill liveness probe for non-Unix builds

## 0.1.1 (2026-04-24)

### Features

- add ACP client module
- add AcpAgent and fake-agent test harness
- per-repo and per-task agent_kind selection
- per-agent instruction file seeding
- stream ACP tool calls as live GitHub comments
- add DiscordClient trait, config, and publishers
- end-to-end Discord-sourced lifecycle
- serenity-backed runtime behind `discord` feature
- add --config and --data-dir flags for container-friendly paths
- fetch hammurabi.toml from https:// URL
- single ${VAR} expansion funnel across all string fields
- *_file sibling fields for Secret-as-file mounts
- graceful SIGTERM/SIGINT shutdown with ACP subprocess fanout
- per-agent Dockerfiles (base/claude/gemini/codex)
- Kubernetes raw manifests and Helm chart
- container image + helm chart publishing workflows

### Fixes

- use Template struct syntax for knope CreatePullRequest fields
- cfg-gate libc::kill liveness probe for non-Unix builds
