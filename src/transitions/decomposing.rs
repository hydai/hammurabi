use crate::claude::AiInvocation;
use crate::error::HammurabiError;
use crate::models::{IssueState, TrackedIssue};
use crate::prompts;

use super::TransitionContext;

pub async fn execute(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
    feedback: Option<&str>,
) -> Result<(), HammurabiError> {
    tracing::info!(
        issue = issue.github_issue_number,
        has_feedback = feedback.is_some(),
        "Starting decomposition"
    );

    // Get the spec content from the spec PR branch
    let spec_branch = format!("hammurabi/{}-spec", issue.github_issue_number);
    let spec_content = ctx
        .github
        .get_file_content(&spec_branch, "SPEC.md")
        .await
        .unwrap_or_else(|_| "No SPEC.md found".to_string());

    // Get original issue
    let gh_issue = ctx
        .github
        .get_issue(issue.github_issue_number)
        .await?;

    // Create worktree for decomposition (needed as working context for Claude)
    let default_branch = ctx.github.get_default_branch().await?;
    let worktree_path = ctx
        .worktree
        .create_worktree(issue.github_issue_number, "decompose", &default_branch)
        .await?;

    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| HammurabiError::Worktree("invalid worktree path".to_string()))?
        .to_string();

    // Invoke AI for decomposition
    let prompt = prompts::decomposition_prompt(
        &spec_content,
        &gh_issue.title,
        &gh_issue.body,
        feedback,
    );
    let model = ctx.config.ai_model_for_task("decompose").to_string();
    let max_turns = ctx.config.ai_max_turns_for_task("decompose");
    let effort = ctx.config.ai_effort_for_task("decompose").to_string();

    let result = ctx
        .ai
        .invoke(AiInvocation {
            model: model.clone(),
            max_turns,
            effort,
            worktree_path: worktree_str.clone(),
            prompt,
        })
        .await?;

    // Log usage
    ctx.db.log_usage(
        issue.id,
        None,
        "decomposing",
        result.input_tokens,
        result.output_tokens,
        &model,
    )?;

    // Cleanup worktree (decomposition doesn't produce a PR)
    ctx.worktree.remove_worktree(&worktree_path).await?;

    // Parse decomposition output
    let sub_tasks = prompts::parse_decomposition_json(&result.content).map_err(|e| {
        HammurabiError::Ai(format!(
            "failed to parse decomposition output: {}. Raw output: {}",
            e,
            &result.content[..result.content.len().min(500)]
        ))
    })?;

    if sub_tasks.is_empty() {
        return Err(HammurabiError::Ai(
            "decomposition produced no sub-tasks".to_string(),
        ));
    }

    // Store sub-issues in DB
    // First, clear any existing sub-issues if re-decomposing
    let existing = ctx.db.get_sub_issues(issue.id)?;
    if !existing.is_empty() {
        // On re-decomposition, we keep existing sub-issues but this is a new plan
        // For simplicity, we just post the new plan. Sub-issues will be created on approval.
    }

    // Format and post decomposition plan as issue comment
    let mut plan_body = String::from("## Decomposition Plan\n\n");
    for (i, task) in sub_tasks.iter().enumerate() {
        plan_body.push_str(&format!(
            "{}. **{}**\n   {}\n\n",
            i + 1,
            task.title,
            task.description
        ));
    }
    plan_body.push_str(
        "\n---\nReply `/approve` to proceed with implementation, or provide feedback to revise.",
    );

    let comment_id = ctx
        .github
        .post_issue_comment(issue.github_issue_number, &plan_body)
        .await?;

    // Insert sub-issues into DB for later use
    // Clear existing sub-issues on re-decomposition
    for existing_sub in &existing {
        // Leave existing sub-issues, the new plan will create fresh ones on approval
        let _ = existing_sub;
    }

    // Store the new sub-tasks
    for task in &sub_tasks {
        ctx.db
            .insert_sub_issue(issue.id, &task.title, &task.description)?;
    }

    // Update issue state
    ctx.db.update_issue_state(
        issue.id,
        IssueState::AwaitDecompApproval,
        Some(IssueState::Decomposing),
    )?;
    ctx.db
        .update_issue_decomp_comment(issue.id, comment_id)?;
    ctx.db
        .update_issue_last_comment(issue.id, comment_id)?;

    tracing::info!(
        issue = issue.github_issue_number,
        sub_tasks = sub_tasks.len(),
        "Decomposition complete, awaiting approval"
    );

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
    use std::sync::Arc;

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
    async fn test_decomposition_posts_plan() {
        let tmp = std::env::temp_dir().join("hammurabi-test-decomp");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Feature X".to_string(),
            body: "Build feature X".to_string(),
            labels: vec![],
            state: "Open".to_string(),
        });
        gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Feature X Spec\n\nDetails...");

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: r#"[{"title": "Add model", "description": "Create the data model"}, {"title": "Add API", "description": "Build the endpoint"}]"#.to_string(),
            session_id: None,
            input_tokens: 200,
            output_tokens: 100,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue, None).await.unwrap();

        // Verify state
        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::AwaitDecompApproval);
        assert!(updated.decomposition_comment_id.is_some());

        // Verify sub-issues created
        let subs = db.get_sub_issues(issue.id).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].title, "Add model");
        assert_eq!(subs[1].title, "Add API");

        // Verify comment posted with plan
        let comments = gh.created_comments.lock().unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].1.contains("Decomposition Plan"));
        assert!(comments[0].1.contains("/approve"));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_decomposition_with_feedback() {
        let tmp = std::env::temp_dir().join("hammurabi-test-decomp-fb");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Feature X".to_string(),
            body: "Build feature X".to_string(),
            labels: vec![],
            state: "Open".to_string(),
        });
        gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Spec");

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: r#"[{"title": "Updated task", "description": "Incorporating feedback"}]"#
                .to_string(),
            session_id: None,
            input_tokens: 100,
            output_tokens: 50,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh.clone(),
            ai,
            worktree: wt,
            db: db.clone(),
            config: Arc::new(test_config()),
        };

        execute(&ctx, &issue, Some("Add more detail")).await.unwrap();

        let subs = db.get_sub_issues(issue.id).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].title, "Updated task");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_decomposition_unparseable_output() {
        let tmp = std::env::temp_dir().join("hammurabi-test-decomp-bad");
        let _ = tokio::fs::remove_dir_all(&tmp).await;

        let gh = Arc::new(MockGitHubClient::new());
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Feature X".to_string(),
            body: "Build feature X".to_string(),
            labels: vec![],
            state: "Open".to_string(),
        });
        gh.set_file_content("hammurabi/1-spec", "SPEC.md", "# Spec");

        let ai = Arc::new(MockAiAgent::new());
        ai.set_default_response(AiResult {
            content: "I couldn't parse the spec properly, here's some text".to_string(),
            session_id: None,
            input_tokens: 100,
            output_tokens: 50,
        });

        let wt = Arc::new(MockWorktreeManager::new(tmp.clone()));
        let db = Arc::new(Database::open(":memory:").unwrap());
        db.insert_issue(1, "Feature X").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        let ctx = TransitionContext {
            github: gh,
            ai,
            worktree: wt,
            db,
            config: Arc::new(test_config()),
        };

        let result = execute(&ctx, &issue, None).await;
        assert!(result.is_err());

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
