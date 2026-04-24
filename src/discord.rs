//! Discord intake client abstraction.
//!
//! The trait lets the rest of the daemon talk to Discord without depending
//! on the `serenity` crate — the real serenity-backed impl is gated behind
//! the `discord` Cargo feature and tests use the in-memory mock here.
//!
//! Identity conventions (Discord-side):
//! - a "channel" is where the bot is @mentioned to start a new intake.
//! - a "thread" is a child channel opened from the triggering message;
//!   its snowflake becomes `TrackedIssue.external_id`.
//! - message snowflakes are globally unique, so `Publisher::update` can
//!   route edits with the thread_id + message_id pair.

use async_trait::async_trait;

use crate::error::HammurabiError;

/// A Discord message relevant to our lifecycle: either an incoming idea in
/// an allowlisted channel, a reply inside a draft-spec thread, or a status
/// message we previously posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMessage {
    pub id: u64,
    pub channel_id: u64,
    /// `Some(thread_id)` if this message lives inside a thread. For the
    /// initial @mention that triggers thread creation, this is `None`.
    pub thread_id: Option<u64>,
    pub author_id: u64,
    pub author_username: String,
    pub content: String,
    /// Set by the ingest path — true if the bot was explicitly mentioned
    /// (by ID or role) in `content`.
    pub mentions_bot: bool,
}

/// Source-agnostic thread identity. The `channel_id` is the *parent*
/// channel for messages that live in a thread; for root-channel messages
/// it's the channel itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub struct DiscordThreadRef {
    pub channel_id: u64,
    pub thread_id: u64,
}

#[async_trait]
#[allow(dead_code)]
pub trait DiscordClient: Send + Sync {
    /// Fetch root-channel messages since `since_id`. The poller uses this
    /// to discover new @mentions in the allowlisted channel. Messages
    /// already inside a thread are **not** returned here.
    async fn fetch_new_messages(
        &self,
        channel_id: u64,
        since_id: Option<u64>,
    ) -> Result<Vec<DiscordMessage>, HammurabiError>;

    /// Fetch all messages in a thread since `since_id`. Used by the
    /// approval checker to scan for `/revise` / `/confirm` replies.
    async fn fetch_thread_messages(
        &self,
        thread_id: u64,
        since_id: Option<u64>,
    ) -> Result<Vec<DiscordMessage>, HammurabiError>;

