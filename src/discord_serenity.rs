//! Serenity-backed `DiscordClient` — the production runtime.
//!
//! Feature-gated behind `discord`. Builds on top of `serenity::http::Http`
//! (the REST client) for all five `DiscordClient` operations:
//! `fetch_new_messages` / `fetch_thread_messages` poll the channel via
//! `GET /channels/{id}/messages`, `post_message` and `edit_message` map
//! to the obvious REST verbs, and `start_thread` calls
//! `POST /channels/{id}/messages/{mid}/threads`.
//!
//! Polling is correct (if noisy) for a daemon that already runs on a
//! tick — the real-time Gateway listener is a future upgrade (add the
//! `client` + `gateway` serenity features and wire an `EventHandler`
//! into `run_daemon`). The trait stays unchanged for that upgrade.

#![cfg(feature = "discord")]

use std::sync::Arc;

use async_trait::async_trait;
use serenity::builder::{CreateMessage, CreateThread, EditMessage, GetMessages};
use serenity::http::Http;
use serenity::model::channel::AutoArchiveDuration;
use serenity::model::id::{ChannelId, MessageId, UserId};

use crate::discord::{DiscordClient, DiscordMessage};
use crate::error::HammurabiError;

/// Discord HTTP client scoped to one bot identity. `bot_user_id` is
/// captured at construction so the intake path can check whether a
/// message mentions *this* bot without a round-trip.
pub struct SerenityDiscordClient {
    http: Arc<Http>,
    bot_user_id: u64,
}

impl SerenityDiscordClient {
    /// Construct with a pre-resolved bot user id. Primarily useful for
    /// tests or callers that already learned the id elsewhere. The
    /// production `run_daemon` path uses [`Self::connect`] instead.
    #[allow(dead_code)]
    pub fn new(token: impl AsRef<str>, bot_user_id: u64) -> Self {
        Self {
            http: Arc::new(Http::new(token.as_ref())),
            bot_user_id,
        }
    }

    /// Build a client and resolve the bot's own user id in one go by
    /// calling `GET /users/@me`. Needed when the caller doesn't already
    /// know the bot id (the common deployment case).
    pub async fn connect(token: impl AsRef<str>) -> Result<Self, HammurabiError> {
        let http = Arc::new(Http::new(token.as_ref()));
        let me = http
            .get_current_user()
            .await
            .map_err(|e| HammurabiError::Discord(format!("get_current_user failed: {}", e)))?;
        Ok(Self {
            http,
            bot_user_id: me.id.get(),
        })
    }
}

fn err(prefix: &str, e: serenity::Error) -> HammurabiError {
    HammurabiError::Discord(format!("{}: {}", prefix, e))
}

fn mentions_user(content: &str, user_id: u64) -> bool {
    // Discord renders mentions as `<@ID>` or `<@!ID>` depending on nickname form.
    let forms = [format!("<@{}>", user_id), format!("<@!{}>", user_id)];
    forms.iter().any(|f| content.contains(f.as_str()))
}

#[async_trait]
impl DiscordClient for SerenityDiscordClient {
    async fn fetch_new_messages(
        &self,
        channel_id: u64,
        since_id: Option<u64>,
    ) -> Result<Vec<DiscordMessage>, HammurabiError> {
        let channel = ChannelId::new(channel_id);
        let mut builder = GetMessages::new().limit(50);
        if let Some(id) = since_id {
            builder = builder.after(MessageId::new(id));
        }
        let msgs = channel
            .messages(&*self.http, builder)
            .await
            .map_err(|e| err("fetch_new_messages", e))?;

        let bot_id = self.bot_user_id;
        // `messages` returns newest-first; reverse so callers see chronological order
        // (matching how the `MockDiscordClient` fixtures are seeded).
        let mut out: Vec<DiscordMessage> = msgs
            .into_iter()
            .map(|m| {
                let mentions_us = mentions_user(&m.content, bot_id)
                    || m.mentions.iter().any(|u| u.id.get() == bot_id);
                DiscordMessage {
                    id: m.id.get(),
                    channel_id: m.channel_id.get(),
                    thread_id: None,
                    author_id: m.author.id.get(),
                    author_username: m.author.name,
                    content: m.content,
                    mentions_bot: mentions_us,
                }
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    async fn fetch_thread_messages(
        &self,
        thread_id: u64,
        since_id: Option<u64>,
    ) -> Result<Vec<DiscordMessage>, HammurabiError> {
        // Discord threads are channels; the same GET messages endpoint works.
        let channel = ChannelId::new(thread_id);
        let mut builder = GetMessages::new().limit(100);
        if let Some(id) = since_id {
            builder = builder.after(MessageId::new(id));
        }
        let msgs = channel
            .messages(&*self.http, builder)
            .await
            .map_err(|e| err("fetch_thread_messages", e))?;

        let mut out: Vec<DiscordMessage> = msgs
            .into_iter()
            .map(|m| DiscordMessage {
                id: m.id.get(),
                channel_id: m.channel_id.get(),
                thread_id: Some(thread_id),
                author_id: m.author.id.get(),
                author_username: m.author.name,
                content: m.content,
                // mentions_bot is only meaningful for root-channel discovery.
                mentions_bot: false,
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    async fn post_message(&self, thread_id: u64, body: &str) -> Result<u64, HammurabiError> {
        let channel = ChannelId::new(thread_id);
        let msg = channel
            .send_message(&*self.http, CreateMessage::new().content(body))
            .await
            .map_err(|e| err("post_message", e))?;
        Ok(msg.id.get())
    }

    async fn edit_message(
        &self,
        thread_id: u64,
        message_id: u64,
        body: &str,
    ) -> Result<(), HammurabiError> {
        let channel = ChannelId::new(thread_id);
        channel
            .edit_message(
                &*self.http,
                MessageId::new(message_id),
                EditMessage::new().content(body),
            )
            .await
            .map_err(|e| err("edit_message", e))?;
        Ok(())
    }

    async fn start_thread(
        &self,
        channel_id: u64,
        message_id: u64,
        name: &str,
    ) -> Result<u64, HammurabiError> {
        let channel = ChannelId::new(channel_id);
        let thread = channel
            .create_thread_from_message(
                &*self.http,
                MessageId::new(message_id),
                CreateThread::new(name).auto_archive_duration(AutoArchiveDuration::OneWeek),
            )
            .await
            .map_err(|e| err("start_thread", e))?;
        Ok(thread.id.get())
    }
}

// Silence a warning about unused UserId import when the file is
// partially referenced by future Gateway integration.
#[allow(dead_code)]
fn _touch_userid(id: UserId) -> u64 {
    id.get()
}
