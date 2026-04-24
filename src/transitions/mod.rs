pub mod completion;
pub mod implementing;
pub mod reviewing;
pub mod spec_drafting;

mod progress;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::agents::{AgentEvent, AgentKind, AgentRegistry, AiInvocation, AiResult};
use crate::config::RepoConfig;
use crate::db::Database;
use crate::discord::DiscordClient;
use crate::error::HammurabiError;
use crate::github::GitHubClient;
use crate::hooks;
use crate::models::TrackedIssue;
use crate::publisher::{DiscordPublisher, Publisher};
use crate::worktree::WorktreeManager;

/// Convention for the per-agent instruction file seeded into the worktree
/// before the AI runs. Each agent reads instructions from a different
/// filename by convention.
pub(crate) fn seed_filename(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCli | AgentKind::AcpClaude => "CLAUDE.md",
        AgentKind::AcpGemini => "GEMINI.md",
        AgentKind::AcpCodex => "AGENTS.md",
    }
}

#[derive(Clone)]
pub struct TransitionContext {
    pub github: Arc<dyn GitHubClient>,
    /// Discord client for sources that route through a chat channel.
    /// `None` when only GitHub intake is configured; `publisher_for` falls
    /// back to GitHub-only publishing in that case.
    pub discord: Option<Arc<dyn DiscordClient>>,
    /// Default progress publisher — a `GithubPublisher` wrapping `github`.
    /// Used by transitions that only run after a GitHub issue exists
    /// (Implementing/Reviewing/AwaitPRApproval/Completion). For the
    /// pre-`/confirm` Discord flow, callers use `publisher_for` instead.
    pub publisher: Arc<dyn Publisher>,
    pub agents: Arc<AgentRegistry>,
    pub worktree: Arc<dyn WorktreeManager>,
    pub db: Arc<Database>,
    pub config: Arc<RepoConfig>,
}

impl TransitionContext {
    /// Return the `Publisher` appropriate for `issue`'s lifecycle stage.
    ///
    /// - Discord-sourced issues *before* `/confirm` (no `github_issue_number`
    ///   yet) get a `DiscordPublisher` so the draft spec and status
    ///   updates land in the thread.
    /// - All other issues (GitHub-originated, or Discord-originated that
    ///   have already been `/confirm`ed) get the default GitHub publisher,
    ///   so progress lands on the GitHub issue/PR.
    ///
    /// Mirroring post-`/confirm` updates back to the originating Discord
    /// thread is left as a follow-up once `MultiplexPublisher` is wired in.
    pub(crate) fn publisher_for(&self, issue: &TrackedIssue) -> Arc<dyn Publisher> {
        if issue.is_discord_pending() {
            if let Some(discord) = &self.discord {
                return Arc::new(DiscordPublisher::new(discord.clone()));
            }
            tracing::warn!(
                "Discord-sourced issue but no DiscordClient in ctx; \
                 falling back to GitHub publisher"
            );
        }
        self.publisher.clone()
    }

    /// Resolve the publisher thread_id for `issue` — the number passed to
    /// `Publisher::post`/`update`. For GitHub-sourced issues (and Discord
    /// issues past `/confirm`) this is the GitHub issue number; for
    /// pre-`/confirm` Discord threads it's the thread snowflake.
    pub(crate) fn thread_id_for(&self, issue: &TrackedIssue) -> u64 {
        if issue.is_discord_pending() {
            issue.external_id_u64().unwrap_or(0)
        } else {
            issue.github_issue_number
        }
    }
}

pub(crate) struct AiLifecycleParams {
    pub task_name: String,
    pub base_branch: String,
    pub claude_md: String,
    pub prompt: String,
    pub ai_task: String,
    /// If true, prepend the caller's claude_md to the worktree's existing CLAUDE.md
    /// (separated by a divider). This preserves project-level instructions for hooks.
    pub prepend_claude_md: bool,
}

pub(crate) struct AiLifecycleResult {
    pub ai_result: AiResult,
    pub worktree_path: PathBuf,
    pub worktree_str: String,
    /// The filename (relative to the worktree root) that was seeded with
    /// the caller-provided instructions before the agent ran. Callers use
    /// this to clean up after the agent finishes.
    pub seed_filename: &'static str,
}

