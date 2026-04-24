//! Source-agnostic progress publishing.
//!
//! Transitions and the progress aggregator emit status messages through a
//! [`Publisher`] rather than calling `GitHubClient` directly, so Discord and
//! any future channels can back the same call sites. GitHub remains the
//! backing surface for all repo-level operations (PR create/merge, label
//! reads); `Publisher` covers only the "post and later update a message on
//! a thread" concern.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::HammurabiError;
use crate::github::GitHubClient;

/// Post status messages on a thread and edit them in place. For GitHub a
/// "thread" is an issue or PR number; for Discord it will be a thread/channel
/// ID. Identifiers are `u64` because every supported backend uses 64-bit
/// snowflakes or issue numbers.
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Post a new message on the given thread. Returns the new message id.
    async fn post(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError>;

    /// Replace the body of a previously-posted message. `thread_id` is
    /// required by some backends (e.g. Discord needs the channel_id to
    /// route an edit) and ignored by others (GitHub comment ids are
    /// globally unique within a repo).
    async fn update(
        &self,
        thread_id: u64,
        message_id: u64,
        body: &str,
    ) -> Result<(), HammurabiError>;
}

/// `Publisher` backed by a `GitHubClient`: posts go to
/// `post_issue_comment`, edits go to `update_issue_comment`.
pub struct GithubPublisher {
    github: Arc<dyn GitHubClient>,
}

impl GithubPublisher {
    pub fn new(github: Arc<dyn GitHubClient>) -> Self {
        Self { github }
    }
}

#[async_trait]
impl Publisher for GithubPublisher {
    async fn post(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError> {
        self.github.post_issue_comment(thread_id, body).await
    }

    async fn update(
        &self,
        _thread_id: u64,
        message_id: u64,
        body: &str,
    ) -> Result<(), HammurabiError> {
        self.github.update_issue_comment(message_id, body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::mock::MockGitHubClient;

    #[tokio::test]
    async fn github_publisher_post_routes_to_issue_comment() {
        let gh = Arc::new(MockGitHubClient::new());
        let publisher = GithubPublisher::new(gh.clone());

        let id = publisher.post(42, "hello").await.unwrap();
        assert!(id > 0);

        let posted = gh.created_comments.lock().unwrap();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].0, 42);
        assert_eq!(posted[0].1, "hello");
    }

    #[tokio::test]
    async fn github_publisher_update_routes_to_update_issue_comment() {
        let gh = Arc::new(MockGitHubClient::new());
        let publisher = GithubPublisher::new(gh.clone());

        let id = publisher.post(42, "first").await.unwrap();
        publisher.update(42, id, "second").await.unwrap();

        let updated = gh.updated_comments.lock().unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].0, id);
        assert_eq!(updated[0].1, "second");
    }
}
