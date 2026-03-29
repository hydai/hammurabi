use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::claude::AiInvocation;
use crate::error::HammurabiError;
use crate::models::{IssueState, SubIssueState, TrackedIssue};
use crate::prompts;

use super::TransitionContext;

pub async fn execute(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    tracing::info!(
        issue = issue.github_issue_number,
        "Starting agent work"
    );

    let sub_issues = ctx.db.get_sub_issues(issue.id)?;
    let pending: Vec<_> = sub_issues
        .iter()
        .filter(|s| s.state == SubIssueState::Pending)
        .collect();

    if pending.is_empty() {
        // All sub-issues already processed. Check results.
        return check_completion(ctx, issue).await;
    }

    // Get spec content for agents
    let spec_branch = format!("hammurabi/{}-spec", issue.github_issue_number);
    let spec_content = ctx
        .github
        .get_file_content(&spec_branch, "SPEC.md")
        .await
        .unwrap_or_else(|_| "No SPEC.md found".to_string());

    let default_branch = ctx.github.get_default_branch().await?;

    let semaphore = Arc::new(Semaphore::new(ctx.config.max_concurrent_agents));
    let mut handles = Vec::new();

    for sub in &pending {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| HammurabiError::Ai(format!("semaphore error: {}", e)))?;

        let sub_id = sub.id;
        let sub_title = sub.title.clone();
        let sub_description = sub.description.clone();
        let issue_number = issue.github_issue_number;
        let issue_id = issue.id;
        let spec_content = spec_content.clone();
        let default_branch = default_branch.clone();
        let ctx_github = ctx.github.clone();
        let ctx_ai = ctx.ai.clone();
        let ctx_wt = ctx.worktree.clone();
        let ctx_db = ctx.db.clone();
        let ctx_config = ctx.config.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            let result = run_single_agent(
                &ctx_github,
                &ctx_ai,
                &ctx_wt,
                &ctx_db,
                &ctx_config,
                issue_number,
                issue_id,
                sub_id,
                &sub_title,
                &sub_description,
                &spec_content,
                &default_branch,
            )
            .await;

            match result {
                Ok(()) => {
                    tracing::info!(
                        issue = issue_number,
                        sub_issue = sub_title,
                        "Agent completed successfully"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        issue = issue_number,
                        sub_issue = sub_title,
                        error = %e,
                        "Agent failed"
                    );
                    let _ = ctx_db.update_sub_issue_state(sub_id, SubIssueState::Failed);
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all agents to complete
    for handle in handles {
        let _ = handle.await;
    }

    // Mark sub-issues as working → update their status based on completion
    check_completion(ctx, issue).await
}

#[allow(clippy::too_many_arguments)]
async fn run_single_agent(
    github: &Arc<dyn crate::github::GitHubClient>,
    ai: &Arc<dyn crate::claude::AiAgent>,
    worktree: &Arc<dyn crate::worktree::WorktreeManager>,
    db: &Arc<crate::db::Database>,
    config: &Arc<crate::config::Config>,
    issue_number: u64,
    issue_id: i64,
    sub_id: i64,
    sub_title: &str,
    sub_description: &str,
    spec_content: &str,
    default_branch: &str,
) -> Result<(), HammurabiError> {
    // Update sub-issue state to working
    db.update_sub_issue_state(sub_id, SubIssueState::Working)?;

    // Create GitHub sub-issue if not already created
    let sub = db
        .get_sub_issues(issue_id)?
        .into_iter()
        .find(|s| s.id == sub_id)
        .ok_or_else(|| HammurabiError::Database("sub-issue not found".to_string()))?;

    let gh_sub_number = if let Some(num) = sub.github_issue_number {
        num
    } else {
        let body = format!(
            "{}\n\n---\n*Sub-issue of #{}*",
            sub_description, issue_number
        );
        let num = github
            .create_issue(
                sub_title,
                &body,
                &[config.tracking_label.clone()],
            )
            .await?;
        db.update_sub_issue_github_number(sub_id, num)?;
        num
    };

    // Create worktree
    let task_name = format!("sub{}", gh_sub_number);
    let worktree_path = worktree
        .create_worktree(issue_number, &task_name, default_branch)
        .await?;

    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| HammurabiError::Worktree("invalid worktree path".to_string()))?
        .to_string();

    db.update_sub_issue_worktree(sub_id, Some(&worktree_str))?;

    // Seed CLAUDE.md
    let claude_md =
        prompts::claude_md_for_implementation(sub_title, sub_description, spec_content);
    worktree
        .seed_file(&worktree_path, "CLAUDE.md", &claude_md)
        .await?;

    // Invoke AI
    let prompt = prompts::implementation_prompt(sub_title, sub_description, spec_content);
    let model = config.ai_model_for_task("implement").to_string();
    let max_turns = config.ai_max_turns_for_task("implement");
    let effort = config.ai_effort_for_task("implement").to_string();

    let result = ai
        .invoke(AiInvocation {
            model: model.clone(),
            max_turns,
            effort,
            worktree_path: worktree_str,
            prompt,
        })
        .await?;

    // Log usage
    db.log_usage(
        issue_id,
        Some(sub_id),
        &format!("implement_{}", sub_title),
        result.input_tokens,
        result.output_tokens,
        &model,
    )?;

    if let Some(session_id) = &result.session_id {
        db.update_sub_issue_session(sub_id, Some(session_id))?;
    }

    // Ensure all changes are committed (AI may or may not have committed)
    worktree
        .commit_all_changes(
            &worktree_path,
            &format!("feat: implement {} for #{}", sub_title, issue_number),
        )
        .await?;

    // Push branch
    let branch_name = format!("hammurabi/{}-{}", issue_number, task_name);
    worktree.push_branch(&branch_name).await?;

    // Create PR
    let pr_title = format!("{} (#{} sub-issue)", sub_title, issue_number);
    let pr_body = format!(
        "Implementation for sub-issue #{}: {}\n\nPart of #{}\n\n---\n*Generated by Hammurabi*",
        gh_sub_number, sub_title, issue_number
    );
    let pr_number = github
        .create_pull_request(&pr_title, &branch_name, default_branch, &pr_body)
        .await?;

    // Update sub-issue
    db.update_sub_issue_state(sub_id, SubIssueState::PrOpen)?;
    db.update_sub_issue_pr(sub_id, pr_number)?;

    Ok(())
}

async fn check_completion(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let sub_issues = ctx.db.get_sub_issues(issue.id)?;

    let all_done = sub_issues
        .iter()
        .all(|s| s.state == SubIssueState::PrOpen || s.state == SubIssueState::Done);
    let any_failed = sub_issues.iter().any(|s| s.state == SubIssueState::Failed);
    let any_pending = sub_issues
        .iter()
        .any(|s| s.state == SubIssueState::Pending || s.state == SubIssueState::Working);

    if any_pending {
        // Still working
        return Ok(());
    }

    if any_failed {
        ctx.db.update_issue_state(
            issue.id,
            IssueState::Failed,
            Some(IssueState::AgentsWorking),
        )?;
        ctx.db
            .update_issue_error(issue.id, "One or more agents failed")?;
        ctx.github
            .post_issue_comment(
                issue.github_issue_number,
                "One or more agents failed. Use `/retry` to re-run failed sub-issues.",
            )
            .await?;
    } else if all_done {
        ctx.db.update_issue_state(
            issue.id,
            IssueState::AwaitSubPRApprovals,
            Some(IssueState::AgentsWorking),
        )?;
        ctx.github
            .post_issue_comment(
                issue.github_issue_number,
                "All agents completed. Sub-issue PRs are open for review.",
            )
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::mock::MockAiAgent;
    use crate::claude::AiResult;
    use crate::config::Config;
    use crate::db::Database;
    use crate::github::mock::MockGitHubClient;
    use crate::github::GitHubIssue;
    use crate::worktree::mock::MockWorktreeManager;

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
    async fn test_agents_working_creates_prs() {
        let tmp = std::env::temp_dir().join("hammurabi-test-agents");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Feature X".to_string(),
            body: "Build it".to_string(),
            labels: vec![],
            state: "Open".to_string(),
        });
        gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Spec");

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "Implementation complete".to_string(),
            session_id: Some("sess-1".to_string()),
            input_tokens: 500,
            output_tokens: 300,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();
        db.insert_sub_issue(issue.id, "Task 1", "Do thing 1")
            .unwrap();
        db.insert_sub_issue(issue.id, "Task 2", "Do thing 2")
            .unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt.clone(),
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // Verify sub-issues got PRs
        let subs = db.get_sub_issues(issue.id).unwrap();
        assert!(subs.iter().all(|s| s.state == SubIssueState::PrOpen));
        assert!(subs.iter().all(|s| s.pr_number.is_some()));

        // Verify PRs created
        let prs = gh.created_prs.lock().unwrap();
        assert_eq!(prs.len(), 2);

        // Verify parent state
        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitSubPRApprovals);

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_agents_partial_failure() {
        let tmp = std::env::temp_dir().join("hammurabi-test-agents-fail");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Feature X".to_string(),
            body: "Build it".to_string(),
            labels: vec![],
            state: "Open".to_string(),
        });
        gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Spec");

        // Agent that fails on "fail" keyword
        let ai = Arc::new(MockAiAgent::new());
        ai.set_response(
            "Task 1",
            AiResult {
                content: "Done".to_string(),
                session_id: None,
                input_tokens: 100,
                output_tokens: 50,
            },
        );
        // No response for Task 2 → will fail

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();
        db.insert_sub_issue(issue.id, "Task 1", "Do thing 1")
            .unwrap();
        db.insert_sub_issue(issue.id, "Task 2", "Do thing 2")
            .unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue).await.unwrap();

        // Parent should be Failed
        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::Failed);

        // One sub-issue should be failed
        let subs = db.get_sub_issues(issue.id).unwrap();
        assert!(subs.iter().any(|s| s.state == SubIssueState::Failed));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
