//! Integration test: full happy-path lifecycle
//! Discovered → SpecDrafting → AwaitSpecApproval → Decomposing →
//! AwaitDecompApproval → AgentsWorking → AwaitSubPRApprovals → Done

use std::sync::Arc;

// Re-use the library's types by importing from the binary crate
// Since this is a binary crate, we need to build tests as part of it.
// Instead, we replicate the key flows using the modules directly.

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
use github::{GitHubComment, GitHubIssue, PrStatus};
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
    // Decomposition response (checked first since decomp prompts also contain "spec")
    ai.set_response(
        "independently-implementable sub-tasks",
        AiResult {
            content: r#"[{"title": "Add user model", "description": "Create User struct"}, {"title": "Add login endpoint", "description": "POST /login"}]"#.to_string(),
            session_id: None,
            input_tokens: 200,
            output_tokens: 100,
        },
    );
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

    // Phase 2: Spec drafting
    gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# SPEC\n\nAuth");
    transitions::spec_drafting::execute(&ctx, &issue).await.unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitSpecApproval);
    assert!(issue.spec_pr_number.is_some());
    let spec_pr = issue.spec_pr_number.unwrap();

    // Phase 3: Merge spec PR
    gh.set_pr_status(spec_pr, PrStatus::Merged);
    let result = approval::check_spec_approval(&*ctx.github, spec_pr)
        .await
        .unwrap();
    assert_eq!(result, approval::SpecApprovalResult::Approved);

    // Transition to Decomposing
    db.update_issue_state(issue.id, IssueState::Decomposing, Some(IssueState::AwaitSpecApproval))
        .unwrap();

    // Phase 4: Decomposition
    let issue = db.get_issue(1).unwrap().unwrap();
    transitions::decomposing::execute(&ctx, &issue, None)
        .await
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitDecompApproval);
    let subs = db.get_sub_issues(issue.id).unwrap();
    assert_eq!(subs.len(), 2);

    // Phase 5: Approve decomposition
    // Use an ID higher than the mock's next_comment_id (starts at 1000 + comments already posted)
    gh.add_comment(
        1,
        GitHubComment {
            id: 9999,
            body: "/approve".to_string(),
            user_login: "alice".to_string(),
        },
    );
    let result = approval::check_decomp_approval(
        &*ctx.github,
        1,
        issue.last_comment_id,
        &["alice".to_string()],
    )
    .await
    .unwrap();
    assert!(matches!(result, approval::DecompApprovalResult::Approved { .. }));

    // Transition to AgentsWorking
    db.update_issue_state(
        issue.id,
        IssueState::AgentsWorking,
        Some(IssueState::AwaitDecompApproval),
    )
    .unwrap();

    // Phase 6: Run agents
    let issue = db.get_issue(1).unwrap().unwrap();
    transitions::agents_working::execute(&ctx, &issue)
        .await
        .unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::AwaitSubPRApprovals);

    let subs = db.get_sub_issues(issue.id).unwrap();
    assert!(subs.iter().all(|s| s.state == SubIssueState::PrOpen));
    assert!(subs.iter().all(|s| s.pr_number.is_some()));

    // Phase 7: Merge all sub-PRs
    for sub in &subs {
        gh.set_pr_status(sub.pr_number.unwrap(), PrStatus::Merged);
    }

    transitions::completion::check(&ctx, &issue).await.unwrap();

    let issue = db.get_issue(1).unwrap().unwrap();
    assert_eq!(issue.state, IssueState::Done);

    // Verify usage was logged
    let usage = db.get_usage_by_issue(issue.id).unwrap();
    assert!(usage.len() >= 3); // spec + decompose + 2 implementations

    let _ = tokio::fs::remove_dir_all(&tmp).await;
}