    /// Post a message into a thread (or root channel). Returns the new
    /// message's snowflake. Acts as the backing operation for
    /// `Publisher::post`.
    async fn post_message(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError>;

    /// Edit a previously-posted message. Discord edits require the
    /// containing channel/thread in the API path, which is why
    /// `thread_id` is taken here and (harmlessly) unused by GitHub.
    async fn edit_message(
        &self,
        thread_id: u64,
        message_id: u64,
        body: &str,
    ) -> Result<(), HammurabiError>;

    /// Open a new thread rooted at `message_id` in `channel_id`. Returns
    /// the new thread's snowflake. The poller calls this when it first
    /// sees an @mention so all follow-up chat is scoped to one thread.
    async fn start_thread(
        &self,
        channel_id: u64,
        message_id: u64,
        name: &str,
    ) -> Result<u64, HammurabiError>;
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// In-memory `DiscordClient` used across unit and integration tests.
    ///
    /// Setters (`add_root_message`, `add_thread_message`) seed incoming
    /// fixture data; getters on the public fields (`posted_messages`,
    /// `edited_messages`, `created_threads`) let tests assert on outgoing
    /// traffic. Mirrors the shape of `MockGitHubClient`.
    pub struct MockDiscordClient {
        /// Keyed by `channel_id` — messages sent into the root channel
        /// (i.e. `thread_id == None`).
        root_messages: Mutex<HashMap<u64, Vec<DiscordMessage>>>,
        /// Keyed by `thread_id` — every message (including our own posts)
        /// inside that thread, in insertion order.
        thread_messages: Mutex<HashMap<u64, Vec<DiscordMessage>>>,

        /// Posts made via `post_message(thread_id, body)`, ordered.
        pub posted_messages: Mutex<Vec<(u64, String, u64)>>,
        /// Edits made via `edit_message`, ordered.
        pub edited_messages: Mutex<Vec<(u64, u64, String)>>,
        /// Threads opened via `start_thread`, ordered `(channel_id, seed_message_id, name, new_thread_id)`.
        pub created_threads: Mutex<Vec<(u64, u64, String, u64)>>,

        next_id: AtomicU64,
    }

    impl Default for MockDiscordClient {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MockDiscordClient {
        pub fn new() -> Self {
            Self {
                root_messages: Mutex::new(HashMap::new()),
                thread_messages: Mutex::new(HashMap::new()),
                posted_messages: Mutex::new(Vec::new()),
                edited_messages: Mutex::new(Vec::new()),
                created_threads: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1_000_000),
            }
        }

        fn fresh_id(&self) -> u64 {
            self.next_id.fetch_add(1, Ordering::SeqCst)
        }

        /// Seed a root-channel message. Returns the message's id.
        pub fn add_root_message(&self, channel_id: u64, msg: DiscordMessage) -> u64 {
            let id = if msg.id == 0 { self.fresh_id() } else { msg.id };
            let stored = DiscordMessage { id, ..msg };
            self.root_messages
                .lock()
                .unwrap()
                .entry(channel_id)
                .or_default()
                .push(stored);
            id
        }

        /// Seed a thread message. Returns the message's id.
        pub fn add_thread_message(&self, thread_id: u64, msg: DiscordMessage) -> u64 {
            let id = if msg.id == 0 { self.fresh_id() } else { msg.id };
            let stored = DiscordMessage { id, ..msg };
            self.thread_messages
                .lock()
                .unwrap()
                .entry(thread_id)
                .or_default()
                .push(stored);
            id
        }
    }

    #[async_trait]
    impl DiscordClient for MockDiscordClient {
        async fn fetch_new_messages(
            &self,
            channel_id: u64,
            since_id: Option<u64>,
        ) -> Result<Vec<DiscordMessage>, HammurabiError> {
            let map = self.root_messages.lock().unwrap();
            let msgs = map
                .get(&channel_id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| since_id.is_none_or(|s| m.id > s))
                .collect();
            Ok(msgs)
        }

        async fn fetch_thread_messages(
            &self,
            thread_id: u64,
            since_id: Option<u64>,
        ) -> Result<Vec<DiscordMessage>, HammurabiError> {
            let map = self.thread_messages.lock().unwrap();
            let msgs = map
                .get(&thread_id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| since_id.is_none_or(|s| m.id > s))
                .collect();
            Ok(msgs)
        }

        async fn post_message(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError> {
            let id = self.fresh_id();
            self.posted_messages
                .lock()
                .unwrap()
                .push((thread_id, body.to_string(), id));
            // Also append to thread_messages so subsequent fetches see the
            // bot's own reply — matches Discord's real behavior.
            self.thread_messages
                .lock()
                .unwrap()
                .entry(thread_id)
                .or_default()
                .push(DiscordMessage {
                    id,
                    channel_id: thread_id,
                    thread_id: Some(thread_id),
                    author_id: 0,
                    author_username: "bot".to_string(),
                    content: body.to_string(),
                    mentions_bot: false,
                });
            Ok(id)
        }

        async fn edit_message(
            &self,
            thread_id: u64,
            message_id: u64,
            body: &str,
        ) -> Result<(), HammurabiError> {
            self.edited_messages
                .lock()
                .unwrap()
                .push((thread_id, message_id, body.to_string()));
            Ok(())
        }

        async fn start_thread(
            &self,
            channel_id: u64,
            message_id: u64,
            name: &str,
        ) -> Result<u64, HammurabiError> {
            let thread_id = self.fresh_id();
            self.created_threads.lock().unwrap().push((
                channel_id,
                message_id,
                name.to_string(),
                thread_id,
            ));
            Ok(thread_id)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn fetch_new_messages_filters_by_since_id() {
            let c = MockDiscordClient::new();
            c.add_root_message(
                100,
                DiscordMessage {
                    id: 1,
                    channel_id: 100,
                    thread_id: None,
                    author_id: 42,
                    author_username: "alice".into(),
                    content: "hello".into(),
                    mentions_bot: true,
                },
            );
            c.add_root_message(
                100,
                DiscordMessage {
                    id: 2,
                    channel_id: 100,
                    thread_id: None,
                    author_id: 42,
                    author_username: "alice".into(),
                    content: "world".into(),
                    mentions_bot: true,
                },
            );

            let all = c.fetch_new_messages(100, None).await.unwrap();
            assert_eq!(all.len(), 2);

            let after1 = c.fetch_new_messages(100, Some(1)).await.unwrap();
            assert_eq!(after1.len(), 1);
            assert_eq!(after1[0].id, 2);
        }

        #[tokio::test]
        async fn post_message_records_and_appears_in_thread_fetch() {
            let c = MockDiscordClient::new();
            let id = c.post_message(200, "draft spec v1").await.unwrap();
            assert!(id > 0);

            let posts = c.posted_messages.lock().unwrap();
            assert_eq!(posts.len(), 1);
            assert_eq!(posts[0].0, 200);
            assert_eq!(posts[0].1, "draft spec v1");
            drop(posts);

            let thread = c.fetch_thread_messages(200, None).await.unwrap();
            assert_eq!(thread.len(), 1);
            assert_eq!(thread[0].content, "draft spec v1");
        }

        #[tokio::test]
        async fn edit_message_records() {
            let c = MockDiscordClient::new();
            let id = c.post_message(300, "v1").await.unwrap();
            c.edit_message(300, id, "v2").await.unwrap();

            let edits = c.edited_messages.lock().unwrap();
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0], (300, id, "v2".to_string()));
        }

        #[tokio::test]
        async fn start_thread_returns_fresh_id() {
            let c = MockDiscordClient::new();
            let t1 = c.start_thread(42, 100, "idea-1").await.unwrap();
            let t2 = c.start_thread(42, 101, "idea-2").await.unwrap();
            assert_ne!(t1, t2);

            let threads = c.created_threads.lock().unwrap();
            assert_eq!(threads.len(), 2);
            assert_eq!(threads[0].0, 42);
            assert_eq!(threads[0].2, "idea-1");
        }
    }
}
