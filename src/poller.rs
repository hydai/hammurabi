use std::path::PathBuf;
use std::sync::Arc;

use crate::approval::{self, DecompApprovalResult, SpecApprovalResult};
use crate::claude::ClaudeCliAgent;
use crate::config::Config;
use crate::db::Database;
use crate::error::HammurabiError;
use crate::github::{GitHubClient, OctocrabClient};
use crate::lock::LockFile;
use crate::models::{IssueState, TrackedIssue};
use crate::transitions::{self, TransitionContext};
use crate::worktree::{GitWorktreeManager, WorktreeManager};

pub async fn run_daemon(config: Config) -> Result<(), HammurabiError> {
    let base_dir = PathBuf::from(".hammurabi");

    // Acquire lock
    let lock_path = base_dir.join("daemon.lock");
    let _lock = LockFile::acquire(&lock_path)?;
    tracing::info!("Lock acquired");

    // Initialize database
    let db_path = base_dir.join("hammurabi.db");
    tokio::fs::create_dir_all(&base_dir)
        .await
        .map_err(|e| HammurabiError::Io(e))?;
    let db = Arc::new(Database::open(
        db_path.to_str().unwrap_or(".hammurabi/hammurabi.db"),
    )?);

    // Initialize GitHub client
    let github = Arc::new(OctocrabClient::new(
        &config.github_token,
        &config.owner,
        &config.repo_name,
        config.api_retry_count,
    )?);

    // Initialize worktree manager
    let worktree_mgr = Arc::new(GitWorktreeManager::new(base_dir.clone()));

    // Ensure bare clone
    let repo_url = format!(
        "https://x-access-token:{}@github.com/{}.git",
        config.github_token, config.repo
    );
    worktree_mgr.ensure_bare_clone(&repo_url).await?;
    tracing::info!("Bare clone ready");

    // Initialize AI agent
    let ai = Arc::new(ClaudeCliAgent::new());

    let config = Arc::new(config);

    let ctx = TransitionContext {
        github: github.clone(),
        ai,
        worktree: worktree_mgr.clone(),
        db: db.clone(),
        config: config.clone(),
    };

    // Run startup reconciliation
    reconcile(&ctx).await?;
    tracing::info!("Reconciliation complete");

    // Main poll loop
    loop {
        if let Err(e) = poll_cycle(&ctx).await {
            tracing::error!("Poll cycle error: {}", e);
        }

        tokio::time::sleep(std::time::Duration::from_secs(config.poll_interval)).await;
    }
}

async fn poll_cycle(ctx: &TransitionContext) -> Result<(), HammurabiError> {
    tracing::debug!("Starting poll cycle");

    // Fetch origin
    ctx.worktree.fetch_origin().await?;

    // Discover new issues
    let labeled_issues = ctx
        .github
        .list_labeled_issues(&ctx.config.tracking_label)
        .await?;

    for gh_issue in &labeled_issues {
        if ctx.db.get_issue(gh_issue.number)?.is_none() {
            ctx.db.insert_issue(gh_issue.number, &gh_issue.title)?;
            tracing::info!(issue = gh_issue.number, "Discovered new issue");
        }
    }

    // Check for externally closed issues
    let all_tracked = ctx.db.get_all_issues()?;
    for issue in &all_tracked {
        if issue.state == IssueState::Done || issue.state == IssueState::Failed {
            continue;
        }
        match ctx.github.is_issue_open(issue.github_issue_number).await {
            Ok(false) => {
                ctx.db
                    .update_issue_state(issue.id, IssueState::Done, Some(issue.state))?;
                tracing::info!(
                    issue = issue.github_issue_number,
                    "Issue closed externally, marking as Done"
                );
            }
            Err(e) => {
                tracing::warn!(
                    issue = issue.github_issue_number,
                    "Failed to check issue status: {}",
                    e
                );
            }
            _ => {}
        }
    }

    // Process each tracked issue
    let all_tracked = ctx.db.get_all_issues()?;
    for issue in &all_tracked {
        if let Err(e) = process_issue(ctx, issue).await {
            tracing::error!(
                issue = issue.github_issue_number,
                "Error processing issue: {}",
                e
            );
            // Transition to Failed on error for active states
            if issue.state.is_active() {
                let _ = ctx
                    .db
                    .update_issue_state(issue.id, IssueState::Failed, Some(issue.state));
                let _ = ctx
                    .db
                    .update_issue_error(issue.id, &e.to_string());
                let _ = ctx
                    .github
                    .post_issue_comment(
                        issue.github_issue_number,
                        &format!("Error during {}: {}", issue.state, e),
                    )
                    .await;
            }
        }
    }

    tracing::debug!("Poll cycle complete");
    Ok(())
}

