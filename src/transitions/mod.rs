pub mod completion;
pub mod implementing;
pub mod reviewing;
pub mod spec_drafting;

use std::path::PathBuf;
use std::sync::Arc;

use crate::agents::{AgentRegistry, AiInvocation, AiResult};
use crate::config::RepoConfig;
use crate::db::Database;
use crate::error::HammurabiError;
use crate::github::GitHubClient;
use crate::hooks;
use crate::worktree::WorktreeManager;

#[derive(Clone)]
pub struct TransitionContext {
    pub github: Arc<dyn GitHubClient>,
    pub agents: Arc<AgentRegistry>,
    pub worktree: Arc<dyn WorktreeManager>,
    pub db: Arc<Database>,
    pub config: Arc<RepoConfig>,
}

pub(crate) struct AiLifecycleParams {
    pub issue_number: u64,
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
}

/// Run the standard AI lifecycle: create worktree, run hooks, seed CLAUDE.md,
/// invoke AI, run after_run hook. Returns the AI result and worktree path.
/// The caller handles all post-AI logic (commit, verdict, DB updates, cleanup).
pub(crate) async fn run_ai_lifecycle(
    ctx: &TransitionContext,
    params: AiLifecycleParams,
) -> Result<AiLifecycleResult, HammurabiError> {
    let worktree_path = ctx
        .worktree
        .create_worktree(params.issue_number, &params.task_name, &params.base_branch)
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

    let claude_md_content = if params.prepend_claude_md {
        let existing = tokio::fs::read_to_string(worktree_path.join("CLAUDE.md"))
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
        .seed_file(&worktree_path, "CLAUDE.md", &claude_md_content)
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

    let agent_kind = ctx.config.agent_kind_for_task(&params.ai_task);
    let agent = ctx.agents.get(agent_kind)?;
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
        })
        .await;

    hooks::run_hook_best_effort(
        "after_run",
        ctx.config.hooks.after_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await;

    let result = ai_result?;

    tracing::info!(
        issue = params.issue_number,
        input_tokens = result.input_tokens,
        output_tokens = result.output_tokens,
        content_len = result.content.len(),
        "AI invocation complete"
    );
    tracing::debug!(
        issue = params.issue_number,
        content = %result.content,
        "AI output content"
    );

    Ok(AiLifecycleResult {
        ai_result: result,
        worktree_path,
        worktree_str,
    })
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
