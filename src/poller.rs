use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::approval::{self, CommentApprovalResult, PrApprovalResult};
use crate::claude::ClaudeCliAgent;
use crate::config::Config;
use crate::db::Database;
use crate::error::HammurabiError;
use crate::github::{GitHubClient, OctocrabClient};
use crate::lock::LockFile;
use crate::models::{IssueState, TrackedIssue};
use crate::transitions::{self, TransitionContext};
use crate::config::{self, GitHubAuth};
use crate::worktree::{AppTokenProvider, GitWorktreeManager, StaticTokenProvider, TokenProvider, WorktreeManager};

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

    // Initialize GitHub client and token provider based on auth mode
    let github = Arc::new(OctocrabClient::new(
        &config.github_auth,
        &config.owner,
        &config.repo_name,
        config.api_retry_count,
    )?);

    let token_provider: Arc<dyn TokenProvider> = match &config.github_auth {
        GitHubAuth::Token(token) => Arc::new(StaticTokenProvider::new(token.clone())),
        GitHubAuth::App {
            app_id,
            private_key_pem,
            installation_id,
        } => {
            let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem)
                .map_err(|e| HammurabiError::Config(format!("invalid PEM key: {}", e)))?;
            let app_crab = octocrab::Octocrab::builder()
                .app(
                    octocrab::models::AppId(*app_id),
                    key,
                )
                .build()
                .map_err(|e| HammurabiError::GitHub(format!(
                    "failed to create App client for token provider: {}",
                    e
                )))?;
            Arc::new(AppTokenProvider::new(
                app_crab,
                octocrab::models::InstallationId(*installation_id),
            ))
        }
    };

    let worktree_mgr = Arc::new(GitWorktreeManager::new(
        base_dir.clone(),
        token_provider,
    ));

    // Ensure bare clone (token not embedded in URL — uses GIT_ASKPASS instead)
    let repo_url = format!(
        "https://x-access-token@github.com/{}.git",
        config.repo
    );
    worktree_mgr.ensure_bare_clone(&repo_url).await?;
    tracing::info!("Bare clone ready");

    // Ensure the remote default branch exists (empty repos need an initial commit)
    let default_branch = github.get_default_branch().await?;
    worktree_mgr.ensure_default_branch(&default_branch).await?;

    // Initialize AI agent
    let ai: Arc<dyn crate::claude::AiAgent> = Arc::new(ClaudeCliAgent::new());

    let mut current_config = Arc::new(config);

    let ctx = TransitionContext {
        github: github.clone(),
        ai: ai.clone(),
        worktree: worktree_mgr.clone(),
        db: db.clone(),
        config: current_config.clone(),
    };

    // Run startup reconciliation
    reconcile(&ctx).await?;
    tracing::info!(
        max_concurrent = current_config.max_concurrent_agents,
        "Reconciliation complete, entering poll loop"
    );

    // Main poll loop
    loop {
        // Dynamic config reload: re-read config each cycle
        match config::load() {
            Ok(new_config) => {
                current_config = Arc::new(new_config);
            }
            Err(e) => {
                tracing::warn!("Config reload failed, using previous config: {}", e);
            }
        }

        let ctx = TransitionContext {
            github: github.clone(),
            ai: ai.clone(),
            worktree: worktree_mgr.clone(),
            db: db.clone(),
            config: current_config.clone(),
        };

        if let Err(e) = poll_cycle(&ctx).await {
            tracing::error!("Poll cycle error: {}", e);
        }

        tokio::time::sleep(std::time::Duration::from_secs(current_config.poll_interval)).await;
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
            // Verify the tracking label was applied by an authorized approver
            match ctx
                .github
                .get_label_adder(gh_issue.number, &ctx.config.tracking_label)
                .await
            {
                Ok(Some(ref adder)) if ctx.config.approvers.contains(adder) => {
                    ctx.db.insert_issue(gh_issue.number, &gh_issue.title)?;
                    tracing::info!(
                        issue = gh_issue.number,
                        labeled_by = %adder,
                        "Discovered new issue"
                    );
                }
                Ok(Some(adder)) => {
                    tracing::warn!(
                        issue = gh_issue.number,
                        labeled_by = %adder,
                        "Ignoring issue: label applied by unauthorized user"
                    );
                }
                Ok(None) => {
                    tracing::warn!(
                        issue = gh_issue.number,
                        "Ignoring issue: could not determine who applied the label"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        issue = gh_issue.number,
                        "Skipping issue: label check failed: {}",
                        e
                    );
                }
            }
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

    // Process each tracked issue concurrently (bounded by max_concurrent_agents)
    let mut all_tracked = ctx.db.get_all_issues()?;
    // Sort by issue number — oldest issues get processed first
    all_tracked.sort_by_key(|i| i.github_issue_number);

    // Filter out terminal states that need no processing
    let actionable: Vec<_> = all_tracked
        .into_iter()
        .filter(|i| i.state != IssueState::Done)
        .collect();

    let semaphore = Arc::new(Semaphore::new(
        ctx.config.max_concurrent_agents as usize,
    ));
    let mut join_set = JoinSet::new();

    for issue in actionable {
        let sem = semaphore.clone();
        let ctx = ctx.clone();
        join_set.spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let result = process_issue(&ctx, &issue).await;
            (issue, result)
        });
    }

    // Collect results and handle errors
    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok((issue, Ok(()))) => {
                if issue.retry_count > 0 {
                    let _ = ctx.db.reset_retry_count(issue.id);
                }
            }
            Ok((issue, Err(e))) => {
                tracing::error!(
                    issue = issue.github_issue_number,
                    "Error processing issue: {}",
                    e
                );
                if issue.state.is_active() {
                    let max_retries = ctx.config.ai_max_retries;
                    if issue.retry_count < max_retries {
                        let new_count = ctx.db.increment_retry_count(issue.id)
                            .unwrap_or(issue.retry_count + 1);
                        tracing::warn!(
                            issue = issue.github_issue_number,
                            retry_count = new_count,
                            max_retries = max_retries,
                            "Will retry on next poll cycle (attempt {}/{})",
                            new_count, max_retries
                        );
                        let _ = ctx.db.update_issue_error(issue.id, &e.to_string());
                    } else {
                        let _ = ctx.db.update_issue_state(
                            issue.id,
                            IssueState::Failed,
                            Some(issue.state),
                        );
                        let _ = ctx.db.update_issue_error(issue.id, &e.to_string());
                        let _ = ctx
                            .github
                            .post_issue_comment(
                                issue.github_issue_number,
                                &format!(
                                    "Error during {} (after {} retries): {}",
                                    issue.state, max_retries, e
                                ),
                            )
                            .await;
                    }
                }
            }
            Err(join_err) => {
                tracing::error!("Task panicked: {}", join_err);
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
            transitions::spec_drafting::execute(ctx, issue, None).await?;
        }
        IssueState::SpecDrafting => {
            // Re-execute on restart (idempotent)
            transitions::spec_drafting::execute(ctx, issue, None).await?;
        }
        IssueState::AwaitSpecApproval => {
            match approval::check_comment_approval(
                &*ctx.github,
                issue.github_issue_number,
                issue.last_comment_id,
                &ctx.config.approvers,
            )
            .await?
            {
                CommentApprovalResult::Approved { comment_id } => {
                    ctx.db
                        .update_issue_last_comment(issue.id, comment_id)?;
                    ctx.db.update_issue_state(
                        issue.id,
                        IssueState::Implementing,
                        Some(IssueState::AwaitSpecApproval),
                    )?;
                    ctx.github
                        .post_issue_comment(
                            issue.github_issue_number,
                            "Spec approved. Starting implementation...",
                        )
                        .await?;
                    let updated = ctx.db.get_issue(issue.github_issue_number)?.unwrap();
                    transitions::implementing::execute(ctx, &updated, None).await?;
                }
                CommentApprovalResult::Feedback { body, comment_id } => {
                    ctx.db
                        .update_issue_last_comment(issue.id, comment_id)?;
                    ctx.db.update_issue_state(
                        issue.id,
                        IssueState::SpecDrafting,
                        Some(IssueState::AwaitSpecApproval),
                    )?;
                    ctx.github
                        .post_issue_comment(
                            issue.github_issue_number,
                            "Feedback received. Revising spec...",
                        )
                        .await?;
                    let updated = ctx.db.get_issue(issue.github_issue_number)?.unwrap();
                    transitions::spec_drafting::execute(ctx, &updated, Some(&body)).await?;
                }
                CommentApprovalResult::Pending => {
                    check_stale(ctx, issue).await?;
                }
            }
        }
        IssueState::Implementing => {
            transitions::implementing::execute(ctx, issue, None).await?;
        }
        IssueState::AwaitPRApproval => {
            // First check if PR was merged or closed
            transitions::completion::check(ctx, issue).await?;
            let updated = ctx.db.get_issue(issue.github_issue_number)?;
            let issue = match &updated {
                Some(u) if u.state == IssueState::AwaitPRApproval => u,
                _ => return Ok(()), // State changed (merged/failed), done
            };

            // PR still open — check for reviewer feedback on the PR
            if let Some(pr_number) = issue.impl_pr_number {
                match approval::check_comment_approval(
                    &*ctx.github,
                    pr_number,
                    issue.last_pr_comment_id,
                    &ctx.config.approvers,
                )
                .await?
                {
                    CommentApprovalResult::Feedback { body, comment_id } => {
                        ctx.db.update_issue_last_pr_comment(issue.id, comment_id)?;
                        ctx.db.update_issue_state(
                            issue.id,
                            IssueState::Implementing,
                            Some(IssueState::AwaitPRApproval),
                        )?;
                        ctx.github
                            .post_issue_comment(
                                issue.github_issue_number,
                                "PR feedback received. Revising implementation...",
                            )
                            .await?;
                        let updated = ctx.db.get_issue(issue.github_issue_number)?.unwrap();
                        transitions::implementing::execute(ctx, &updated, Some(&body)).await?;
                    }
                    CommentApprovalResult::Approved { comment_id } => {
                        // /approve on a PR is not meaningful — merge is the real approval.
                        // Just update the cursor so we don't re-process this comment.
                        ctx.db.update_issue_last_pr_comment(issue.id, comment_id)?;
                    }
                    CommentApprovalResult::Pending => {
                        check_stale(ctx, issue).await?;
                    }
                }
            } else {
                check_stale(ctx, issue).await?;
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
                    ctx.db
                        .update_issue_state(issue.id, prev, None)?;
                    ctx.db.reset_retry_count(issue.id)?;
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
            | IssueState::Implementing => {
                tracing::info!(
                    issue = issue.github_issue_number,
                    state = %issue.state,
                    "Active state — will re-execute on next poll"
                );
            }
            // Check for new comments since last processed
            IssueState::AwaitSpecApproval => {
                tracing::info!(
                    issue = issue.github_issue_number,
                    "Checking for spec approval comments during downtime"
                );
            }
            // Check if implementation PR was merged while stopped
            IssueState::AwaitPRApproval => {
                if let Some(pr_number) = issue.impl_pr_number {
                    match approval::check_pr_approval(&*ctx.github, pr_number).await {
                        Ok(PrApprovalResult::Merged) => {
                            tracing::info!(
                                issue = issue.github_issue_number,
                                "Implementation PR merged during downtime"
                            );
                        }
                        Ok(PrApprovalResult::ClosedWithoutMerge) => {
                            tracing::info!(
                                issue = issue.github_issue_number,
                                "Implementation PR closed during downtime"
                            );
                        }
                        _ => {}
                    }
                }
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