async fn process_issue(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    match issue.state {
        IssueState::Discovered => {
            transitions::spec_drafting::execute(ctx, issue).await?;
        }
        IssueState::SpecDrafting => {
            // Re-execute on restart (idempotent)
            transitions::spec_drafting::execute(ctx, issue).await?;
        }
        IssueState::AwaitSpecApproval => {
            if let Some(pr_number) = issue.spec_pr_number {
                match approval::check_spec_approval(&*ctx.github, pr_number).await? {
                    SpecApprovalResult::Approved => {
                        ctx.db.update_issue_state(
                            issue.id,
                            IssueState::Decomposing,
                            Some(IssueState::AwaitSpecApproval),
                        )?;
                        ctx.github
                            .post_issue_comment(
                                issue.github_issue_number,
                                "Spec PR merged. Starting decomposition...",
                            )
                            .await?;
                        // Execute decomposition immediately
                        let updated = ctx.db.get_issue(issue.github_issue_number)?.unwrap();
                        transitions::decomposing::execute(ctx, &updated, None).await?;
                    }
                    SpecApprovalResult::Rejected => {
                        ctx.db.update_issue_state(
                            issue.id,
                            IssueState::Failed,
                            Some(IssueState::AwaitSpecApproval),
                        )?;
                        ctx.db
                            .update_issue_error(issue.id, "Spec PR closed without merge")?;
                        ctx.github
                            .post_issue_comment(
                                issue.github_issue_number,
                                "Spec PR was closed without merge. Use `/retry` to regenerate.",
                            )
                            .await?;
                    }
                    SpecApprovalResult::Pending => {
                        check_stale(ctx, issue).await?;
                    }
                }
            }
        }
        IssueState::Decomposing => {
            transitions::decomposing::execute(ctx, issue, None).await?;
        }
        IssueState::AwaitDecompApproval => {
            match approval::check_decomp_approval(
                &*ctx.github,
                issue.github_issue_number,
                issue.last_comment_id,
                &ctx.config.approvers,
            )
            .await?
            {
                DecompApprovalResult::Approved { comment_id } => {
                    ctx.db
                        .update_issue_last_comment(issue.id, comment_id)?;
                    ctx.db.update_issue_state(
                        issue.id,
                        IssueState::AgentsWorking,
                        Some(IssueState::AwaitDecompApproval),
                    )?;
                    ctx.github
                        .post_issue_comment(
                            issue.github_issue_number,
                            "Decomposition approved. Spawning agents...",
                        )
                        .await?;
                    let updated = ctx.db.get_issue(issue.github_issue_number)?.unwrap();
                    transitions::agents_working::execute(ctx, &updated).await?;
                }
                DecompApprovalResult::Feedback { body, comment_id } => {
                    ctx.db
                        .update_issue_last_comment(issue.id, comment_id)?;
                    ctx.db.update_issue_state(
                        issue.id,
                        IssueState::Decomposing,
                        Some(IssueState::AwaitDecompApproval),
                    )?;
                    ctx.github
                        .post_issue_comment(
                            issue.github_issue_number,
                            "Feedback received. Re-running decomposition...",
                        )
                        .await?;
                    let updated = ctx.db.get_issue(issue.github_issue_number)?.unwrap();
                    transitions::decomposing::execute(ctx, &updated, Some(&body)).await?;
                }
                DecompApprovalResult::Pending => {
                    check_stale(ctx, issue).await?;
                }
            }
        }
        IssueState::AgentsWorking => {
            transitions::agents_working::execute(ctx, issue).await?;
        }
        IssueState::AwaitSubPRApprovals => {
            transitions::completion::check(ctx, issue).await?;
            // Also check stale
            let updated = ctx.db.get_issue(issue.github_issue_number)?;
            if let Some(u) = &updated {
                if u.state == IssueState::AwaitSubPRApprovals {
                    check_stale(ctx, u).await?;
                }
            }
        }
        IssueState::Failed => {
            // Check for /retry comment
            if let Some(comment_id) = approval::check_retry_comment(
                &*ctx.github,
                issue.github_issue_number,
                issue.last_comment_id,
                &ctx.config.approvers,
            )
            .await?
            {
                ctx.db
                    .update_issue_last_comment(issue.id, comment_id)?;
                if let Some(prev) = issue.previous_state {
                    // Reset failed sub-issues if retrying from AgentsWorking
                    if prev == IssueState::AgentsWorking {
                        ctx.db.reset_failed_sub_issues(issue.id)?;
                    }
                    ctx.db
                        .update_issue_state(issue.id, prev, None)?;
                    ctx.github
                        .post_issue_comment(
                            issue.github_issue_number,
                            &format!("Retrying from {} state...", prev),
                        )
                        .await?;
                }
            }
        }
        IssueState::Done => {
            // Nothing to do
        }
    }

    Ok(())
}

