# Getting started with Hammurabi

A guided walk-through that takes you from a labelled GitHub issue to a
merged PR driven entirely by an AI agent, with you holding the
approval pen at every gate.

This assumes you've already installed Hammurabi and have an AI agent
CLI on `PATH`. If not, start at [`install.md`](install.md) first.

## What you'll do

1. Create a low-stakes test issue (a typo fix in a throwaway repo).
2. Watch Hammurabi draft a spec.
3. Approve or revise the spec with `/approve` or feedback.
4. Watch Hammurabi implement the spec and open a PR.
5. Review the self-reviewed PR, leave feedback if you like, merge.
6. Explore what to try next: ACP agents, multi-repo, Discord intake,
   lifecycle hooks.

Plan for ~10–15 minutes of human time; the AI does most of the work.

## Step 1 — pick a test repository

Use a repository you own and don't mind the daemon writing to. A
personal scratchpad repo is ideal. The daemon will:

- create branches under `hammurabi/<issue_number>-*`;
- push those branches to the remote;
- open a PR against the default branch;
- leave comments on the tracked issue and its PR.

Create the repo if you don't have one, clone it locally, and make sure
its default branch has at least one commit.

## Step 2 — write a minimal config

Create `hammurabi.toml` in your working directory:

```toml
repo = "your-username/test-playground"
ai_model = "claude-sonnet-4-6"
approvers = ["your-github-username"]
github_token = "${GITHUB_TOKEN}"
```

Then export your GitHub token:

```bash
export GITHUB_TOKEN="ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

That's the whole config. Everything else uses sensible defaults — the
full reference with commented examples is in
[`hammurabi.toml.example`](hammurabi.toml.example).

## Step 3 — label a test issue

On GitHub, open an issue in your test repo. A **good first scenario**
is a trivial fix that finishes in one spec/implement cycle:

> **Title:** Fix typo in README.md
>
> **Body:** `"Hamurabi"` should be `"Hammurabi"` in the first
> paragraph of README.md. Replace it and open a PR.

Apply the `hammurabi` label (Hammurabi's default tracking label — see
`tracking_label` in the example config if you want a different one).
The label **must be applied by someone in your `approvers` list** —
that's how Hammurabi distinguishes intended work from drive-by labels.

## Step 4 — start the daemon

```bash
hammurabi watch
```

You'll see lines like:

```text
INFO hammurabi: starting daemon
INFO hammurabi::poller: poll cycle repo=your-username/test-playground
INFO hammurabi::transitions::spec_drafting: running AI agent issue=42
```

Hammurabi polls every 60 seconds by default. Each cycle it:

1. Lists all open issues with the tracking label.
2. Inserts new ones into SQLite as `Discovered`.
3. Advances each tracked issue one transition (draft spec, post
   comment, pick up `/approve`, implement, open PR, …).

Leave the terminal running — the daemon is a long-lived process.

## Step 5 — review the spec

Within a minute or two, Hammurabi posts a spec as a comment on the
issue. It typically covers:

- **Goal**: one-sentence restatement of the issue.
- **Approach**: how the change will be made.
- **Files to change**: specific paths.
- **Tests**: how to verify the change.

You have two options:

- **Approve.** Reply with `/approve` (exact match, from an authorised
  approver). Hammurabi moves on to implementation.
- **Revise.** Reply with any other comment — e.g. "Also capitalise it
  in the page title". Hammurabi regenerates the spec with your
  feedback appended. You can iterate multiple times.

Only comments from users in `approvers` are honoured. Comments from
non-approvers are ignored.

## Step 6 — watch the PR land

Once the spec is approved, Hammurabi:

1. Creates a git worktree on `hammurabi/<issue>-impl`.
2. Invokes the AI agent with the spec, the issue body, and access to
   the worktree.
3. Pushes the branch and opens a PR against the default branch.
4. Posts a progress comment on the issue linking to the PR.
5. Enters **Reviewing** — the agent re-reads its own diff and can
   push follow-up commits to the same branch. This loop is bounded
   by `review_max_iterations` (default 2, minimum 1).

Open the PR on GitHub. The agent will have typed a PR description and
commit messages.

At this point you can:

- **Comment on the PR.** Any comment from an approver is treated as
  feedback; the daemon re-runs implementation with the feedback and
  force-pushes to the same branch. The PR updates in place.
- **Merge.** Merging the PR transitions the issue to `Done` on the
  next poll cycle. Hammurabi never force-merges — this step is
  always yours.

## Step 7 — confirm completion

Run:

```bash
hammurabi status
```

You should see your issue with state `Done`. The daemon has deleted
the worktree and stopped tracking the PR branch.

## What to try next

Once you're comfortable with the happy path, these extensions are all
one-line config changes:

### Switch agents

Route spec drafting through Gemini and implementation through Claude:

```toml
[spec]
agent_kind = "acp-gemini"

[implement]
agent_kind = "acp-claude"
```

See the ACP install notes in [`install.md`](install.md) for adapter
binaries.

### Watch multiple repos at once

Replace the top-level `repo = ...` line with a `[[repos]]` array:

```toml
[[repos]]
repo = "your-username/repo-a"

[[repos]]
repo = "your-username/repo-b"
agent_kind = "acp-gemini"
review_max_iterations = 3
```

Each repo can override any global scalar, plus `[hooks]`, `[spec]`,
`[implement]`, `[review]`.

### Accept ideas from Discord

With the `discord` Cargo feature enabled, a `[[sources]]` entry lets
your team pitch ideas in a Discord channel; Hammurabi drafts a spec
in a thread, iterates with `/revise`, and opens the GitHub issue on
`/confirm`. See the Discord section of `hammurabi.toml.example`.

### Run scripts around each agent invocation

```toml
[hooks]
before_run = "cargo fetch"             # warm the dependency cache
after_run  = "../scripts/post-lint.sh"
```

Hooks run under `sh -c` with the worktree as CWD. Non-zero exit from
`after_create` or `before_run` is fatal (issue moves to `Failed`);
`after_run` and `before_remove` failures are logged but non-fatal.

## Troubleshooting

| Symptom                                              | Likely cause                                                                                     |
|------------------------------------------------------|--------------------------------------------------------------------------------------------------|
| `Error: No GitHub token configured`                  | `GITHUB_TOKEN` not exported and `github_token` not in the config.                                |
| Issue never leaves `Discovered`                      | Label was applied by a non-approver. Re-label from an approver account, or add the labeller to `approvers`. |
| `claude: command not found`                          | Claude Code CLI missing. `npm i -g @anthropic-ai/claude-code` or switch to an ACP kind.          |
| Issue stuck in `SpecDrafting`                        | AI invocation is retrying. Check logs at `RUST_LOG=debug`; the daemon gives up after `ai_max_retries`. |
| `hammurabi watch` exits with "another instance is running" | A previous daemon's PID lock is stale. Remove `.hammurabi/hammurabi.pid` if no `hammurabi` process is actually running. |
| Worktree directory left behind after a failure       | Expected — `/retry` reuses the worktree. If you want a clean slate, delete `.hammurabi/repos/<owner>/<name>/worktrees/<N>` and run `hammurabi reset <N>`. |

For deeper debugging, set `RUST_LOG=hammurabi=debug` before
`hammurabi watch`. The state machine and its invariants are documented
in [`docs/architecture.md`](docs/architecture.md).
