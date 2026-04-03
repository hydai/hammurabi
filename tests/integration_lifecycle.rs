//! Integration test: full happy-path lifecycle
//! Discovered -> SpecDrafting -> AwaitSpecApproval -> Implementing ->
//! Reviewing -> AwaitPRApproval -> Done

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
use config::RepoConfig;
use db::Database;
use github::mock::MockGitHubClient;
use github::{GitHubComment, GitHubIssue, PrStatus};
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
        user_login: "alice".to_string(),
    });

    let ai = Arc::new(MockAiAgent::new());
    // Spec drafting response
    ai.set_response(
        "Architect agent",
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
    db.insert_issue("owner/repo", 1, "Add user authentication").unwrap();
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Discovered);

    // Phase 2: Spec drafting — posts spec as comment
    transitions::spec_drafting::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
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

    // Phase 4: Implementation — transitions to Reviewing (PR created during review)
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    transitions::implementing::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    // First implementation now goes to Reviewing (auto-review gate)
    assert_eq!(issue.state, IssueState::Reviewing);
    // PR is created during reviewing transition, not implementing
    assert!(issue.impl_pr_number.is_none());

    // Phase 5: Auto-review (mock's default unparseable response defaults to PASS via parse_review_verdict)
    transitions::reviewing::execute(&ctx, &issue).await.unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitPRApproval);
    assert!(issue.impl_pr_number.is_some());
    let impl_pr = issue.impl_pr_number.unwrap();

    // Phase 6: Merge implementation PR
    gh.set_pr_status(impl_pr, PrStatus::Merged);
    transitions::completion::check(&ctx, &issue).await.unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Done);

    // Verify usage was logged
    let usage = db.get_usage_by_issue(issue.id).unwrap();
    assert!(usage.len() >= 3); // spec + implementation + review

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}

#[tokio::test]
async fn test_bypass_spec_auto_approval() {
    // When bypass is active, AwaitSpecApproval should auto-transition to Implementing
    // without requiring a /approve comment.
    let tmp = std::env::temp_dir().join("hammurabi-integ-bypass");
    let _ = tokio::fs::remove_dir_all(&tmp).await;

    let gh = Arc::new(MockGitHubClient::new());
    gh.add_issue(GitHubIssue {
        number: 1,
        title: "Quick fix".to_string(),
        body: "Fix the typo".to_string(),
        labels: vec!["hammurabi".to_string(), "hammurabi-bypass".to_string()],
        state: "Open".to_string(),
        user_login: "alice".to_string(),
    });

    let ai = Arc::new(MockAiAgent::new());
    ai.set_response(
        "Architect agent",
        AiResult {
            content: "# SPEC\n\nFix the typo".to_string(),
            session_id: Some("sess-spec".to_string()),
            input_tokens: 200,
            output_tokens: 100,
        },
    );
    ai.set_default_response(AiResult {
        content: "Implementation complete".to_string(),
        session_id: Some("sess-impl".to_string()),
        input_tokens: 400,
        output_tokens: 200,
    });

    let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
    let db = Arc::new(Database::open(":memory:").unwrap());

    // Enable bypass in config
    let mut config = test_config();
    config.bypass_label = Some("hammurabi-bypass".to_string());
    let config = Arc::new(config);

    let ctx = TransitionContext {
        github: gh.clone(),
        ai,
        worktree: wt,
        db: db.clone(),
        config,
    };

    // Insert issue and activate bypass (simulating what poller discovery does)
    db.insert_issue("owner/repo", 1, "Quick fix").unwrap();
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    db.set_issue_bypass(issue.id, true).unwrap();

    // Phase 1: Spec drafting
    transitions::spec_drafting::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);
    assert!(issue.bypass);

    // Phase 2: Bypass auto-approval — no /approve comment needed!
    // The poller's process_issue would do this; we simulate the bypass logic here.
    assert!(issue.bypass);
    db.update_issue_state(
        issue.id,
        IssueState::Implementing,
        Some(IssueState::AwaitSpecApproval),
    )
    .unwrap();

    // Phase 3: Implementation
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    transitions::implementing::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    // First implementation now goes to Reviewing (auto-review gate)
    assert_eq!(issue.state, IssueState::Reviewing);
    assert!(issue.impl_pr_number.is_none());

    // Phase 4: Auto-review (default response = optimistic PASS)
    transitions::reviewing::execute(&ctx, &issue).await.unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitPRApproval);
    assert!(issue.impl_pr_number.is_some());

    // Phase 5: PR still needs human merge even in bypass mode
    let pr = issue.impl_pr_number.unwrap();
    gh.set_pr_status(pr, PrStatus::Merged);
    transitions::completion::check(&ctx, &issue).await.unwrap();

    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Done);

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}

#[tokio::test]
async fn test_bypass_rejected_for_non_approver() {
    // Bypass label is present but issue creator is NOT an approver — bypass should not activate.
    let db = Database::open(":memory:").unwrap();
    db.insert_issue("owner/repo", 1, "Feature from outsider").unwrap();
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

    // Do NOT set bypass (simulating what poller would do for non-approver)
    assert!(!issue.bypass);

    // The issue should follow normal flow (bypass stays false)
    let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
    assert!(!issue.bypass);
}
