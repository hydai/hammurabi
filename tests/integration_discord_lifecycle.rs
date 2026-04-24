//! Integration test: full Discord-originated lifecycle.
//!
//! User @mentions bot -> thread created -> SpecDrafting -> AwaitSpecApproval
//! -> /revise (re-draft) -> /confirm (opens GitHub issue) -> Implementing
//! -> Reviewing -> AwaitPRApproval -> merged -> Done.

use std::sync::Arc;

#[path = "../src/access.rs"]
mod access;
#[path = "../src/acp/mod.rs"]
mod acp;
#[path = "../src/agents/mod.rs"]
mod agents;
#[path = "../src/approval.rs"]
mod approval;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/db.rs"]
mod db;
#[path = "../src/discord.rs"]
mod discord;
#[path = "../src/env_expand.rs"]
mod env_expand;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/github.rs"]
mod github;
#[path = "../src/hooks.rs"]
mod hooks;
#[path = "../src/lock.rs"]
mod lock;
#[path = "../src/models.rs"]
mod models;
#[path = "../src/poller.rs"]
mod poller;
#[path = "../src/prompts.rs"]
mod prompts;
#[path = "../src/publisher.rs"]
mod publisher;
#[path = "../src/state_machine.rs"]
mod state_machine;
#[path = "../src/transitions/mod.rs"]
mod transitions;
#[path = "../src/worktree.rs"]
mod worktree;

use access::AllowUsers;
use agents::mock::MockAiAgent;
use agents::registry::AgentRegistry;
use agents::{AgentKind, AiResult};
use config::{DiscordChannelConfig, RepoConfig};
use db::Database;
use discord::mock::MockDiscordClient;
use discord::DiscordMessage;
use github::mock::MockGitHubClient;
use github::PrStatus;
use models::{IssueState, SourceKind};
use transitions::TransitionContext;
use worktree::mock::MockWorktreeManager;

fn test_repo_config() -> RepoConfig {
    RepoConfig {
        repo: "owner/repo".to_string(),
        owner: "owner".to_string(),
        repo_name: "repo".to_string(),
        tracking_label: "hammurabi".to_string(),
        stale_timeout_days: 7,
        ai_model: "test-model".to_string(),
        ai_max_turns: 50,
        ai_effort: "high".to_string(),
        ai_timeout_secs: 3600,
        ai_stall_timeout_secs: 0,
        ai_max_retries: 2,
        max_concurrent_agents: 5,
        hooks: crate::config::HooksConfig::default(),
        approvers: vec!["hydai".to_string()],
        bypass_label: None,
        review: None,
        review_max_iterations: 2,
        spec: None,
        implement: None,
        agent_kind: AgentKind::ClaudeCli,
    }
}

fn test_discord_config(channel_id: u64) -> DiscordChannelConfig {
    DiscordChannelConfig {
        name: "intake".into(),
        channel_id,
        repo: "owner/repo".into(),
        bot_token: "fake-token".into(),
        approvers: vec!["hydai".into()],
        agent_kind: None,
        command_prefix: "/".into(),
        max_draft_revisions: 5,
        allow: AllowUsers::List(vec!["hydai".into()]),
    }
}

