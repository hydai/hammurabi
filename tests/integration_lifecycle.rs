//! Integration test: full happy-path lifecycle
//! Discovered → SpecDrafting → AwaitSpecApproval → Implementing →
//! AwaitPRApproval → Done

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

use claude::mock::MockAiAgent;
use claude::AiResult;
use config::Config;
use db::Database;
use github::mock::MockGitHubClient;
use github::{GitHubComment, GitHubIssue, PrStatus};
use models::IssueState;
use transitions::TransitionContext;
use worktree::mock::MockWorktreeManager;

fn test_config() -> Config {
    Config {
        repo: "owner/repo".to_string(),
        owner: "owner".to_string(),
        repo_name: "repo".to_string(),
        poll_interval: 60,
        tracking_label: "hammurabi".to_string(),
        stale_timeout_days: 7,
        api_retry_count: 3,
        ai_model: "test-model".to_string(),
        ai_max_turns: 50,
        ai_effort: "high".to_string(),
        ai_timeout_secs: 3600,
        ai_stall_timeout_secs: 300,
        ai_max_retries: 2,
        max_concurrent_agents: 5,
        hooks: crate::config::HooksConfig::default(),
        approvers: vec!["alice".to_string()],
        github_auth: crate::config::GitHubAuth::Token("token".to_string()),
        spec: None,
        implement: None,
    }
}

#[tokio::test]
async fn test_full_lifecycle() {
    let tmp = std::env::temp_dir().join("hammurabi-integ-lifecycle");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    gh.add_issue(GitHubIssue {
        number: 1,
        title: "Add user authentication".to_string(),
        body: "We need login/logout".to_string(),
        labels: vec!["hammurabi".to_string()],
        state: "Open".to_string(),
    });

    let ai = Arc::new(MockAiAgent::new());
    // Spec drafting response
    ai.set_response(
        "producing a SPEC.md",
        AiResult {
            content: "# SPEC\n\nAuthentication feature".to_string(),
            session_id: Some("sess-spec".to_string()),
            input_tokens: 500,
            output_tokens: 300,
        },
    );
    // Implementation response (default fallback)
    ai.set_default_response(AiResult {
        content: "Implementation complete".to_string(),
        session_id: Some("sess-impl".to_string()),
        input_tokens: 1000,
        output_tokens: 500,
    });

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());
    let config = Arc::new(test_config());

    let ctx = TransitionContext {
        github: gh.clone(),
        ai,
        worktree: wt,
        db: db.clone(),
        config,
    };

    // Phase 1: Insert as Discovered
    db.insert_issue(1, "Add user authentication").unwrap();
    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Discovered);

    // Phase 2: Spec drafting — posts spec as comment
    transitions::spec_drafting::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);
    assert!(issue.spec_comment_id.is_some());
    assert!(issue.spec_content.is_some());

    // Phase 3: Approve spec via /approve comment
    gh.add_comment(
        1,
        GitHubComment {
            id: 9999,
            body: "/approve".to_string(),
            user_login: "alice".to_string(),
        },
    );
    let result = approval::check_comment_approval(
        &*ctx.github,
        1,
        issue.last_comment_id,
        &["alice".to_string()],
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        approval::CommentApprovalResult::Approved { .. }
    ));

    // Transition to Implementing
    db.update_issue_state(
        issue.id,
        IssueState::Implementing,
        Some(IssueState::AwaitSpecApproval),
    )
    .unwrap();

    // Phase 4: Implementation — creates single PR
    let issue = db.get_issue(1).unwrap().unwrap();
    transitions::implementing::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitPRApproval);
    assert!(issue.impl_pr_number.is_some());
    let impl_pr = issue.impl_pr_number.unwrap();

    // Phase 5: Merge implementation PR
    gh.set_pr_status(impl_pr, PrStatus::Merged);
    transitions::completion::check(&ctx, &issue).await.unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Done);

    // Verify usage was logged
    let usage = db.get_usage_by_issue(issue.id).unwrap();
    assert!(usage.len() >= 2); // spec + implementation

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}
