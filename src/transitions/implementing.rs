use crate::claude::AiInvocation;
use crate::error::HammurabiError;
use crate::hooks;
use crate::models::{IssueState, TrackedIssue};
use crate::prompts;

use super::TransitionContext;

pub async fn execute(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
    feedback: Option<&str>,
) -> Result<(), HammurabiError> {
    // If PR already exists and no feedback, this is an idempotent re-run — nothing to do
    if issue.impl_pr_number.is_some() && feedback.is_none() {
        tracing::debug!(
            issue = issue.github_issue_number,
            "Implementation PR already exists, skipping"
        );
        return Ok(());
    }

    // Revision if feedback is provided — whether from PR review (impl_pr_number set)
    // or from auto-review failure (impl_pr_number unset but impl branch exists).
    let is_revision = feedback.is_some();
    let has_pr = issue.impl_pr_number.is_some();

    tracing::info!(
        issue = issue.github_issue_number,
        revision = is_revision,
        "Starting implementation"
    );

    let gh_issue = ctx.github.get_issue(issue.github_issue_number).await?;
    let default_branch = ctx.github.get_default_branch().await?;

    // Read spec content from DB
    let spec_content = issue
        .spec_content
        .as_deref()
        .unwrap_or("No spec available");

    // For revisions, create worktree from the existing impl branch;
    // for first run, create from default branch
    let base_branch = if is_revision {
        format!("hammurabi/{}-impl", issue.github_issue_number)
    } else {
        default_branch.clone()
    };

    let worktree_path = ctx
        .worktree
        .create_worktree(issue.github_issue_number, "impl", &base_branch)
        .await?;

    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| HammurabiError::Worktree("invalid worktree path".to_string()))?
        .to_string();

    // Run after_create hook
    let hook_timeout = hooks::hooks_timeout(&ctx.config.hooks);
    hooks::run_hook(
        "after_create",
        ctx.config.hooks.after_create.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await?;

    // Seed CLAUDE.md with implementation context
    let claude_md = prompts::claude_md_for_implementation(
        &gh_issue.title,
        &gh_issue.body,
        spec_content,
        feedback,
    );
    ctx.worktree
        .seed_file(&worktree_path, "CLAUDE.md", &claude_md)
        .await?;

    // Invoke AI
    let prompt = prompts::implementation_prompt(
        &gh_issue.title,
        &gh_issue.body,
        spec_content,
        feedback,
    );
    let model = ctx.config.ai_model_for_task("implement").to_string();
    let max_turns = ctx.config.ai_max_turns_for_task("implement");
    let effort = ctx.config.ai_effort_for_task("implement").to_string();

    // Run before_run hook
    hooks::run_hook(
        "before_run",
        ctx.config.hooks.before_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await?;

    let ai_result = ctx
        .ai
        .invoke(AiInvocation {
            model: model.clone(),
            max_turns,
            effort,
            worktree_path: worktree_str.clone(),
            prompt,
            timeout_secs: ctx.config.ai_timeout_for_task("implement"),
            stall_timeout_secs: ctx.config.ai_stall_timeout_for_task("implement"),
        })
        .await;

    // Run after_run hook (best-effort, regardless of AI result)
    hooks::run_hook_best_effort(
        "after_run",
        ctx.config.hooks.after_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await;

    let result = ai_result?;

    // Log AI output for debugging
    tracing::info!(
        issue = issue.github_issue_number,
        input_tokens = result.input_tokens,
        output_tokens = result.output_tokens,
        content_len = result.content.len(),
        "AI invocation complete"
    );
    tracing::debug!(
        issue = issue.github_issue_number,
        content = %result.content,
        "AI output content"
    );

    // Log usage
    ctx.db.log_usage(
        issue.id,
        None,
        "implementing",
        result.input_tokens,
        result.output_tokens,
        &model,
    )?;

    // Remove seeded CLAUDE.md so it doesn't leak into the PR
    let _ = tokio::fs::remove_file(worktree_path.join("CLAUDE.md")).await;

    // Ensure all changes are committed
    let commit_msg = if is_revision {
        format!(
            "fix: revise implementation for #{} based on review feedback",
            issue.github_issue_number
        )
    } else {
        format!(
            "feat: implement #{} - {}",
            issue.github_issue_number, gh_issue.title
        )
    };
    let has_changes = ctx
        .worktree
        .commit_all_changes(&worktree_path, &commit_msg)
        .await?;

    if !has_changes {
        return Err(HammurabiError::Ai(format!(
            "AI produced no file changes for issue #{}. AI output: {}",
            issue.github_issue_number,
            result.content.chars().take(500).collect::<String>()
        )));
    }

    // Push branch
    let branch_name = format!("hammurabi/{}-impl", issue.github_issue_number);
    ctx.worktree.push_branch(&branch_name).await?;

    if has_pr {
        // PR already exists (human PR feedback revision) — go back to AwaitPRApproval
        ctx.db.update_issue_state(
            issue.id,
            IssueState::AwaitPRApproval,
            Some(IssueState::Implementing),
        )?;
        ctx.db
            .update_issue_worktree(issue.id, Some(&worktree_str))?;

        ctx.github
            .post_issue_comment(
                issue.github_issue_number,
                "Implementation revised based on PR feedback. Please review the updated PR.",
            )
            .await?;

        tracing::info!(
            issue = issue.github_issue_number,
            "Implementation revised and pushed"
        );
    } else {
        // No PR yet — send to Reviewing for auto-review before PR creation
        // (applies to both first implementation and auto-review revision)
        ctx.db.update_issue_state(
            issue.id,
            IssueState::Reviewing,
            Some(IssueState::Implementing),
        )?;
        ctx.db
            .update_issue_worktree(issue.id, Some(&worktree_str))?;

        let comment_msg = if is_revision {
            "Implementation revised based on review feedback. Running auto-review..."
        } else {
            "Implementation complete. Running auto-review..."
        };
        ctx.github
            .post_issue_comment(issue.github_issue_number, comment_msg)
            .await?;

        tracing::info!(
            issue = issue.github_issue_number,
            revision = is_revision,
            "Implementation complete, transitioning to review"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::mock::MockAiAgent;
    use crate::claude::AiResult;
    use crate::config::RepoConfig;
    use crate::db::Database;
    use crate::github::mock::MockGitHubClient;
    use crate::github::GitHubIssue;
    use crate::worktree::mock::MockWorktreeManager;
    use std::sync::Arc;

    fn test_config() -> RepoConfig {
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
        }
    }

    #[tokio::test]
    async fn test_implementing_creates_pr() {
        let tmp = std::env::temp_dir().join("hammurabi-test-impl");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Add feature X".to_string(),
            body: "We need feature X".to_string(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "Implementation complete".to_string(),
            session_id: Some("sess-1".to_string()),
            input_tokens: 500,
            output_tokens: 300,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue("owner/repo", 1, "Add feature X").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_spec_content(issue.id, "# Spec\nImplement feature X")
            .unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue, None).await.unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        // First implementation now goes to Reviewing (not AwaitPRApproval)
        assert_eq!(updated.state, IssueState::Reviewing);
        // PR is NOT created here — that happens in the reviewing transition
        assert!(updated.impl_pr_number.is_none());

        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 0);

        let wts = wt.created_worktrees.lock().unwrap();
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].0, 1);
        assert_eq!(wts[0].1, "impl");

        let usage = db.get_usage_by_issue(issue.id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].transition, "implementing");

        // Comment should mention auto-review
        let comments = gh.created_comments.lock().unwrap();
        assert!(comments.iter().any(|(_, body)| body.contains("auto-review")));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_implementing_skips_if_pr_exists_no_feedback() {
        let tmp = std::env::temp_dir().join("hammurabi-test-impl-skip");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        let ai = Arc::new(MockAiAgent::new());
        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue("owner/repo", 1, "Feature X").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        db.update_issue_impl_pr(issue.id, 42).unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue, None).await.unwrap();

        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 0);

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_implementing_with_feedback_revises() {
        let tmp = std::env::temp_dir().join("hammurabi-test-impl-revise");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Add feature X".to_string(),
            body: "We need feature X".to_string(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "Revised implementation".to_string(),
            session_id: Some("sess-2".to_string()),
            input_tokens: 600,
            output_tokens: 400,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue("owner/repo", 1, "Add feature X").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        db.update_issue_spec_content(issue.id, "# Spec\nDo X").unwrap();
        db.update_issue_impl_pr(issue.id, 42).unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue, Some("Fix the error handling")).await.unwrap();

        // Should NOT create a new PR
        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 0);

        // Should still transition to AwaitPRApproval
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);

        // Worktree should be based on the impl branch
        let wts = wt.created_worktrees.lock().unwrap();
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].2, "hammurabi/1-impl"); // base branch

        // Usage should be logged
        let usage = db.get_usage_by_issue(issue.id).unwrap();
        assert_eq!(usage.len(), 1);

        // Comment posted on issue
        let comments = gh.created_comments.lock().unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].1.contains("revised"));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_implementing_with_feedback_no_pr_uses_impl_branch_and_goes_to_reviewing() {
        // Auto-review revision path: feedback provided but no PR exists yet.
        // Should base worktree off the impl branch and transition to Reviewing.
        let tmp = std::env::temp_dir().join("hammurabi-test-impl-review-revision");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Add feature X".to_string(),
            body: "We need feature X".to_string(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "Revised implementation".to_string(),
            session_id: Some("sess-review-rev".to_string()),
            input_tokens: 600,
            output_tokens: 400,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue("owner/repo", 1, "Add feature X").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        db.update_issue_spec_content(issue.id, "# Spec\nDo X").unwrap();
        // Note: impl_pr_number is NOT set (no PR yet)
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert!(issue.impl_pr_number.is_none());

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue, Some("Missing tests found in review")).await.unwrap();

        // Should NOT create a PR (no PR path — goes to Reviewing)
        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 0);

        // Should transition to Reviewing (not AwaitPRApproval since no PR exists)
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::Reviewing);

        // Worktree should be based on the impl branch (revision), not default branch
        let wts = wt.created_worktrees.lock().unwrap();
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].2, "hammurabi/1-impl");

        // Comment should mention revision + auto-review
        let comments = gh.created_comments.lock().unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].1.contains("revised"));
        assert!(comments[0].1.contains("auto-review"));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
