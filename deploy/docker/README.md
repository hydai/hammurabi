# Hammurabi Docker images

Four images cover Hammurabi's four agent kinds. Build any variant with the
matching Dockerfile from the repository root:

```bash
docker build -f deploy/docker/Dockerfile.claude -t hammurabi-claude:dev .
```

## Variants

| Image                     | Base                     | Agents served                            | Use case                                                                    |
| ------------------------- | ------------------------ | ---------------------------------------- | --------------------------------------------------------------------------- |
| `hammurabi-base`          | `debian:bookworm-slim`   | none (bring your own agent binary)       | Custom agent installs via `FROM hammurabi-base`, or pure-GitHub-only setups |
| `hammurabi-claude`        | `node:22-bookworm-slim`  | `agent_kind = "claude_cli" \| "acp_claude"` | Default for Claude-backed deployments                                       |
| `hammurabi-gemini`        | `node:22-bookworm-slim`  | `agent_kind = "acp_gemini"`              | Gemini via `gemini --acp`                                                   |
| `hammurabi-codex`         | `node:22-bookworm-slim`  | `agent_kind = "acp_codex"`               | Codex via `codex-acp` wrapper                                               |

Notes:

- The **claude variant merges `claude_cli` and `acp_claude`** because both
  dispatch modes resolve the same `/usr/local/bin/claude` binary on PATH.
  Switching between the two is a config-file change, not an image change.
- Every image sets `CLAUDE_CODE_EXECUTABLE=/usr/local/bin/claude` (the claude
  variant only — load-bearing for `claude-agent-acp`).
- Every image runs as UID/GID `1000:1000` and expects a writable volume at
  `/var/lib/hammurabi` (`HAMMURABI_DATA_DIR`) and a read-only config at
  `/etc/hammurabi/hammurabi.toml` (`HAMMURABI_CONFIG_PATH`).
- `tini` is PID 1 so the process group of each ACP subprocess is reaped
  cleanly; `HEALTHCHECK` uses `pgrep -x hammurabi`.

## Quick start

```bash
# Build
docker build -f deploy/docker/Dockerfile.claude -t hammurabi-claude:dev .

# Run with a mounted config + data volume
docker run -d --name hammurabi \
  -e GITHUB_TOKEN=ghp_xxx \
  -e ANTHROPIC_API_KEY=sk-xxx \
  -v $(pwd)/hammurabi.toml:/etc/hammurabi/hammurabi.toml:ro \
  -v hammurabi-data:/var/lib/hammurabi \
  hammurabi-claude:dev

# Check status
docker exec hammurabi hammurabi status

# Graceful shutdown (< 30 s drain)
docker stop --time 30 hammurabi
```

## Build-time args

Each Dockerfile pins its dependency versions via `ARG`s so rebuilds are
reproducible. Override at build time when you want a specific release:

```bash
docker build -f deploy/docker/Dockerfile.claude \
  --build-arg CLAUDE_CODE_VERSION=2.1.114 \
  --build-arg CLAUDE_AGENT_ACP_VERSION=0.29.2 \
  -t hammurabi-claude:v0.1.0 .
```

Available args per variant:

| Variant   | Args                                                   |
| --------- | ------------------------------------------------------ |
| base      | `RUST_VERSION`, `DEBIAN_CODENAME`                      |
| claude    | `NODE_MAJOR`, `CLAUDE_CODE_VERSION`, `CLAUDE_AGENT_ACP_VERSION` + the base args |
| gemini    | `NODE_MAJOR`, `GEMINI_CLI_VERSION` + the base args     |
| codex     | `NODE_MAJOR`, `OPENAI_CODEX_VERSION`, `CODEX_ACP_VERSION` + the base args       |

## Multi-arch

The builder stage uses `rust:1-bookworm` which is published for both
`linux/amd64` and `linux/arm64`, and the runtime bases (`debian:bookworm-slim`,
`node:22-bookworm-slim`) likewise. The CI workflow at
`.github/workflows/images.yml` builds both platforms via buildx.

For local multi-arch builds:

```bash
docker buildx build --platform linux/amd64,linux/arm64 \
  -f deploy/docker/Dockerfile.claude \
  -t ghcr.io/<org>/hammurabi-claude:dev --push .
```

## Extending the base image

If you need a custom agent not covered by the three variants, start from
`hammurabi-base` and add the install step:

```dockerfile
FROM ghcr.io/<org>/hammurabi-base:v0.1.0
USER root
RUN apt-get update && apt-get install -y --no-install-recommends my-agent-cli \
 && apt-get clean && rm -rf /var/lib/apt/lists/*
USER 1000:1000
```

Remember to drop back to `USER 1000:1000` at the end so the runtime image
stays non-root.

## Hooks in containers

`[hooks]` scripts run under `bash -c`. The image ships `bash`, `git`, `gh`,
`ripgrep`, and `curl` — stick to those or add tools via `FROM`. Hooks
inherit the daemon's full environment, **including** `GITHUB_TOKEN` and
Discord bot tokens; treat them accordingly.
