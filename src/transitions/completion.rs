use crate::approval::{self, PrApprovalResult};
use crate::error::HammurabiError;
use crate::models::{IssueState, TrackedIssue};

use super::TransitionContext;

pub async fn check(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let pr_number = match issue.impl_pr_number {
        Some(n) => n,
        None => return Ok(()),
    };

    match approval::check_pr_approval(&*ctx.github, pr_number).await? {
        PrApprovalResult::Merged => {
            ctx.db.update_issue_state(
                issue.id,
                IssueState::Done,
                Some(IssueState::AwaitPRApproval),
            )?;

            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    "Implementation PR merged. Issue complete!",
                )
                .await?;

            tracing::info!(
                issue = issue.github_issue_number,
                pr = pr_number,
                "PR merged, issue complete"
            );
        }
        PrApprovalResult::ClosedWithoutMerge => {
            ctx.db.update_issue_state(
                issue.id,
                IssueState::Failed,
                Some(IssueState::AwaitPRApproval),
            )?;
            ctx.db.update_issue_error(
                issue.id,
                "Implementation PR was closed without merge",
            )?;

            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    "Implementation PR was closed without merge. Use `/retry` to retry.",
                )
                .await?;
        }
        PrApprovalResult::Pending => {
            // Still waiting
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::mock::MockAiAgent;
    use crate::config::Config;
    use crate::db::Database;
    use crate::github::mock::MockGitHubClient;
    use crate::github::PrStatus;
    use crate::worktree::mock::MockWorktreeManager;
    use std::sync::Arc;

    fn test_config() -> Config {
        Config {
            repo: "owner/repo".to_string(),
            owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            poll_interval: 60,
            tracking_label: "hammurabi".to_string(),
            stale_timeout_days: 7,
            api_retry_count: 3,
            ai_model: "test-model".to_string(),
            ai_max_turns: 50,
            ai_effort: "high".to_string(),
            ai_timeout_secs: 3600,
            ai_stall_timeout_secs: 300,
            ai_max_retries: 2,
            approvers: vec!["alice".to_string()],
            github_auth: crate::config::GitHubAuth::Token("token".to_string()),
            spec: None,
            implement: None,
        }
    }

    #[tokio::test]
    async fn test_pr_merged_completes_issue() {
        let tmp = std::env::temp_dir().join("hammurabi-test-completion");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.set_pr_status(10, PrStatus::Merged);

        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();
        db.update_issue_impl_pr(issue.id, 10).unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai: Arc::new(MockAiAgent::new()),
            worktree: Arc::new(MockWorktreeManager::new(tmp.clone())),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        check(&ctx, &issue).await.unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::Done);

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_pr_closed_fails_issue() {
        let tmp = std::env::temp_dir().join("hammurabi-test-completion-fail");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.set_pr_status(10, PrStatus::ClosedWithoutMerge);

        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();
        db.update_issue_impl_pr(issue.id, 10).unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh,
            ai: Arc::new(MockAiAgent::new()),
            worktree: Arc::new(MockWorktreeManager::new(tmp.clone())),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        check(&ctx, &issue).await.unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::Failed);

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_no_pr_number_does_nothing() {
        let tmp = std::env::temp_dir().join("hammurabi-test-completion-nopr");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh,
            ai: Arc::new(MockAiAgent::new()),
            worktree: Arc::new(MockWorktreeManager::new(tmp.clone())),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        check(&ctx, &issue).await.unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::Discovered); // unchanged

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