/// Run the standard AI lifecycle: create worktree, run hooks, seed CLAUDE.md,
/// invoke AI, run after_run hook. Returns the AI result and worktree path.
/// The caller handles all post-AI logic (commit, verdict, DB updates, cleanup).
///
/// `issue` is taken by reference so progress can be routed to the right
/// thread via `ctx.publisher_for(issue)`. The worktree's numeric scope
/// (branch naming, work-dir path) also derives from the issue — GitHub
/// issues use their issue number; Discord threads use their snowflake
/// until `/confirm` assigns a GitHub issue number.
pub(crate) async fn run_ai_lifecycle(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
    params: AiLifecycleParams,
) -> Result<AiLifecycleResult, HammurabiError> {
    let thread_id = ctx.thread_id_for(issue);
    let worktree_path = ctx
        .worktree
        .create_worktree(thread_id, &params.task_name, &params.base_branch)
        .await?;

    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| HammurabiError::Worktree("invalid worktree path".to_string()))?
        .to_string();

    let hook_timeout = hooks::hooks_timeout(&ctx.config.hooks);

    hooks::run_hook(
        "after_create",
        ctx.config.hooks.after_create.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await?;

    let agent_kind = ctx.config.agent_kind_for_task(&params.ai_task);
    let seed_name = seed_filename(agent_kind);

    let seed_content = if params.prepend_claude_md {
        let existing = tokio::fs::read_to_string(worktree_path.join(seed_name))
            .await
            .unwrap_or_default();
        if existing.is_empty() {
            params.claude_md
        } else {
            format!("{}\n\n---\n\n{}", params.claude_md, existing)
        }
    } else {
        params.claude_md
    };
    ctx.worktree
        .seed_file(&worktree_path, seed_name, &seed_content)
        .await?;

    let model = ctx.config.ai_model_for_task(&params.ai_task).to_string();
    let max_turns = ctx.config.ai_max_turns_for_task(&params.ai_task);
    let effort = ctx.config.ai_effort_for_task(&params.ai_task).to_string();

    hooks::run_hook(
        "before_run",
        ctx.config.hooks.before_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await?;

    let agent = ctx.agents.get(agent_kind)?;

    // Spawn a progress aggregator that surfaces ACP events as a live status
    // comment on the issue's thread (GitHub comment or Discord message).
    // ClaudeCliAgent ignores the sender, so the aggregator never posts
    // anything on that path.
    let publisher = ctx.publisher_for(issue);
    let (events_tx, events_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let aggregator = tokio::spawn(progress::run_aggregator(
        publisher,
        thread_id,
        events_rx,
        Duration::from_secs(10),
    ));

    let ai_result = agent
        .invoke(AiInvocation {
            agent_kind,
            model: model.clone(),
            max_turns,
            effort,
            worktree_path: worktree_str.clone(),
            prompt: params.prompt,
            timeout_secs: ctx.config.ai_timeout_for_task(&params.ai_task),
            stall_timeout_secs: ctx.config.ai_stall_timeout_for_task(&params.ai_task),
            events: Some(events_tx),
        })
        .await;

    // Sender dropped when the invocation scope ends; wait for the
    // aggregator to post its final rendering before proceeding.
    let _ = aggregator.await;

    hooks::run_hook_best_effort(
        "after_run",
        ctx.config.hooks.after_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await;

    let result = ai_result?;

    tracing::info!(
        thread_id = thread_id,
        issue = issue.github_issue_number,
        input_tokens = result.input_tokens,
        output_tokens = result.output_tokens,
        content_len = result.content.len(),
        "AI invocation complete"
    );
    tracing::debug!(
        thread_id = thread_id,
        content = %result.content,
        "AI output content"
    );

    Ok(AiLifecycleResult {
        ai_result: result,
        worktree_path,
        worktree_str,
        seed_filename: seed_name,
    })
}

#[cfg(test)]
mod seed_filename_tests {
    use super::seed_filename;
    use crate::agents::AgentKind;

    #[test]
    fn claude_kinds_seed_claude_md() {
        assert_eq!(seed_filename(AgentKind::ClaudeCli), "CLAUDE.md");
        assert_eq!(seed_filename(AgentKind::AcpClaude), "CLAUDE.md");
    }

    #[test]
    fn gemini_seeds_gemini_md() {
        assert_eq!(seed_filename(AgentKind::AcpGemini), "GEMINI.md");
    }

    #[test]
    fn codex_seeds_agents_md() {
        assert_eq!(seed_filename(AgentKind::AcpCodex), "AGENTS.md");
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Arc;

    use crate::agents::mock::MockAiAgent;
    use crate::agents::{AgentRegistry, AiAgent};
    use crate::config::RepoConfig;

    /// Build a registry wrapping a single mock agent. Used by transition tests
    /// that want fine-grained control over mock behavior.
    pub fn test_registry_with<A>(ai: Arc<A>) -> Arc<AgentRegistry>
    where
        A: AiAgent + 'static,
    {
        Arc::new(AgentRegistry::for_test(ai))
    }

    /// Build a registry with an empty mock. Convenient when a test doesn't
    /// expect the AI to be invoked.
    #[allow(dead_code)]
    pub fn test_registry() -> Arc<AgentRegistry> {
        test_registry_with(Arc::new(MockAiAgent::new()))
    }

    pub fn test_config() -> RepoConfig {
        RepoConfig {
            repo: "owner/repo".to_string(),
            owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            tracking_label: "hammurabi".to_string(),
            stale_timeout_days: 7,
            ai_model: "test-model".to_string(),
            ai_max_turns: 50,
            ai_effort: "high".to_string(),
            ai_timeout_secs: 3600,
            ai_stall_timeout_secs: 0,
            ai_max_retries: 2,
            max_concurrent_agents: 5,
            hooks: crate::config::HooksConfig::default(),
            approvers: vec!["alice".to_string()],
            bypass_label: None,
            review: None,
            review_max_iterations: 2,
            spec: None,
            implement: None,
            agent_kind: crate::agents::AgentKind::ClaudeCli,
        }
    }
}
