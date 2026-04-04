use crate::error::HammurabiError;
use crate::github::{GitHubClient, PrStatus};

/// Result of checking comment-based approval (used for spec approval).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentApprovalResult {
    Approved { comment_id: u64 },
    Feedback { body: String, comment_id: u64 },
    Pending,
}

/// Result of checking a PR's merge status (used for implementation PR).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrApprovalResult {
    Merged,
    ClosedWithoutMerge,
    Pending,
}

/// Check for `/approve` or feedback comments from authorized approvers.
/// Used for spec approval — scans comments since `last_comment_id`.
///
/// Iterates forward so the last comment wins: an `/approve` after feedback
/// approves, and feedback after `/approve` re-opens the review loop.
pub async fn check_comment_approval(
    github: &dyn GitHubClient,
    issue_number: u64,
    last_comment_id: Option<u64>,
    approvers: &[String],
) -> Result<CommentApprovalResult, HammurabiError> {
    let comments = github
        .get_issue_comments(issue_number, last_comment_id)
        .await?;

    let mut last_approve: Option<u64> = None;
    let mut last_feedback: Option<(String, u64)> = None;

    for comment in &comments {
        if !approvers.contains(&comment.user_login) {
            continue;
        }

        let trimmed = comment.body.trim();
        if trimmed == "/approve" {
            last_approve = Some(comment.id);
            last_feedback = None;
        } else {
            last_feedback = Some((trimmed.to_string(), comment.id));
            last_approve = None;
        }
    }

    if let Some(comment_id) = last_approve {
        return Ok(CommentApprovalResult::Approved { comment_id });
    }

    if let Some((body, comment_id)) = last_feedback {
        return Ok(CommentApprovalResult::Feedback { body, comment_id });
    }

    Ok(CommentApprovalResult::Pending)
}

/// Check the merge status of the implementation PR.
pub async fn check_pr_approval(
    github: &dyn GitHubClient,
    pr_number: u64,
) -> Result<PrApprovalResult, HammurabiError> {
    match github.get_pr_status(pr_number).await? {
        PrStatus::Merged => Ok(PrApprovalResult::Merged),
        PrStatus::ClosedWithoutMerge => Ok(PrApprovalResult::ClosedWithoutMerge),
        PrStatus::Open => Ok(PrApprovalResult::Pending),
    }
}

/// Check for a `/retry` comment from an authorized approver.
///
/// Iterates in reverse to short-circuit on the most recent `/retry` —
/// only one retry is needed regardless of how many were posted.
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

    #[tokio::test]
    async fn test_comment_approved_by_approver() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "/approve".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_comment_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, CommentApprovalResult::Approved { comment_id: 100 });
    }

    #[tokio::test]
    async fn test_comment_approve_from_unauthorized_ignored() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "/approve".to_string(),
                user_login: "eve".to_string(),
            },
        );

        let result = check_comment_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, CommentApprovalResult::Pending);
    }

    #[tokio::test]
    async fn test_comment_feedback() {
        let gh = MockGitHubClient::new();
        gh.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "Add more detail to the spec".to_string(),
                user_login: "alice".to_string(),
            },
        );

        let result = check_comment_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            CommentApprovalResult::Feedback {
                body: "Add more detail to the spec".to_string(),
                comment_id: 100,
            }
        );
    }

    #[tokio::test]
    async fn test_most_recent_feedback_wins() {
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

        let result = check_comment_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            CommentApprovalResult::Feedback {
                body: "Second feedback".to_string(),
                comment_id: 101,
            }
        );
    }

    #[tokio::test]
    async fn test_approve_after_feedback() {
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

        let result = check_comment_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, CommentApprovalResult::Approved { comment_id: 101 });
    }

    #[tokio::test]
    async fn test_feedback_after_approve() {
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

        let result = check_comment_approval(&gh, 1, None, &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(
            result,
            CommentApprovalResult::Feedback {
                body: "Wait, actually change this".to_string(),
                comment_id: 101,
            }
        );
    }

    #[tokio::test]
    async fn test_since_id_filters() {
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

        let result = check_comment_approval(&gh, 1, Some(100), &["alice".to_string()])
            .await
            .unwrap();
        assert_eq!(result, CommentApprovalResult::Pending);
    }

    #[tokio::test]
    async fn test_pr_merged() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Merged);
        let result = check_pr_approval(&gh, 10).await.unwrap();
        assert_eq!(result, PrApprovalResult::Merged);
    }

    #[tokio::test]
    async fn test_pr_closed() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::ClosedWithoutMerge);
        let result = check_pr_approval(&gh, 10).await.unwrap();
        assert_eq!(result, PrApprovalResult::ClosedWithoutMerge);
    }

    #[tokio::test]
    async fn test_pr_pending() {
        let gh = MockGitHubClient::new();
        gh.set_pr_status(10, PrStatus::Open);
        let result = check_pr_approval(&gh, 10).await.unwrap();
        assert_eq!(result, PrApprovalResult::Pending);
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
