//! Integration tests for error handling and retry flows

use std::sync::Arc;

#[path = "../src/agents/mod.rs"]
mod agents;
#[path = "../src/approval.rs"]
mod approval;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/db.rs"]
mod db;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/github.rs"]
mod github;
#[path = "../src/hooks.rs"]
mod hooks;
#[path = "../src/models.rs"]
mod models;
#[path = "../src/prompts.rs"]
mod prompts;
#[path = "../src/state_machine.rs"]
mod state_machine;
#[path = "../src/transitions/mod.rs"]
mod transitions;
#[path = "../src/worktree.rs"]
mod worktree;

use agents::mock::MockAiAgent;
use agents::registry::AgentRegistry;
use agents::{AgentKind, AiResult};
use config::RepoConfig;
use db::Database;
use github::mock::MockGitHubClient;
use github::{GitHubIssue, PrStatus};
use models::IssueState;
use transitions::TransitionContext;
use worktree::mock::MockWorktreeManager;

fn test_config() -> RepoConfig {
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
        approvers: vec!["alice".to_string()],
        bypass_label: None,
        review: None,
        review_max_iterations: 2,
        spec: None,
        implement: None,
    }
}

#[tokio::test]
async fn test_retry_after_spec_failure() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-retry");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    gh.add_issue(GitHubIssue {
        number: 1,
        title: "Feature".to_string(),
        body: "Do it".to_string(),
        labels: vec!["hammurabi".to_string()],
        state: "Open".to_string(),
        user_login: "alice".to_string(),
    });

    // AI that fails first
    let ai = Arc::new(MockAiAgent::new());
    // No response configured -> will fail

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());

    let ctx = TransitionContext {
        github: gh.clone(),
        agents: Arc::new(AgentRegistry::for_test(ai.clone())),
        worktree: wt,
        db: db.clone(),
        config: Arc::new(test_config()),
    };

    db.insert_issue("owner/repo", 1, "Feature").unwrap();
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

    // Spec drafting should fail (no mock response)
    let result = transitions::spec_drafting::execute(&ctx, &issue, None).await;
    assert!(result.is_err());

    // Simulate failure state
    db.update_issue_state(issue.id, IssueState::Failed, Some(IssueState::Discovered))
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Failed);
    assert_eq!(issue.previous_state, Some(IssueState::Discovered));

    // Retry: reset to previous state
    db.update_issue_state(issue.id, IssueState::Discovered, None)
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Discovered);

    // Now configure AI to succeed
    ai.set_default_response(AiResult {
        content: "# SPEC\n\nDone".to_string(),
        session_id: None,
        input_tokens: 100,
        output_tokens: 50,
        agent_kind: AgentKind::ClaudeCli,
        tool_summary: Vec::new(),
    });

    // Should succeed now
    transitions::spec_drafting::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}

#[tokio::test]
async fn test_pr_closed_without_merge() {
    let gh = Arc::new(MockGitHubClient::new());
    gh.set_pr_status(10, PrStatus::ClosedWithoutMerge);

    let result = approval::check_pr_approval(&*gh, 10).await.unwrap();
    assert_eq!(result, approval::PrApprovalResult::ClosedWithoutMerge);
}

#[tokio::test]
async fn test_implementation_failure_and_retry() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-impl-fail");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    gh.add_issue(GitHubIssue {
        number: 1,
        title: "Feature".to_string(),
        body: "Build it".to_string(),
        labels: vec![],
        state: "Open".to_string(),
        user_login: "alice".to_string(),
    });

    // AI that fails
    let ai = Arc::new(MockAiAgent::new());

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());
    db.insert_issue("owner/repo", 1, "Feature").unwrap();
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    db.update_issue_spec_content(issue.id, "# Spec\nBuild feature")
        .unwrap();
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

    let ctx = TransitionContext {
        github: gh.clone(),
        agents: Arc::new(AgentRegistry::for_test(ai.clone())),
        worktree: wt,
        db: db.clone(),
        config: Arc::new(test_config()),
    };

    // Implementation should fail (no mock response)
    let result = transitions::implementing::execute(&ctx, &issue, None).await;
    assert!(result.is_err());

    // Simulate failure state
    db.update_issue_state(issue.id, IssueState::Failed, Some(IssueState::Implementing))
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Failed);
    assert_eq!(issue.previous_state, Some(IssueState::Implementing));

    // Retry: reset to Implementing
    db.update_issue_state(issue.id, IssueState::Implementing, None)
        .unwrap();

    // Now configure AI to succeed
    ai.set_default_response(AiResult {
        content: "Implementation complete".to_string(),
        session_id: None,
        input_tokens: 100,
        output_tokens: 50,
        agent_kind: AgentKind::ClaudeCli,
        tool_summary: Vec::new(),
    });

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    transitions::implementing::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    // First implementation now goes to Reviewing (auto-review gate)
    assert_eq!(issue.state, IssueState::Reviewing);
    // PR is created during reviewing transition, not implementing
    assert!(issue.impl_pr_number.is_none());

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}
