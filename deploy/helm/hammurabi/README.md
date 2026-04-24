# Hammurabi Helm chart

One chart, four agent variants, single-deployment shape.

## Install

From a local checkout:

```bash
helm install hammurabi deploy/helm/hammurabi \
  --namespace hammurabi --create-namespace \
  --set agent=acp_claude \
  --set secrets.data.github_token=ghp_xxxxx
```

From the published OCI registry (after the CI release flow runs):

```bash
helm install hammurabi oci://ghcr.io/hydai/charts/hammurabi \
  --version 0.1.0 \
  --namespace hammurabi --create-namespace \
  --values my-values.yaml
```

## Key values

| Key                       | Default         | Purpose                                                                              |
| ------------------------- | --------------- | ------------------------------------------------------------------------------------ |
| `agent`                   | `acp_claude`    | `claude` / `acp_claude` / `acp_gemini` / `acp_codex` / `none`. Drives image inference + rendered `agent_kind`. |
| `image.repository`        | auto-inferred   | Override to pull from a non-default registry/org.                                    |
| `image.tag`               | `.Chart.AppVersion` | Image tag to deploy.                                                             |
| `persistence.size`        | `20Gi`          | PVC size. Bump for fleet-scale use.                                                  |
| `config.raw`              | see values.yaml | Literal TOML, templated through Helm `tpl`. Use `{{ .Values.agent }}` etc. inside. |
| `config.url`              | empty           | Remote HTTPS URL (mutually exclusive with `config.raw`).                            |
| `secrets.data.*`          | placeholders    | Becomes both envFrom env vars AND projected files under `/var/run/secrets/hammurabi/`. |
| `secrets.existingSecret`  | empty           | Set + flip `secrets.create=false` to reference an externally-managed Secret.         |
| `livenessProbe.enabled`   | `false`         | `pgrep`-based probe. Off by default (risks false-positive kill on slow AI calls).   |

## Upgrade

```bash
helm upgrade hammurabi deploy/helm/hammurabi --values my-values.yaml
```

`strategy: Recreate` means there is ~1-2 s of downtime during the rollout.
Hammurabi's lock file is released cleanly on SIGTERM; the new pod starts
only after the old one exits.

## Uninstall (keeping state)

```bash
helm uninstall hammurabi
```

The PVC and Secret have `helm.sh/resource-policy: keep` by default, so
uninstall leaves your tracking database and tokens behind. To wipe
everything:

```bash
helm uninstall hammurabi
kubectl delete pvc -l app.kubernetes.io/instance=hammurabi
kubectl delete secret -l app.kubernetes.io/instance=hammurabi
```

## Singleton-only

This chart hard-codes `replicas: 1` and `strategy: Recreate`. Hammurabi's
state model (PID lock file, RWO PVC with bare clone + worktrees, SQLite
WAL) is incompatible with horizontal scaling. Do not override these.

## Remote config mode

Set `config.url=https://...` and clear `config.raw` to have the daemon
fetch its real `hammurabi.toml` at startup and on every poll cycle.
The ConfigMap then just carries a bootstrap stub pointing at the URL.
Caveats: 1 MiB body cap, 30 s total timeout, HTTPS only.
