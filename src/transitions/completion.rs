use crate::approval::{self, SubPrApprovalResult};
use crate::error::HammurabiError;
use crate::models::{IssueState, SubIssueState, TrackedIssue};

use super::TransitionContext;

pub async fn check(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let sub_issues = ctx.db.get_sub_issues(issue.id)?;

    let pr_subs: Vec<_> = sub_issues
        .iter()
        .filter(|s| s.pr_number.is_some())
        .cloned()
        .collect();

    if pr_subs.is_empty() {
        return Ok(());
    }

    match approval::check_sub_pr_approvals(&*ctx.github, &pr_subs).await? {
        SubPrApprovalResult::AllMerged => {
            // Update all sub-issues to done
            for sub in &pr_subs {
                ctx.db
                    .update_sub_issue_state(sub.id, SubIssueState::Done)?;
            }

            ctx.db.update_issue_state(
                issue.id,
                IssueState::Done,
                Some(IssueState::AwaitSubPRApprovals),
            )?;

            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    "All sub-issue PRs merged. Issue complete!",
                )
                .await?;

            tracing::info!(
                issue = issue.github_issue_number,
                "All sub-PRs merged, issue complete"
            );
        }
        SubPrApprovalResult::AnyClosedWithoutMerge => {
            ctx.db.update_issue_state(
                issue.id,
                IssueState::Failed,
                Some(IssueState::AwaitSubPRApprovals),
            )?;
            ctx.db.update_issue_error(
                issue.id,
                "A sub-issue PR was closed without merge",
            )?;

            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    "A sub-issue PR was closed without merge. Use `/retry` to retry.",
                )
                .await?;
        }
        SubPrApprovalResult::Pending => {
            // Nothing to do, still waiting
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::Database;
    use crate::github::mock::MockGitHubClient;
    use crate::github::PrStatus;
    use crate::claude::mock::MockAiAgent;
    use crate::worktree::mock::MockWorktreeManager;
    use std::sync::Arc;

    fn test_config() -> Config {
        Config {
            repo: "owner/repo".to_string(),
            owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            poll_interval: 60,
            max_concurrent_agents: 3,
            tracking_label: "hammurabi".to_string(),
            stale_timeout_days: 7,
            api_retry_count: 3,
            ai_model: "test-model".to_string(),
            ai_max_turns: 50,
            approvers: vec!["alice".to_string()],
            github_token: "token".to_string(),
            spec: None,
            decompose: None,
            implement: None,
        }
    }

    #[tokio::test]
    async fn test_all_merged_completes_issue() {
        let tmp = std::env::temp_dir().join("hammurabi-test-completion");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.set_pr_status(10, PrStatus::Merged);
        gh.set_pr_status(11, PrStatus::Merged);

        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();
        let sub1 = db.insert_sub_issue(issue.id, "Task 1", "").unwrap();
        let sub2 = db.insert_sub_issue(issue.id, "Task 2", "").unwrap();
        db.update_sub_issue_state(sub1, SubIssueState::PrOpen).unwrap();
        db.update_sub_issue_pr(sub1, 10).unwrap();
        db.update_sub_issue_state(sub2, SubIssueState::PrOpen).unwrap();
        db.update_sub_issue_pr(sub2, 11).unwrap();

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
        gh.set_pr_status(10, PrStatus::Merged);
        gh.set_pr_status(11, PrStatus::ClosedWithoutMerge);

        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();
        let sub1 = db.insert_sub_issue(issue.id, "Task 1", "").unwrap();
        let sub2 = db.insert_sub_issue(issue.id, "Task 2", "").unwrap();
        db.update_sub_issue_state(sub1, SubIssueState::PrOpen).unwrap();
        db.update_sub_issue_pr(sub1, 10).unwrap();
        db.update_sub_issue_state(sub2, SubIssueState::PrOpen).unwrap();
        db.update_sub_issue_pr(sub2, 11).unwrap();

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
}
