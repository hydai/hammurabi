//! Integration tests for error handling and retry flows

use std::sync::Arc;

#[path = "../src/approval.rs"]
mod approval;
#[path = "../src/claude.rs"]
mod claude;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/db.rs"]
mod db;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/github.rs"]
mod github;
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

use claude::mock::MockAiAgent;
use claude::AiResult;
use config::Config;
use db::Database;
use github::mock::MockGitHubClient;
use github::{GitHubIssue, PrStatus};
use models::{IssueState, SubIssueState};
use transitions::TransitionContext;
use worktree::mock::MockWorktreeManager;

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
        ai_effort: "high".to_string(),
        approvers: vec!["alice".to_string()],
        github_auth: crate::config::GitHubAuth::Token("token".to_string()),
        spec: None,
        decompose: None,
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
    });

    // AI that fails first
    let ai = Arc::new(MockAiAgent::new());
    // No response configured → will fail

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());

    let ctx = TransitionContext {
        github: gh.clone(),
        ai: ai.clone(),
        worktree: wt,
        db: db.clone(),
        config: Arc::new(test_config()),
    };

    db.insert_issue(1, "Feature").unwrap();
    let issue = db.get_issue(1).unwrap().unwrap();

    // Spec drafting should fail (no mock response)
    let result = transitions::spec_drafting::execute(&ctx, &issue).await;
    assert!(result.is_err());

    // Simulate failure state
    db.update_issue_state(issue.id, IssueState::Failed, Some(IssueState::Discovered))
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Failed);
    assert_eq!(issue.previous_state, Some(IssueState::Discovered));

    // Retry: reset to previous state
    db.update_issue_state(issue.id, IssueState::Discovered, None)
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Discovered);

    // Now configure AI to succeed
    ai.set_default_response(AiResult {
        content: "# SPEC\n\nDone".to_string(),
        session_id: None,
        input_tokens: 100,
        output_tokens: 50,
    });

    // Should succeed now
    transitions::spec_drafting::execute(&ctx, &issue)
        .await
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}

#[tokio::test]
async fn test_pr_closed_without_merge() {
    let gh = Arc::new(MockGitHubClient::new());
    gh.set_pr_status(10, PrStatus::ClosedWithoutMerge);

    let result = approval::check_spec_approval(&*gh, 10).await.unwrap();
    assert_eq!(result, approval::SpecApprovalResult::Rejected);
}

#[tokio::test]
async fn test_partial_agent_failure_and_retry() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-partial");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    gh.add_issue(GitHubIssue {
        number: 1,
        title: "Feature".to_string(),
        body: "Build it".to_string(),
        labels: vec![],
        state: "Open".to_string(),
    });
    gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Spec");

    let ai = Arc::new(MockAiAgent::new());
    // Only Task 1 succeeds
    ai.set_response(
        "Task 1",
        AiResult {
            content: "Done".to_string(),
            session_id: None,
            input_tokens: 100,
            output_tokens: 50,
        },
    );

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());
    db.insert_issue(1, "Feature").unwrap();
    let issue = db.get_issue(1).unwrap().unwrap();
    db.insert_sub_issue(issue.id, "Task 1", "Succeeds").unwrap();
    db.insert_sub_issue(issue.id, "Task 2", "Will fail").unwrap();

    let ctx = TransitionContext {
        github: gh,
        ai,
        worktree: wt,
        db: db.clone(),
        config: Arc::new(test_config()),
    };

    transitions::agents_working::execute(&ctx, &issue)
        .await
        .unwrap();

    // Parent should be Failed
    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Failed);

    // Check sub-issue states
    let subs = db.get_sub_issues(issue.id).unwrap();
    let succeeded = subs.iter().filter(|s| s.state == SubIssueState::PrOpen).count();
    let failed = subs.iter().filter(|s| s.state == SubIssueState::Failed).count();
    assert_eq!(succeeded, 1);
    assert_eq!(failed, 1);

    // Retry resets only failed sub-issues
    db.reset_failed_sub_issues(issue.id).unwrap();
    let subs = db.get_sub_issues(issue.id).unwrap();
    let pending = subs.iter().filter(|s| s.state == SubIssueState::Pending).count();
    assert_eq!(pending, 1); // Only the failed one reset to pending
    let still_open = subs.iter().filter(|s| s.state == SubIssueState::PrOpen).count();
    assert_eq!(still_open, 1); // Successful one unchanged

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}

#[tokio::test]
async fn test_unparseable_decomposition() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-unparse");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    gh.add_issue(GitHubIssue {
        number: 1,
        title: "Feature".to_string(),
        body: "Build it".to_string(),
        labels: vec![],
        state: "Open".to_string(),
    });
    gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Spec");

    let ai = Arc::new(MockAiAgent::new());
    ai.set_default_response(AiResult {
        content: "I don't understand the spec. Please clarify.".to_string(),
        session_id: None,
        input_tokens: 100,
        output_tokens: 50,
    });

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());
    db.insert_issue(1, "Feature").unwrap();
    let issue = db.get_issue(1).unwrap().unwrap();

    let ctx = TransitionContext {
        github: gh,
        ai,
        worktree: wt,
        db,
        config: Arc::new(test_config()),
    };

    let result = transitions::decomposing::execute(&ctx, &issue, None).await;
    assert!(result.is_err());

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}
