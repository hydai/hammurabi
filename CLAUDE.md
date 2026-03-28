# Hammurabi

## Project Overview

Hammurabi is a Rust CLI daemon that monitors a GitHub repository's issue board and orchestrates Claude CLI agents to automate the issue lifecycle (spec drafting, decomposition, implementation) with mandatory human approval at every write step.

## Build & Test

```bash
cargo build --release    # Always use release builds
cargo test               # Run all unit + integration tests
```

## Architecture

- **Pure state machine** (`src/state_machine.rs`) -- all transitions are `(State, Event) -> Vec<SideEffect>` with no I/O
- **Trait-based abstractions** -- `GitHubClient`, `AiAgent`, `WorktreeManager` traits enable mock-based testing
- **Database** (`src/db.rs`) -- SQLite with WAL mode, wrapped in `Mutex` for thread safety
- **Transitions** (`src/transitions/`) -- one module per active state, each performing the actual work
- **Poller** (`src/poller.rs`) -- main daemon loop that orchestrates everything

## Key Conventions

- All external dependencies are behind traits (GitHub, AI, git worktrees)
- Test with mocks in `#[cfg(test)] mod mock` blocks within each module
- State machine tests must be exhaustive -- one test per valid transition
- Integration tests live in `tests/` and use `#[path = ...]` to import modules
- `rusqlite` is used (not sqlx) per design spec -- synchronous access wrapped in `Mutex`

## File Layout

```
src/
├── main.rs              # CLI entry point
├── config.rs            # TOML config + env overrides
├── db.rs                # SQLite schema + CRUD
├── models.rs            # IssueState, SubIssueState, data structs
├── state_machine.rs     # Pure transition function
├── github.rs            # GitHubClient trait + OctocrabClient
├── claude.rs            # AiAgent trait + ClaudeCliAgent
├── worktree.rs          # WorktreeManager trait + GitWorktreeManager
├── approval.rs          # Approval gate checking
├── prompts.rs           # AI prompt templates
├── poller.rs            # Daemon main loop
├── lock.rs              # PID-based lock file
├── error.rs             # Error types
└── transitions/
    ├── spec_drafting.rs
    ├── decomposing.rs
    ├── agents_working.rs
    └── completion.rs
```
