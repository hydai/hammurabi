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

use crate::discord::DiscordClient;
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

/// `Publisher` backed by a `DiscordClient`: posts go to
/// `DiscordClient::post_message`, edits go to `DiscordClient::edit_message`.
/// `thread_id` in the trait corresponds to the Discord thread snowflake
/// (threads in Discord are channels, so routing is by the same ID).
#[allow(dead_code)]
pub struct DiscordPublisher {
    client: Arc<dyn DiscordClient>,
}

#[allow(dead_code)]
impl DiscordPublisher {
    pub fn new(client: Arc<dyn DiscordClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Publisher for DiscordPublisher {
    async fn post(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError> {
        self.client.post_message(thread_id, body).await
    }

    async fn update(
        &self,
        thread_id: u64,
        message_id: u64,
        body: &str,
    ) -> Result<(), HammurabiError> {
        self.client.edit_message(thread_id, message_id, body).await
    }
}

/// Fan progress updates out to multiple publishers — used by the Discord
/// lifecycle so a status message lands on both the GitHub issue (once
/// created at `/confirm`) and the original Discord thread. The first
/// publisher's message id is the one returned from `post`; updates are
/// routed to every member so edits stay in sync.
#[allow(dead_code)]
pub struct MultiplexPublisher {
    members: Vec<Arc<dyn Publisher>>,
}

#[allow(dead_code)]
impl MultiplexPublisher {
    pub fn new(members: Vec<Arc<dyn Publisher>>) -> Self {
        Self { members }
    }
}

#[async_trait]
impl Publisher for MultiplexPublisher {
    async fn post(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError> {
        let mut primary_id: Option<u64> = None;
        for m in &self.members {
            match m.post(thread_id, body).await {
                Ok(id) => {
                    if primary_id.is_none() {
                        primary_id = Some(id);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "multiplex publisher: member post failed, continuing"
                    );
                }
            }
        }
        primary_id.ok_or_else(|| {
            HammurabiError::GitHub("multiplex publisher had no successful post".to_string())
        })
    }

    async fn update(
        &self,
        thread_id: u64,
        message_id: u64,
        body: &str,
    ) -> Result<(), HammurabiError> {
        for m in &self.members {
            if let Err(e) = m.update(thread_id, message_id, body).await {
                tracing::warn!(
                    error = %e,
                    "multiplex publisher: member update failed, continuing"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::mock::MockDiscordClient;
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

    #[tokio::test]
    async fn discord_publisher_post_routes_to_post_message() {
        let dc = Arc::new(MockDiscordClient::new());
        let publisher = DiscordPublisher::new(dc.clone());

        let id = publisher.post(555, "draft v1").await.unwrap();
        assert!(id > 0);

        let posts = dc.posted_messages.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, 555);
        assert_eq!(posts[0].1, "draft v1");
    }

    #[tokio::test]
    async fn discord_publisher_update_routes_to_edit_message() {
        let dc = Arc::new(MockDiscordClient::new());
        let publisher = DiscordPublisher::new(dc.clone());

        let id = publisher.post(666, "v1").await.unwrap();
        publisher.update(666, id, "v2").await.unwrap();

        let edits = dc.edited_messages.lock().unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].0, 666);
        assert_eq!(edits[0].1, id);
        assert_eq!(edits[0].2, "v2");
    }

    #[tokio::test]
    async fn multiplex_fans_post_to_all_members() {
        let gh = Arc::new(MockGitHubClient::new());
        let dc = Arc::new(MockDiscordClient::new());
        let members: Vec<Arc<dyn Publisher>> = vec![
            Arc::new(GithubPublisher::new(gh.clone())),
            Arc::new(DiscordPublisher::new(dc.clone())),
        ];
        let mux = MultiplexPublisher::new(members);

        let id = mux.post(123, "status").await.unwrap();
        assert!(id > 0);

        assert_eq!(gh.created_comments.lock().unwrap().len(), 1);
        assert_eq!(dc.posted_messages.lock().unwrap().len(), 1);
    }
}
