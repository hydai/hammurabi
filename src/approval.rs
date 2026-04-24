use crate::discord::DiscordClient;
use crate::error::HammurabiError;
use crate::github::{GitHubClient, PrStatus};

/// Result of checking comment-based approval (used for spec approval).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentApprovalResult {
    Approved { comment_id: u64 },
    Feedback { body: String, comment_id: u64 },
    Pending,
}

/// Outcome of scanning a Discord thread for `/confirm` / `/revise` /
/// `/cancel` commands. Mirrors `CommentApprovalResult` but with Discord
/// message ids and an explicit cancellation variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DiscordApprovalResult {
    /// User typed `/confirm`. `message_id` is the confirming message's snowflake.
    Confirmed { message_id: u64 },
    /// User typed `/revise <text>`. `feedback` is the text after the command.
    Revised { feedback: String, message_id: u64 },
    /// User typed `/cancel`.
    Cancelled { message_id: u64 },
    /// No actionable command found since `since_id`.
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

/// Scan a Discord thread for `/confirm`, `/revise <text>`, or `/cancel`
/// commands from an authorized approver. The latest command wins —
/// a `/revise` after `/confirm` re-opens the draft loop.
///
/// `command_prefix` is usually `"/"` (matches `/confirm`, `/revise`);
/// the prefix is stripped before the command name is parsed.
#[allow(dead_code)]
pub async fn check_discord_approval(
    client: &dyn DiscordClient,
    thread_id: u64,
    since_id: Option<u64>,
    approvers: &[String],
    command_prefix: &str,
) -> Result<DiscordApprovalResult, HammurabiError> {
    let msgs = client.fetch_thread_messages(thread_id, since_id).await?;

    let mut latest: Option<DiscordApprovalResult> = None;

    for msg in &msgs {
        if !approvers.iter().any(|a| a == &msg.author_username) {
            continue;
        }
        let trimmed = msg.content.trim();
        let Some(command) = trimmed.strip_prefix(command_prefix) else {
            continue;
        };
        // Split command and tail arg (`revise` + "<text>").
        let mut parts = command.splitn(2, char::is_whitespace);
        let (name, tail) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        match name {
            "confirm" => {
                latest = Some(DiscordApprovalResult::Confirmed { message_id: msg.id });
            }
            "revise" => {
                let feedback = tail.trim().to_string();
                if feedback.is_empty() {
                    continue;
                }
                latest = Some(DiscordApprovalResult::Revised {
                    feedback,
                    message_id: msg.id,
                });
            }
            "cancel" => {
                latest = Some(DiscordApprovalResult::Cancelled { message_id: msg.id });
            }
            _ => {}
        }
    }

    Ok(latest.unwrap_or(DiscordApprovalResult::Pending))
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

    // --- Discord approval tests ---

    use crate::discord::mock::MockDiscordClient;
    use crate::discord::DiscordMessage;

    fn seed_thread_msg(c: &MockDiscordClient, thread: u64, user: &str, body: &str) -> u64 {
        c.add_thread_message(
            thread,
            DiscordMessage {
                id: 0,
                channel_id: thread,
                thread_id: Some(thread),
                author_id: 0,
                author_username: user.into(),
                content: body.into(),
                mentions_bot: false,
            },
        )
    }

    #[tokio::test]
    async fn discord_confirm_wins_over_earlier_revise() {
        let c = MockDiscordClient::new();
        seed_thread_msg(&c, 100, "hydai", "/revise make it bigger");
        seed_thread_msg(&c, 100, "hydai", "/confirm");

        let result = check_discord_approval(&c, 100, None, &["hydai".to_string()], "/")
            .await
            .unwrap();
        assert!(matches!(result, DiscordApprovalResult::Confirmed { .. }));
    }

    #[tokio::test]
    async fn discord_revise_captures_feedback() {
        let c = MockDiscordClient::new();
        seed_thread_msg(
            &c,
            100,
            "hydai",
            "/revise also respect prefers-color-scheme",
        );

        let result = check_discord_approval(&c, 100, None, &["hydai".to_string()], "/")
            .await
            .unwrap();
        match result {
            DiscordApprovalResult::Revised { feedback, .. } => {
                assert_eq!(feedback, "also respect prefers-color-scheme");
            }
            _ => panic!("expected Revised"),
        }
    }

    #[tokio::test]
    async fn discord_non_approver_is_ignored() {
        let c = MockDiscordClient::new();
        seed_thread_msg(&c, 100, "eve", "/confirm");

        let result = check_discord_approval(&c, 100, None, &["hydai".to_string()], "/")
            .await
            .unwrap();
        assert_eq!(result, DiscordApprovalResult::Pending);
    }

    #[tokio::test]
    async fn discord_revise_without_feedback_is_ignored() {
        let c = MockDiscordClient::new();
        seed_thread_msg(&c, 100, "hydai", "/revise");

        let result = check_discord_approval(&c, 100, None, &["hydai".to_string()], "/")
            .await
            .unwrap();
        assert_eq!(result, DiscordApprovalResult::Pending);
    }

    #[tokio::test]
    async fn discord_cancel_is_distinct_outcome() {
        let c = MockDiscordClient::new();
        seed_thread_msg(&c, 100, "hydai", "/cancel");

        let result = check_discord_approval(&c, 100, None, &["hydai".to_string()], "/")
            .await
            .unwrap();
        assert!(matches!(result, DiscordApprovalResult::Cancelled { .. }));
    }

    #[tokio::test]
    async fn discord_since_id_filters_older_commands() {
        let c = MockDiscordClient::new();
        let id_old = seed_thread_msg(&c, 100, "hydai", "/confirm");
        let _id_new = seed_thread_msg(&c, 100, "hydai", "hello world");

        // Scan after the /confirm message — should see only "hello world"
        let result = check_discord_approval(&c, 100, Some(id_old), &["hydai".to_string()], "/")
            .await
            .unwrap();
        assert_eq!(result, DiscordApprovalResult::Pending);
    }
}
