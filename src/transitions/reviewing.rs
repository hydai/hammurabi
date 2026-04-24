use crate::error::HammurabiError;
use crate::hooks;
use crate::models::{IssueState, TrackedIssue};
use crate::prompts;

use super::{AiLifecycleParams, TransitionContext};

/// Create a PR, handling the case where one already exists for the head branch
/// (e.g., after a crash between PR creation and DB persistence).
/// On any creation failure, attempts to find an existing PR for the branch
/// (open first, then closed/merged for crash recovery) before returning the
/// original error.
async fn create_or_find_pr(
    ctx: &TransitionContext,
    title: &str,
    branch_name: &str,
    default_branch: &str,
    body: &str,
) -> Result<u64, HammurabiError> {
    match ctx
        .github
        .create_pull_request(title, branch_name, default_branch, body)
        .await
    {
        Ok(pr_number) => Ok(pr_number),
        Err(err) => {
            tracing::warn!(
                branch = %branch_name,
                error = %err,
                "Failed to create PR; attempting to find existing PR by head branch"
            );
            match ctx.github.find_pull_request_by_head(branch_name).await {
                Ok(Some(pr_number)) => {
                    tracing::info!(
                        branch = %branch_name,
                        pr = pr_number,
                        "Using existing PR after creation failure"
                    );
                    Ok(pr_number)
                }
                Ok(None) => Err(err),
                Err(lookup_err) => {
                    tracing::warn!(
                        branch = %branch_name,
                        lookup_error = %lookup_err,
                        "PR lookup also failed; returning original creation error"
                    );
                    Err(err)
                }
            }
        }
    }
}