#[tokio::test]
async fn discord_end_to_end_lifecycle() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-discord");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    // --- Arrange ---
    let gh = Arc::new(MockGitHubClient::new());
    let dc = Arc::new(MockDiscordClient::new());
    let channel_id = 123_456_u64;

    // Seed an incoming @mention with the idea.
    let pitch_id = dc.add_root_message(
        channel_id,
        DiscordMessage {
            id: 0,
            channel_id,
            thread_id: None,
            author_id: 42,
            author_username: "hydai".into(),
            content: "@Hammurabi add dark mode toggle to the settings page".into(),
            mentions_bot: true,
        },
    );

    // AI responds to every prompt with a canned spec/implementation.
    let ai = Arc::new(MockAiAgent::new());
    ai.set_default_response(AiResult {
        content: "# SPEC\n\nAdd dark mode toggle".into(),
        session_id: Some("sess".into()),
        input_tokens: 100,
        output_tokens: 50,
        agent_kind: AgentKind::ClaudeCli,
        tool_summary: Vec::new(),
    });

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());

    let ctx = TransitionContext {
        github: gh.clone(),
        discord: Some(dc.clone()),
        publisher: Arc::new(publisher::GithubPublisher::new(gh.clone())),
        agents: Arc::new(AgentRegistry::for_test(ai.clone())),
        worktree: wt.clone(),
        db: db.clone(),
        config: Arc::new(test_repo_config()),
    };
    let discord_cfg = test_discord_config(channel_id);

    // --- Phase 1: Intake ---
    let cursor = poller::discord_intake_once(&ctx, &discord_cfg, None)
        .await
        .unwrap();
    assert!(cursor.is_some());

    // Exactly one Discord row should exist, in AwaitSpecApproval.
    let issues = db.get_all_issues_for_repo("owner/repo").unwrap();
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.source, SourceKind::Discord);
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);
    assert_eq!(issue.github_issue_number, 0);
    assert!(issue.external_id_u64().is_some());
    let thread_id = issue.external_id_u64().unwrap();

    // Bot should have posted the spec draft into the thread.
    let posts = dc.posted_messages.lock().unwrap();
    assert_eq!(
        posts.len(),
        1,
        "expected exactly one thread post (the draft)"
    );
    assert_eq!(posts[0].0, thread_id);
    assert!(posts[0].1.contains("Spec"));
    assert!(posts[0].1.contains("/confirm"));
    drop(posts);

    // --- Phase 2: /revise iterates the spec ---
    dc.add_thread_message(
        thread_id,
        DiscordMessage {
            id: 0,
            channel_id: thread_id,
            thread_id: Some(thread_id),
            author_id: 42,
            author_username: "hydai".into(),
            content: "/revise also respect prefers-color-scheme".into(),
            mentions_bot: false,
        },
    );

    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    poller::handle_await_spec_approval_discord(&ctx, &issue)
        .await
        .unwrap();

    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    // After /revise the state transitions back through SpecDrafting and
    // spec_drafting::execute runs a re-draft, landing back on AwaitSpecApproval.
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);
    assert_eq!(issue.github_issue_number, 0);

    // --- Phase 3: /confirm opens a GitHub issue and starts implementation ---
    dc.add_thread_message(
        thread_id,
        DiscordMessage {
            id: 0,
            channel_id: thread_id,
            thread_id: Some(thread_id),
            author_id: 42,
            author_username: "hydai".into(),
            content: "/confirm".into(),
            mentions_bot: false,
        },
    );

    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    poller::handle_await_spec_approval_discord(&ctx, &issue)
        .await
        .unwrap();

    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    assert!(
        issue.github_issue_number > 0,
        "GitHub issue should be opened"
    );
    // After /confirm the flow continues through Implementing -> Reviewing.
    assert_eq!(issue.state, IssueState::Reviewing);

    // Verify the GitHub issue body includes the Discord origin footer.
    let created = gh.created_issues.lock().unwrap();
    assert_eq!(created.len(), 1);
    assert!(created[0].1.contains("Discord thread"));
    drop(created);

    // --- Phase 4: Reviewing -> AwaitPRApproval ---
    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    transitions::reviewing::execute(&ctx, &issue).await.unwrap();

    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitPRApproval);
    let pr_number = issue.impl_pr_number.expect("PR should be opened");

    // --- Phase 5: PR merged -> Done ---
    gh.set_pr_status(pr_number, PrStatus::Merged);
    transitions::completion::check(&ctx, &issue).await.unwrap();

    let issue = db.get_issue_by_id(issue.id).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Done);

    // Original source identity preserved through the full lifecycle.
    assert_eq!(issue.source, SourceKind::Discord);
    assert!(issue.github_issue_number > 0);

    let _ = tokio::fs::remove_dir_all(&tmp).await;
    let _ = pitch_id;
}

#[tokio::test]
async fn non_allowlisted_user_intake_is_dropped() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-discord-drop");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    let dc = Arc::new(MockDiscordClient::new());
    let channel_id = 999_u64;

    dc.add_root_message(
        channel_id,
        DiscordMessage {
            id: 0,
            channel_id,
            thread_id: None,
            author_id: 999,
            author_username: "eve".into(),
            content: "@Hammurabi do something sneaky".into(),
            mentions_bot: true,
        },
    );

    let ai = Arc::new(MockAiAgent::new());
    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());

    let ctx = TransitionContext {
        github: gh.clone(),
        discord: Some(dc.clone()),
        publisher: Arc::new(publisher::GithubPublisher::new(gh.clone())),
        agents: Arc::new(AgentRegistry::for_test(ai)),
        worktree: wt,
        db: db.clone(),
        config: Arc::new(test_repo_config()),
    };
    let discord_cfg = test_discord_config(channel_id);

    poller::discord_intake_once(&ctx, &discord_cfg, None)
        .await
        .unwrap();

    // eve is not in allow_users → no thread, no row.
    assert!(dc.created_threads.lock().unwrap().is_empty());
    assert!(db.get_all_issues_for_repo("owner/repo").unwrap().is_empty());

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}
