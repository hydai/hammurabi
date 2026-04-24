# Hammurabi Workflow

```
                         ┌─────────────────────────────────────────────┐
                         │           HAMMURABI WORKFLOW                │
                         │  GitHub Issue → AI Agent → Merged PR       │
                         └─────────────────────────────────────────────┘

  GitHub Issue               ┌────────────┐
  with tracking  ──────────▶ │ Discovered │
  label applied              └─────┬──────┘
                                   │ poll cycle
                                   ▼
                        ┌─────────────────────┐
                   ┌──▶ │    SpecDrafting      │ ◀── AI drafts spec from issue
                   │    └──────────┬──────────┘
                   │               │ spec posted as comment
                   │               ▼
                   │    ┌─────────────────────┐
                   │    │ AwaitSpecApproval 🧑 │ ◀── HUMAN reviews spec
                   │    └──────────┬──────────┘
                   │               │
                   │       ┌───────┴───────┐
                   │       │               │
                   │   feedback         /approve
                   │       │               │
                   └───────┘               ▼
                        ┌─────────────────────┐
                   ┌──▶ │   Implementing      │ ◀── AI writes code in worktree
                   │    └──────────┬──────────┘
                   │               │ code complete
                   │               ▼
                   │    ┌─────────────────────┐
                   │    │     Reviewing        │ ◀── AI auto-reviews its own code
                   │    └──────────┬──────────┘
                   │               │
                   │       ┌───────┴───────┐
                   │       │               │
                   │     FAIL            PASS
                   │    (retry)            │
                   │       │               │
                   └───────┘               ▼
                              ┌─────────────────────┐
                   ┌────────▶ │ AwaitPRApproval  🧑  │ ◀── PR created, HUMAN reviews
                   │          └──────────┬──────────┘
                   │                     │
                   │             ┌───────┴───────┐
                   │             │               │
                   │         feedback          merged
                   │             │               │
              Implementing       │               ▼
              (revise code)      │      ┌──────────────┐
                   ▲             │      │    Done  ✓   │
                   └─────────────┘      └──────────────┘

  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─

  ERROR RECOVERY:
  ┌──────────────┐   /retry    ┌─────────────────┐
  │   Failed  ✗  │ ──────────▶ │ (previous state) │
  └──────────────┘  (approver) └─────────────────┘

  MANUAL OVERRIDE:
  Any State  ── /reset (approver) ──▶  Discovered  (restart from scratch)
  Any State  ── issue closed ────────▶  Done

  🧑 = Human approval gate (only configured approvers)
```

## Key Points

1. **Two human checkpoints** — spec approval and PR merge — nothing ships without human sign-off
2. **Self-correcting loops** — both the spec and implementation can cycle through feedback rounds
3. **AI auto-review** — before creating a PR, the AI reviews its own work and retries on failure
4. **Crash-safe** — every state is persisted to SQLite; the daemon picks up where it left off
5. **`/retry` and `/reset`** — approvers can recover from failures or restart any issue via comments
