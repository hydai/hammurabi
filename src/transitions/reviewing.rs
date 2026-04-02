use crate::claude::AiInvocation;
use crate::error::HammurabiError;
use crate::hooks;
use crate::models::{IssueState, TrackedIssue};
use crate::prompts;

use super::TransitionContext;

pub async fn execute(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    // Idempotency guard: if a PR already exists for this issue, transition to
    // AwaitPRApproval instead of re-running review (prevents duplicate PRs and
    // avoids the issue getting stuck in Reviewing state on every poller cycle).
    if issue.impl_pr_number.is_some() {
        tracing::info!(
            issue = issue.github_issue_number,
            pr = issue.impl_pr_number,
            "PR already exists, transitioning to AwaitPRApproval to avoid duplicate review"
        );
        ctx.db.update_issue_state(
            issue.id,
            IssueState::AwaitPRApproval,
            Some(IssueState::Reviewing),
        )?;
        return Ok(());
    }

    tracing::info!(
        issue = issue.github_issue_number,
        review_count = issue.review_count,
        "Starting auto-review"
    );

    let gh_issue = ctx.github.get_issue(issue.github_issue_number).await?;
    let default_branch = ctx.github.get_default_branch().await?;

    let spec_content = issue
        .spec_content
        .as_deref()
        .unwrap_or("No spec available");

    // Create worktree from the implementation branch
    let impl_branch = format!("hammurabi/{}-impl", issue.github_issue_number);
    let worktree_path = ctx
        .worktree
        .create_worktree(issue.github_issue_number, "review", &impl_branch)
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

    // Seed CLAUDE.md for review context
    let claude_md = prompts::claude_md_for_review(&gh_issue.title, &gh_issue.body, spec_content);
    ctx.worktree
        .seed_file(&worktree_path, "CLAUDE.md", &claude_md)
        .await?;

    // Run before_run hook
    hooks::run_hook(
        "before_run",
        ctx.config.hooks.before_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await?;

    // Invoke AI with review prompt
    let prompt = prompts::review_prompt(
        &gh_issue.title,
        &gh_issue.body,
        spec_content,
        &default_branch,
    );
    let model = ctx.config.ai_model_for_task("review").to_string();
    let max_turns = ctx.config.ai_max_turns_for_task("review");
    let effort = ctx.config.ai_effort_for_task("review").to_string();

    let ai_result = ctx
        .ai
        .invoke(AiInvocation {
            model: model.clone(),
            max_turns,
            effort,
            worktree_path: worktree_str.clone(),
            prompt,
            timeout_secs: ctx.config.ai_timeout_for_task("review"),
            stall_timeout_secs: ctx.config.ai_stall_timeout_for_task("review"),
        })
        .await;

    // Run after_run hook (best-effort)
    hooks::run_hook_best_effort(
        "after_run",
        ctx.config.hooks.after_run.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await;

    let result = ai_result?;

    tracing::info!(
        issue = issue.github_issue_number,
        input_tokens = result.input_tokens,
        output_tokens = result.output_tokens,
        content_len = result.content.len(),
        "Review AI invocation complete"
    );
    tracing::debug!(
        issue = issue.github_issue_number,
        content = %result.content,
        "Review output content"
    );

    // Log usage
    ctx.db.log_usage(
        issue.id,
        None,
        "reviewing",
        result.input_tokens,
        result.output_tokens,
        &model,
    )?;

    // Remove seeded CLAUDE.md
    let _ = tokio::fs::remove_file(worktree_path.join("CLAUDE.md")).await;

    // Clean up worktree
    hooks::run_hook_best_effort(
        "before_remove",
        ctx.config.hooks.before_remove.as_deref(),
        &worktree_path,
        hook_timeout,
    )
    .await;
    let _ = ctx.worktree.remove_worktree(&worktree_path).await;

    // Parse verdict
    let passed = prompts::parse_review_verdict(&result.content);

    if passed {
        tracing::info!(
            issue = issue.github_issue_number,
            "Review PASSED — creating PR"
        );

        // Push branch and create PR
        let branch_name = format!("hammurabi/{}-impl", issue.github_issue_number);
        let pr_title = gh_issue.title.clone();
        let pr_body = format!(
            "Fixes #{}\n\nImplementation for #{}\n\n---\n*Auto-reviewed and approved by Hammurabi*",
            issue.github_issue_number, issue.github_issue_number
        );
        let pr_number = ctx
            .github
            .create_pull_request(&pr_title, &branch_name, &default_branch, &pr_body)
            .await?;

        // Update DB
        ctx.db.update_issue_state(
            issue.id,
            IssueState::AwaitPRApproval,
            Some(IssueState::Reviewing),
        )?;
        ctx.db.update_issue_impl_pr(issue.id, pr_number)?;
        ctx.db.reset_review_count(issue.id)?;

        ctx.github
            .post_issue_comment(
                issue.github_issue_number,
                &format!(
                    "Auto-review passed. Implementation PR opened: #{}. Please review and merge to complete.",
                    pr_number
                ),
            )
            .await?;
    } else {
        let review_count = ctx.db.increment_review_count(issue.id)?;
        let max_iterations = ctx.config.review_max_iterations;

        if review_count >= max_iterations {
            tracing::info!(
                issue = issue.github_issue_number,
                review_count = review_count,
                max = max_iterations,
                "Review FAILED — max iterations reached, creating PR anyway"
            );

            // Push branch and create PR with review findings
            let branch_name = format!("hammurabi/{}-impl", issue.github_issue_number);
            let findings = prompts::extract_blocking_findings(&result.content);
            let pr_title = gh_issue.title.clone();
            let pr_body = format!(
                "Fixes #{}\n\nImplementation for #{}\n\n## Auto-Review Findings\n\nAuto-review found issues after {} attempts. Please review carefully:\n\n{}\n\n---\n*Generated by Hammurabi*",
                issue.github_issue_number,
                issue.github_issue_number,
                review_count,
                findings.chars().take(2000).collect::<String>()
            );
            let pr_number = ctx
                .github
                .create_pull_request(&pr_title, &branch_name, &default_branch, &pr_body)
                .await?;

            ctx.db.update_issue_state(
                issue.id,
                IssueState::AwaitPRApproval,
                Some(IssueState::Reviewing),
            )?;
            ctx.db.update_issue_impl_pr(issue.id, pr_number)?;

            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    &format!(
                        "Auto-review found issues after {} attempts. Proceeding to human review. PR: #{}",
                        review_count, pr_number
                    ),
                )
                .await?;
        } else {
            tracing::info!(
                issue = issue.github_issue_number,
                review_count = review_count,
                max = max_iterations,
                "Review FAILED — sending back for revision"
            );

            // Extract findings and persist them before state change (crash-safe)
            let findings = prompts::extract_blocking_findings(&result.content);
            ctx.db
                .update_issue_review_feedback(issue.id, Some(&findings))?;

            ctx.db.update_issue_state(
                issue.id,
                IssueState::Implementing,
                Some(IssueState::Reviewing),
            )?;

            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    &format!(
                        "Auto-review found issues (attempt {}/{}). Revising implementation...\n\n{}",
                        review_count,
                        max_iterations,
                        findings.chars().take(1000).collect::<String>()
                    ),
                )
                .await?;
        }
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

    fn setup_issue(gh: &MockGitHubClient, db: &Database) -> TrackedIssue {
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Add feature X".to_string(),
            body: "We need feature X".to_string(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });
        db.insert_issue("owner/repo", 1, "Add feature X").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        db.update_issue_spec_content(issue.id, "# Spec\nImplement feature X")
            .unwrap();
        db.update_issue_state(issue.id, IssueState::Reviewing, Some(IssueState::Implementing))
            .unwrap();
        db.get_issue("owner/repo", 1).unwrap().unwrap()
    }

    #[tokio::test]
    async fn test_review_pass_creates_pr() {
        let tmp = std::env::temp_dir().join("hammurabi-test-review-pass");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());
        let issue = setup_issue(&gh, &db);

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "## Review Summary\nPASS -- All criteria met\n\n## Verdict\nPASS: Ready for human review".to_string(),
            session_id: Some("sess-review".to_string()),
            input_tokens: 200,
            output_tokens: 100,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);
        assert!(updated.impl_pr_number.is_some());
        assert_eq!(updated.review_count, 0); // reset on pass

        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 1);

        let comments = gh.created_comments.lock().unwrap();
        assert!(comments.iter().any(|(_, body)| body.contains("Auto-review passed")));

        let usage = db.get_usage_by_issue(issue.id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].transition, "reviewing");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_review_fail_sends_back_to_implementing() {
        let tmp = std::env::temp_dir().join("hammurabi-test-review-fail");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());
        let issue = setup_issue(&gh, &db);

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "## Review Summary\nFAIL -- 1 blocking issue\n\n### BLOCKING: Missing tests\n**File**: src/foo.rs\n**Issue**: No tests\n\n## Verdict\nFAIL: 1 blocking issues must be addressed".to_string(),
            session_id: Some("sess-review-fail".to_string()),
            input_tokens: 200,
            output_tokens: 100,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // After review fail, state should be Implementing (no inline re-execution)
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::Implementing);
        assert_eq!(updated.review_count, 1);
        // Review feedback should be persisted for the poller to pick up
        assert!(updated.review_feedback.is_some());
        assert!(updated.review_feedback.as_ref().unwrap().contains("Missing tests"));

        let comments = gh.created_comments.lock().unwrap();
        assert!(comments.iter().any(|(_, body)| body.contains("Auto-review found issues")));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_review_fail_max_iterations_creates_pr() {
        let tmp = std::env::temp_dir().join("hammurabi-test-review-max");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());
        let issue = setup_issue(&gh, &db);

        // Configure max iterations to 1 so the first FAIL immediately hits the cap
        let mut config = test_config();
        config.review_max_iterations = 1; // first FAIL brings review_count from 0 to 1, hitting the cap

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "## Review Summary\nFAIL -- Issues found\n\n### BLOCKING: Missing error handling\n**File**: src/foo.rs\n\n## Verdict\nFAIL: 1 blocking issues must be addressed".to_string(),
            session_id: Some("sess-review-max".to_string()),
            input_tokens: 200,
            output_tokens: 100,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(config),
        };

        execute(&ctx, &issue).await.unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);
        assert!(updated.impl_pr_number.is_some());

        // PR body should contain review findings
        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 1);

        let comments = gh.created_comments.lock().unwrap();
        assert!(comments.iter().any(|(_, body)| body.contains("Proceeding to human review")));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_review_unparseable_verdict_defaults_pass() {
        let tmp = std::env::temp_dir().join("hammurabi-test-review-unparseable");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());
        let issue = setup_issue(&gh, &db);

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "The code looks fine to me. No issues found.".to_string(),
            session_id: Some("sess-unparseable".to_string()),
            input_tokens: 100,
            output_tokens: 50,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // Unparseable verdict defaults to PASS → PR should be created
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);
        assert!(updated.impl_pr_number.is_some());

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_review_idempotency_guard_transitions_to_await_pr_approval() {
        // If an issue is in Reviewing but already has a PR, the idempotency guard
        // should transition to AwaitPRApproval without invoking AI or creating a PR.
        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());

        // Set up issue in Reviewing state with an existing PR
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Add feature X".to_string(),
            body: "We need feature X".to_string(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });
        db.insert_issue("owner/repo", 1, "Add feature X").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        db.update_issue_state(issue.id, IssueState::Reviewing, Some(IssueState::Implementing))
            .unwrap();
        db.update_issue_impl_pr(issue.id, 42).unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue.state, IssueState::Reviewing);
        assert_eq!(issue.impl_pr_number, Some(42));

        let ai = Arc::new(MockAiAgent::new());
        // AI should NOT be invoked — no response configured intentionally
        let tmp = std::env::temp_dir().join("hammurabi-test-review-idempotent");
        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            ai: ai.clone(),
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // Should transition to AwaitPRApproval
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);

        // No PR should be created (already exists)
        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 0);

        // No worktree should be created (AI was never invoked)
        let wts = wt.created_worktrees.lock().unwrap();
        assert_eq!(wts.len(), 0);

        // No comments posted
        let comments = gh.created_comments.lock().unwrap();
        assert_eq!(comments.len(), 0);
    }
}