async fn check_stale(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    // Parse updated_at and check if stale
    if let Ok(updated) = chrono::NaiveDateTime::parse_from_str(&issue.updated_at, "%Y-%m-%d %H:%M:%S") {
        let now = chrono::Utc::now().naive_utc();
        let days_since = (now - updated).num_days();
        if days_since >= ctx.config.stale_timeout_days as i64 {
            ctx.github
                .post_issue_comment(
                    issue.github_issue_number,
                    &format!(
                        "This issue has been in {} state for {} days. Please review and take action.",
                        issue.state, days_since
                    ),
                )
                .await?;
        }
    }
    Ok(())
}

async fn reconcile(ctx: &TransitionContext) -> Result<(), HammurabiError> {
    tracing::info!("Running startup reconciliation");

    let issues = ctx.db.get_all_issues()?;
    for issue in &issues {
        match issue.state {
            // Active states: will re-execute on next poll cycle (idempotent)
            IssueState::Discovered
            | IssueState::SpecDrafting
            | IssueState::Decomposing
            | IssueState::AgentsWorking => {
                tracing::info!(
                    issue = issue.github_issue_number,
                    state = %issue.state,
                    "Active state — will re-execute on next poll"
                );
            }
            // Check if spec PR was merged while stopped
            IssueState::AwaitSpecApproval => {
                if let Some(pr_number) = issue.spec_pr_number {
                    match approval::check_spec_approval(&*ctx.github, pr_number).await {
                        Ok(SpecApprovalResult::Approved) => {
                            tracing::info!(
                                issue = issue.github_issue_number,
                                "Spec PR merged during downtime"
                            );
                        }
                        Ok(SpecApprovalResult::Rejected) => {
                            tracing::info!(
                                issue = issue.github_issue_number,
                                "Spec PR closed during downtime"
                            );
                        }
                        _ => {}
                    }
                }
            }
            // Check for new comments
            IssueState::AwaitDecompApproval => {
                tracing::info!(
                    issue = issue.github_issue_number,
                    "Checking for comments during downtime"
                );
            }
            // Check sub-PR statuses
            IssueState::AwaitSubPRApprovals => {
                tracing::info!(
                    issue = issue.github_issue_number,
                    "Checking sub-PR statuses during downtime"
                );
            }
            // Terminal states: no action
            IssueState::Failed | IssueState::Done => {}
        }

        // Check if issue was closed externally
        if !issue.state.is_terminal() {
            match ctx.github.is_issue_open(issue.github_issue_number).await {
                Ok(false) => {
                    ctx.db
                        .update_issue_state(issue.id, IssueState::Done, Some(issue.state))?;
                    tracing::info!(
                        issue = issue.github_issue_number,
                        "Issue closed during downtime, marking as Done"
                    );
                }
                _ => {}
            }
        }
    }

    Ok(())
}
