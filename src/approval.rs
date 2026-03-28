use crate::error::HammurabiError;
use crate::github::{GitHubClient, PrStatus};
use crate::models::SubIssue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecApprovalResult {
    Approved,
    Rejected,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecompApprovalResult {
    Approved { comment_id: u64 },
    Feedback { body: String, comment_id: u64 },
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubPrApprovalResult {
    AllMerged,
    AnyClosedWithoutMerge,
    Pending,
}

pub async fn check_spec_approval(
    github: &dyn GitHubClient,
    pr_number: u64,
) -> Result<SpecApprovalResult, HammurabiError> {
    match github.get_pr_status(pr_number).await? {
        PrStatus::Merged => Ok(SpecApprovalResult::Approved),
        PrStatus::ClosedWithoutMerge => Ok(SpecApprovalResult::Rejected),
        PrStatus::Open => Ok(SpecApprovalResult::Pending),
    }
}

pub async fn check_decomp_approval(
    github: &dyn GitHubClient,
    issue_number: u64,
    last_comment_id: Option<u64>,
    approvers: &[String],
) -> Result<DecompApprovalResult, HammurabiError> {
    let comments = github
        .get_issue_comments(issue_number, last_comment_id)
        .await?;

    // Process comments in order to find the most recent actionable one from an approver
    let mut last_approve: Option<u64> = None;
    let mut last_feedback: Option<(String, u64)> = None;

    for comment in &comments {
        if !approvers.contains(&comment.user_login) {
            continue;
        }

        let trimmed = comment.body.trim();
        if trimmed == "/approve" {
            last_approve = Some(comment.id);
            last_feedback = None; // /approve supersedes earlier feedback
        } else {
            last_feedback = Some((trimmed.to_string(), comment.id));
            last_approve = None; // feedback supersedes earlier /approve
        }
    }

    // Return the most recent actionable result
    if let Some(comment_id) = last_approve {
        return Ok(DecompApprovalResult::Approved { comment_id });
    }

    if let Some((body, comment_id)) = last_feedback {
        return Ok(DecompApprovalResult::Feedback { body, comment_id });
    }

    Ok(DecompApprovalResult::Pending)
}

pub async fn check_sub_pr_approvals(
    github: &dyn GitHubClient,
    sub_issues: &[SubIssue],
) -> Result<SubPrApprovalResult, HammurabiError> {
    let mut all_merged = true;

    for sub in sub_issues {
        if let Some(pr_number) = sub.pr_number {
            match github.get_pr_status(pr_number).await? {
                PrStatus::Merged => {}
                PrStatus::ClosedWithoutMerge => {
                    return Ok(SubPrApprovalResult::AnyClosedWithoutMerge);
                }
                PrStatus::Open => {
                    all_merged = false;
                }
            }
        } else {
            all_merged = false;
        }
    }

    if all_merged {
        Ok(SubPrApprovalResult::AllMerged)
    } else {
        Ok(SubPrApprovalResult::Pending)
    }
}

pub async fn check_retry_comment(
    github: &dyn GitHubClient,
    issue_number: u64,
    last_comment_id: Option<u64>,
    approvers: &[String],
) -> Result<Option<u64>, HammurabiError> {
    let comments = github
        .get_issue_comments(issue_number, last_comment_id)
        .await?;

    for comment in comments.iter().rev() {
        if approvers.contains(&comment.user_login) && comment.body.trim() == "/retry" {
            return Ok(Some(comment.id));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::mock::MockGitHubClient;
    use crate::github::GitHubComment;
    use crate::models::SubIssueState;

    #[tokio::test]
    async fn test_spec_approved() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Merged);
        let result = check_spec_approval(&gh, 10).await.unwrap();
        assert_eq!(result, SpecApprovalResult::Approved);
    }

    #[tokio::test]
    async fn test_spec_rejected() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::ClosedWithoutMerge);
        let result = check_spec_approval(&gh, 10).await.unwrap();
        assert_eq!(result, SpecApprovalResult::Rejected);
    }

    #[tokio::test]
    async fn test_spec_pending() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Open);
        let result = check_spec_approval(&gh, 10).await.unwrap();
        assert_eq!(result, SpecApprovalResult::Pending);
    }

    #[tokio::test]
    async fn test_decomp_approved_by_approver() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "/approve".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_decomp_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, DecompApprovalResult::Approved { comment_id: 100 });
    }

    #[tokio::test]
    async fn test_decomp_approve_from_unauthorized_ignored() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "/approve".to_string(),
                user_login: "eve".to_string(),
            },
        );

        let result = check_decomp_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, DecompApprovalResult::Pending);
    }

    #[tokio::test]
    async fn test_decomp_feedback() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "Add more detail to sub-issue 2".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_decomp_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            DecompApprovalResult::Feedback {
                body: "Add more detail to sub-issue 2".to_string(),
                comment_id: 100,
            }
        );
    }

    #[tokio::test]
    async fn test_decomp_most_recent_feedback_wins() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "First feedback".to_string(),
                user_login: "alice".to_string(),
            },
        );
        gh.add_comment(
            1,
            GitHubComment {
                id: 101,
                body: "Second feedback".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_decomp_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            DecompApprovalResult::Feedback {
                body: "Second feedback".to_string(),
                comment_id: 101,
            }
        );
    }

    #[tokio::test]
    async fn test_decomp_approve_after_feedback() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "Feedback".to_string(),
                user_login: "alice".to_string(),
            },
        );
        gh.add_comment(
            1,
            GitHubComment {
                id: 101,
                body: "/approve".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_decomp_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, DecompApprovalResult::Approved { comment_id: 101 });
    }

    #[tokio::test]
    async fn test_decomp_feedback_after_approve() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "/approve".to_string(),
                user_login: "alice".to_string(),
            },
        );
        gh.add_comment(
            1,
            GitHubComment {
                id: 101,
                body: "Wait, actually change this".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_decomp_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            DecompApprovalResult::Feedback {
                body: "Wait, actually change this".to_string(),
                comment_id: 101,
            }
        );
    }

    #[tokio::test]
    async fn test_decomp_since_id_filters() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "/approve".to_string(),
                user_login: "alice".to_string(),
            },
        );
        gh.add_comment(
            1,
            GitHubComment {
                id: 101,
                body: "some comment".to_string(),
                user_login: "bob".to_string(),
            },
        );

        // With since_id=100, the /approve at id=100 is filtered out
        let result = check_decomp_approval(&gh, 1, Some(100), &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, DecompApprovalResult::Pending);
    }

    #[tokio::test]
    async fn test_sub_prs_all_merged() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Merged);
        gh.set_pr_status(11, PrStatus::Merged);

        let subs = vec![
            SubIssue {
                id: 1,
                parent_issue_id: 1,
                github_issue_number: Some(100),
                title: "Sub 1".to_string(),
                description: String::new(),
                state: SubIssueState::PrOpen,
                pr_number: Some(10),
                worktree_path: None,
                session_id: None,
            },
            SubIssue {
                id: 2,
                parent_issue_id: 1,
                github_issue_number: Some(101),
                title: "Sub 2".to_string(),
                description: String::new(),
                state: SubIssueState::PrOpen,
                pr_number: Some(11),
                worktree_path: None,
                session_id: None,
            },
        ];

        let result = check_sub_pr_approvals(&gh, &subs).await.unwrap();
        assert_eq!(result, SubPrApprovalResult::AllMerged);
    }

    #[tokio::test]
    async fn test_sub_prs_one_closed() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Merged);
        gh.set_pr_status(11, PrStatus::ClosedWithoutMerge);

        let subs = vec![
            SubIssue {
                id: 1,
                parent_issue_id: 1,
                github_issue_number: Some(100),
                title: "Sub 1".to_string(),
                description: String::new(),
                state: SubIssueState::PrOpen,
                pr_number: Some(10),
                worktree_path: None,
                session_id: None,
            },
            SubIssue {
                id: 2,
                parent_issue_id: 1,
                github_issue_number: Some(101),
                title: "Sub 2".to_string(),
                description: String::new(),
                state: SubIssueState::PrOpen,
                pr_number: Some(11),
                worktree_path: None,
                session_id: None,
            },
        ];

        let result = check_sub_pr_approvals(&gh, &subs).await.unwrap();
        assert_eq!(result, SubPrApprovalResult::AnyClosedWithoutMerge);
    }

    #[tokio::test]
    async fn test_sub_prs_pending() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Merged);
        gh.set_pr_status(11, PrStatus::Open);

        let subs = vec![
            SubIssue {
                id: 1,
                parent_issue_id: 1,
                github_issue_number: Some(100),
                title: "Sub 1".to_string(),
                description: String::new(),
                state: SubIssueState::PrOpen,
                pr_number: Some(10),
                worktree_path: None,
                session_id: None,
            },
            SubIssue {
                id: 2,
                parent_issue_id: 1,
                github_issue_number: Some(101),
                title: "Sub 2".to_string(),
                description: String::new(),
                state: SubIssueState::PrOpen,
                pr_number: Some(11),
                worktree_path: None,
                session_id: None,
            },
        ];

        let result = check_sub_pr_approvals(&gh, &subs).await.unwrap();
        assert_eq!(result, SubPrApprovalResult::Pending);
    }

    #[tokio::test]
    async fn test_retry_comment_found() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 200,
                body: "/retry".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_retry_comment(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, Some(200));
    }

    #[tokio::test]
    async fn test_retry_comment_from_unauthorized() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 200,
                body: "/retry".to_string(),
                user_login: "eve".to_string(),
            },
        );

        let result = check_retry_comment(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_retry_no_comment() {
        let gh = MockGitHubClient::new();
        let result = check_retry_comment(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, None);
    }
}