pub async fn execute(ctx: &TransitionContext, issue: &TrackedIssue) -> Result<(), HammurabiError> {
    // Idempotency guard: if a PR already exists for this issue (either persisted
    // in DB or found on GitHub for the impl branch), transition to AwaitPRApproval
    // without re-running the expensive AI review.
    let impl_branch =
        crate::worktree::branch_name(issue.github_issue_number, crate::worktree::TASK_IMPL);
    let existing_pr = if issue.impl_pr_number.is_some() {
        issue.impl_pr_number
    } else {
        // Check GitHub in case a PR was created but the number wasn't persisted (crash recovery)
        match ctx.github.find_pull_request_by_head(&impl_branch).await {
            Ok(pr) => pr,
            Err(e) => {
                tracing::debug!(
                    issue = issue.github_issue_number,
                    error = %e,
                    "Failed to check for existing PR by branch, proceeding with review"
                );
                None
            }
        }
    };
    if let Some(pr_number) = existing_pr {
        tracing::info!(
            issue = issue.github_issue_number,
            pr = pr_number,
            "PR already exists, transitioning to AwaitPRApproval to avoid duplicate review"
        );
        // Persist PR number if it wasn't in DB (crash recovery path)
        if issue.impl_pr_number.is_none() {
            ctx.db.update_issue_impl_pr(issue.id, pr_number)?;
        }
        ctx.db.update_issue_state(
            issue.id,
            IssueState::AwaitPRApproval,
            Some(IssueState::Reviewing),
        )?;
        // Clear stale review state so it doesn't leak into future transitions
        ctx.db.update_issue_review_feedback(issue.id, None)?;
        ctx.db.reset_review_count(issue.id)?;
        return Ok(());
    }

    tracing::info!(
        issue = issue.github_issue_number,
        review_count = issue.review_count,
        "Starting auto-review"
    );

    let gh_issue = ctx.github.get_issue(issue.github_issue_number).await?;
    let default_branch = ctx.github.get_default_branch().await?;

    let spec_content = issue.spec_content.as_deref().unwrap_or("No spec available");

    let claude_md = prompts::claude_md_for_review(&gh_issue.title, &gh_issue.body, spec_content);

    let prompt = prompts::review_prompt(
        &gh_issue.title,
        &gh_issue.body,
        spec_content,
        &default_branch,
    );

    let lifecycle_result = super::run_ai_lifecycle(
        ctx,
        AiLifecycleParams {
            issue_number: issue.github_issue_number,
            task_name: "review".to_string(),
            base_branch: impl_branch.clone(),
            claude_md,
            prompt,
            ai_task: "review".to_string(),
            prepend_claude_md: true,
        },
    )
    .await;

    // Always clean up review worktree regardless of success or failure
    if let Ok(ref lifecycle) = lifecycle_result {
        let hook_timeout = hooks::hooks_timeout(&ctx.config.hooks);
        let _ = tokio::fs::remove_file(lifecycle.worktree_path.join("CLAUDE.md")).await;
        hooks::run_hook_best_effort(
            "before_remove",
            ctx.config.hooks.before_remove.as_deref(),
            &lifecycle.worktree_path,
            hook_timeout,
        )
        .await;
        let _ = ctx.worktree.remove_worktree(&lifecycle.worktree_path).await;
    }

    let lifecycle = lifecycle_result?;
    let result = &lifecycle.ai_result;

    let model = ctx.config.ai_model_for_task("review").to_string();
    ctx.db.log_usage(
        issue.id,
        None,
        "reviewing",
        result.input_tokens,
        result.output_tokens,
        &model,
    )?;

    // Parse verdict
    let verdict = prompts::parse_review_verdict(&result.content);

    match verdict {
        prompts::ReviewVerdict::Pass | prompts::ReviewVerdict::Unknown => {
            let is_unknown = verdict == prompts::ReviewVerdict::Unknown;
            if is_unknown {
                tracing::info!(
                    issue = issue.github_issue_number,
                    "Review verdict UNKNOWN — creating PR with advisory note"
                );
            } else {
                tracing::info!(
                    issue = issue.github_issue_number,
                    "Review PASSED — creating PR"
                );
            }

            // Create (or find) PR for the implementation branch (already pushed by implementing transition)
            let branch_name =
                crate::worktree::branch_name(issue.github_issue_number, crate::worktree::TASK_IMPL);
            let pr_title = gh_issue.title.clone();
            let pr_body = if is_unknown {
                format!(
                    "Fixes #{}\n\nImplementation for #{}\n\n---\n*Auto-review verdict could not be parsed. Please review carefully.*\n*Generated by Hammurabi*",
                    issue.github_issue_number, issue.github_issue_number
                )
            } else {
                format!(
                    "Fixes #{}\n\nImplementation for #{}\n\n---\n*Auto-reviewed and approved by Hammurabi*",
                    issue.github_issue_number, issue.github_issue_number
                )
            };
            let pr_number =
                create_or_find_pr(ctx, &pr_title, &branch_name, &default_branch, &pr_body).await?;

            // Persist PR number before state transition so a crash between the two
            // doesn't leave the issue in AwaitPRApproval without a PR number.
            ctx.db.update_issue_impl_pr(issue.id, pr_number)?;
            ctx.db.reset_review_count(issue.id)?;
            ctx.db.update_issue_review_feedback(issue.id, None)?;
            ctx.db.update_issue_state(
                issue.id,
                IssueState::AwaitPRApproval,
                Some(IssueState::Reviewing),
            )?;

            // Best-effort comment: DB state is already committed, don't fail the
            // transition if commenting fails.
            let comment_msg = if is_unknown {
                format!(
                    "Auto-review verdict could not be parsed. PR opened for human review: #{}. Please review carefully.",
                    pr_number
                )
            } else {
                format!(
                    "Auto-review passed. Implementation PR opened: #{}. Please review and merge to complete.",
                    pr_number
                )
            };
            if let Err(e) = ctx
                .publisher
                .post(issue.github_issue_number, &comment_msg)
                .await
            {
                tracing::warn!(
                    issue = issue.github_issue_number,
                    error = %e,
                    "Failed to post review comment"
                );
            }
        }
        prompts::ReviewVerdict::Fail => {
            let review_count = ctx.db.increment_review_count(issue.id)?;
            let max_iterations = ctx.config.review_max_iterations;

            if review_count >= max_iterations {
                tracing::info!(
                    issue = issue.github_issue_number,
                    review_count = review_count,
                    max = max_iterations,
                    "Review FAILED — max iterations reached, creating PR anyway"
                );

                // Create (or find) PR with review findings (branch already pushed by implementing transition)
                let branch_name = crate::worktree::branch_name(
                    issue.github_issue_number,
                    crate::worktree::TASK_IMPL,
                );
                let findings = prompts::extract_blocking_findings(&result.content);
                let pr_title = gh_issue.title.clone();
                let pr_body = format!(
                    "Fixes #{}\n\nImplementation for #{}\n\n## Auto-Review Findings\n\nAuto-review found issues after {} attempts. Please review carefully:\n\n{}\n\n---\n*Generated by Hammurabi*",
                    issue.github_issue_number,
                    issue.github_issue_number,
                    review_count,
                    findings.chars().take(2000).collect::<String>()
                );
                let pr_number =
                    create_or_find_pr(ctx, &pr_title, &branch_name, &default_branch, &pr_body)
                        .await?;

                ctx.db.update_issue_impl_pr(issue.id, pr_number)?;
                ctx.db.update_issue_review_feedback(issue.id, None)?;
                ctx.db.reset_review_count(issue.id)?;
                ctx.db.update_issue_state(
                    issue.id,
                    IssueState::AwaitPRApproval,
                    Some(IssueState::Reviewing),
                )?;

                // Best-effort comment: DB state is already committed
                if let Err(e) = ctx
                    .publisher
                    .post(
                        issue.github_issue_number,
                        &format!(
                            "Auto-review found issues after {} attempts. Proceeding to human review. PR: #{}",
                            review_count, pr_number
                        ),
                    )
                    .await
                {
                    tracing::warn!(
                        issue = issue.github_issue_number,
                        error = %e,
                        "Failed to post max-iterations comment"
                    );
                }
            } else {
                tracing::info!(
                    issue = issue.github_issue_number,
                    review_count = review_count,
                    max = max_iterations,
                    "Review FAILED — sending back for revision"
                );

                // Extract findings and persist them before state change (crash-safe).
                // Truncate to avoid unbounded growth in DB and prompt context.
                let findings_full = prompts::extract_blocking_findings(&result.content);
                let findings: String = findings_full.chars().take(2000).collect();
                ctx.db
                    .update_issue_review_feedback(issue.id, Some(&findings))?;

                ctx.db.update_issue_state(
                    issue.id,
                    IssueState::Implementing,
                    Some(IssueState::Reviewing),
                )?;

                // Best-effort comment: DB state is already committed
                if let Err(e) = ctx
                    .publisher
                    .post(
                        issue.github_issue_number,
                        &format!(
                            "Auto-review found issues (attempt {}/{}). Revising implementation...\n\n{}",
                            review_count,
                            max_iterations,
                            findings.chars().take(1000).collect::<String>()
                        ),
                    )
                    .await
                {
                    tracing::warn!(
                        issue = issue.github_issue_number,
                        error = %e,
                        "Failed to post review-fail comment"
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::mock::MockAiAgent;
    use crate::agents::{AgentKind, AiResult};
    use crate::db::Database;
    use crate::github::mock::MockGitHubClient;
    use crate::github::GitHubIssue;
    use crate::worktree::mock::MockWorktreeManager;
    use std::sync::Arc;

    use crate::transitions::test_helpers::{test_config, test_registry_with};

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
        db.update_issue_state(
            issue.id,
            IssueState::Reviewing,
            Some(IssueState::Implementing),
        )
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
            agent_kind: AgentKind::ClaudeCli,
            tool_summary: Vec::new(),
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            publisher: std::sync::Arc::new(crate::publisher::GithubPublisher::new(gh.clone())),
            agents: test_registry_with(ai),
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
        assert!(comments
            .iter()
            .any(|(_, body)| body.contains("Auto-review passed")));

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
            agent_kind: AgentKind::ClaudeCli,
            tool_summary: Vec::new(),
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            publisher: std::sync::Arc::new(crate::publisher::GithubPublisher::new(gh.clone())),
            agents: test_registry_with(ai),
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
        assert!(updated
            .review_feedback
            .as_ref()
            .unwrap()
            .contains("Missing tests"));

        let comments = gh.created_comments.lock().unwrap();
        assert!(comments
            .iter()
            .any(|(_, body)| body.contains("Auto-review found issues")));

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
            agent_kind: AgentKind::ClaudeCli,
            tool_summary: Vec::new(),
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            publisher: std::sync::Arc::new(crate::publisher::GithubPublisher::new(gh.clone())),
            agents: test_registry_with(ai),
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
        assert!(comments
            .iter()
            .any(|(_, body)| body.contains("Proceeding to human review")));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_review_unparseable_verdict_creates_pr_with_advisory() {
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
            agent_kind: AgentKind::ClaudeCli,
            tool_summary: Vec::new(),
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            publisher: std::sync::Arc::new(crate::publisher::GithubPublisher::new(gh.clone())),
            agents: test_registry_with(ai),
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // Unparseable verdict (Unknown) still creates PR but with advisory messaging
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);
        assert!(updated.impl_pr_number.is_some());

        // PR body should contain advisory note, not "approved"
        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 1);
        let pr_body = &prs[0].3;
        assert!(pr_body.contains("could not be parsed"));
        assert!(!pr_body.contains("approved"));

        // Comment should also reflect the unknown verdict
        let comments = gh.created_comments.lock().unwrap();
        assert!(comments
            .iter()
            .any(|(_, body)| body.contains("could not be parsed")));

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
        db.update_issue_state(
            issue.id,
            IssueState::Reviewing,
            Some(IssueState::Implementing),
        )
        .unwrap();
        db.update_issue_impl_pr(issue.id, 42).unwrap();
        // Simulate stale review state from a previous FAIL cycle
        db.increment_review_count(issue.id).unwrap();
        db.update_issue_review_feedback(issue.id, Some("stale feedback"))
            .unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue.state, IssueState::Reviewing);
        assert_eq!(issue.impl_pr_number, Some(42));
        assert_eq!(issue.review_count, 1);
        assert!(issue.review_feedback.is_some());

        let ai = Arc::new(MockAiAgent::new());
        // AI should NOT be invoked — no response configured intentionally
        let tmp = std::env::temp_dir().join("hammurabi-test-review-idempotent");
        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let ctx = TransitionContext {
            github: gh.clone(),
            publisher: std::sync::Arc::new(crate::publisher::GithubPublisher::new(gh.clone())),
            agents: test_registry_with(ai.clone()),
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // Should transition to AwaitPRApproval with stale review state cleared
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitPRApproval);
        assert_eq!(updated.review_count, 0);
        assert!(updated.review_feedback.is_none());

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
