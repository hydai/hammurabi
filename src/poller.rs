use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::access::AllowUsers;
use crate::agents::acp::AcpAgent;
use crate::agents::{AgentKind, AgentRegistry, AiAgent, ClaudeCliAgent};
use crate::approval::{self, CommentApprovalResult, DiscordApprovalResult, PrApprovalResult};
use crate::config::{self, DiscordChannelConfig, GitHubAuth};
use crate::config::{Config, RepoConfig};
use crate::db::Database;
use crate::error::HammurabiError;
use crate::github::{GitHubClient, OctocrabClient};
use crate::lock::LockFile;
use crate::models::{IssueState, SourceKind, TrackedIssue};
use crate::transitions::{self, TransitionContext};
use crate::worktree::{
    AppTokenProvider, GitWorktreeManager, StaticTokenProvider, TokenProvider, WorktreeManager,
};

/// Build the global agent registry from the parsed config. Every supported
/// agent kind is registered; ACP kinds use the `[agents.*]` overrides the
/// user supplied (missing sections fall back to hard-coded defaults).
fn build_agent_registry(config: &Config) -> AgentRegistry {
    let mut map: std::collections::HashMap<AgentKind, Arc<dyn AiAgent>> =
        std::collections::HashMap::new();
    map.insert(AgentKind::ClaudeCli, Arc::new(ClaudeCliAgent::new()));
    for kind in [
        AgentKind::AcpClaude,
        AgentKind::AcpGemini,
        AgentKind::AcpCodex,
    ] {
        let def = config
            .agents
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| crate::agents::acp::default_agent_def(kind));
        map.insert(kind, Arc::new(AcpAgent::new(kind, def)));
    }
    AgentRegistry::new(map)
}

/// Per-repo runtime context (GitHub client + worktree manager + publisher).
struct RepoRuntime {
    github: Arc<dyn GitHubClient>,
    /// Optional Discord client bound to this repo. `Some` when at least
    /// one configured `[[sources]]` entry targets this repo; the poller
    /// uses it both for intake polling and for the `DiscordPublisher`
    /// handed to transitions via `TransitionContext::publisher_for`.
    discord: Option<Arc<dyn crate::discord::DiscordClient>>,
    publisher: Arc<dyn crate::publisher::Publisher>,
    worktree: Arc<dyn WorktreeManager>,
    config: Arc<RepoConfig>,
}

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
        .map_err(HammurabiError::Io)?;
    let db = Arc::new(Database::open(
        db_path.to_str().unwrap_or(".hammurabi/hammurabi.db"),
    )?);

    // Build token provider (shared across all repos)
    let token_provider = build_token_provider(&config.github_auth)?;

    // Initialize agent registry (shared, stateless). ACP invocations pick
    // up their config overrides from `config.agents`.
    let agents = Arc::new(build_agent_registry(&config));

    // Initialize per-repo runtimes
    let repos_dir = base_dir.join("repos");
    let mut cached_runtimes: HashMap<String, RepoRuntime> = HashMap::new();

    for repo_config in &config.repos {
        let runtime = init_repo_runtime(
            repo_config,
            &config.github_auth,
            config.api_retry_count,
            &repos_dir,
            token_provider.clone(),
        )
        .await?;
        cached_runtimes.insert(repo_config.repo.clone(), runtime);
    }

    // Backfill repo column for existing data (single-repo migration)
    if config.repos.len() == 1 {
        let count = db.backfill_repo(&config.repos[0].repo)?;
        if count > 0 {
            tracing::info!(
                repo = %config.repos[0].repo,
                count = count,
                "Backfilled repo column for existing issues"
            );
        }
    } else if config.repos.len() > 1 {
        // Check for unscoped legacy rows that won't be processed
        let unscoped = db.get_all_issues_for_repo("")?;
        if !unscoped.is_empty() {
            tracing::warn!(
                count = unscoped.len(),
                "Database contains {} issues with empty repo column. \
                 These will not be processed until migrated. \
                 Run with a single repo first to backfill, or manually \
                 update the 'repo' column in the database.",
                unscoped.len()
            );
        }
    }

    // Run startup reconciliation for each repo
    for runtime in cached_runtimes.values() {
        let ctx = TransitionContext {
            github: runtime.github.clone(),
            discord: runtime.discord.clone(),
            publisher: runtime.publisher.clone(),
            agents: agents.clone(),
            worktree: runtime.worktree.clone(),
            db: db.clone(),
            config: runtime.config.clone(),
        };
        reconcile(&ctx).await?;
    }

    tracing::info!(
        repos = config.repos.len(),
        "Reconciliation complete, entering poll loop"
    );

    let mut current_config = config;
    let mut last_api_retry_count = current_config.api_retry_count;
    let mut last_auth_fingerprint = auth_fingerprint(&current_config.github_auth);

    // Main poll loop
    loop {
        // Dynamic config reload: re-read config each cycle
        match config::load() {
            Ok(new_config) => {
                current_config = new_config;
            }
            Err(e) => {
                tracing::warn!("Config reload failed, using previous config: {}", e);
            }
        }

        // If global auth or retry settings changed, clear all cached runtimes
        // so they get rebuilt with the new settings.
        let new_fingerprint = auth_fingerprint(&current_config.github_auth);
        if current_config.api_retry_count != last_api_retry_count
            || new_fingerprint != last_auth_fingerprint
        {
            tracing::info!("Global auth or retry config changed, rebuilding all repo runtimes");
            cached_runtimes.clear();
            last_api_retry_count = current_config.api_retry_count;
            last_auth_fingerprint = new_fingerprint;
        }

        // Update cached runtimes: initialize new repos, remove stale ones
        let configured_repos: std::collections::HashSet<String> = current_config
            .repos
            .iter()
            .map(|r| r.repo.clone())
            .collect();

        // Remove runtimes for repos no longer in config
        cached_runtimes.retain(|repo, _| configured_repos.contains(repo));

        // Initialize runtimes for new repos (not yet cached)
        for repo_config in &current_config.repos {
            if !cached_runtimes.contains_key(&repo_config.repo) {
                let tp = match build_token_provider(&current_config.github_auth) {
                    Ok(tp) => tp,
                    Err(e) => {
                        tracing::error!(repo = %repo_config.repo, "Failed to build token provider: {}", e);
                        continue;
                    }
                };
                match init_repo_runtime(
                    repo_config,
                    &current_config.github_auth,
                    current_config.api_retry_count,
                    &repos_dir,
                    tp,
                )
                .await
                {
                    Ok(runtime) => {
                        cached_runtimes.insert(repo_config.repo.clone(), runtime);
                    }
                    Err(e) => {
                        tracing::error!(
                            repo = %repo_config.repo,
                            "Failed to initialize repo runtime: {}",
                            e
                        );
                    }
                }
            }
        }

        // Poll each repo using cached runtimes
        for repo_config in &current_config.repos {
            if let Some(runtime) = cached_runtimes.get(&repo_config.repo) {
                let ctx = TransitionContext {
                    github: runtime.github.clone(),
                    discord: runtime.discord.clone(),
                    publisher: runtime.publisher.clone(),
                    agents: agents.clone(),
                    worktree: runtime.worktree.clone(),
                    db: db.clone(),
                    config: Arc::new(repo_config.clone()),
                };

                if let Err(e) = poll_cycle(&ctx).await {
                    tracing::error!(
                        repo = %repo_config.repo,
                        "Poll cycle error: {}",
                        e
                    );
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(current_config.poll_interval)).await;
    }
}

/// Produce a comparable fingerprint for the auth config so we can detect changes.
/// Uses a hash for token auth to avoid storing the raw credential in memory.
fn auth_fingerprint(auth: &GitHubAuth) -> String {
    match auth {
        GitHubAuth::Token(token) => {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            token.hash(&mut hasher);
            format!("token:{:x}", hasher.finish())
        }
        GitHubAuth::App {
            app_id,
            installation_id,
            ..
        } => {
            format!("app:{}:{}", app_id, installation_id)
        }
    }
}

fn build_token_provider(
    github_auth: &GitHubAuth,
) -> Result<Arc<dyn TokenProvider>, HammurabiError> {
    match github_auth {
        GitHubAuth::Token(token) => Ok(Arc::new(StaticTokenProvider::new(token.clone()))),
        GitHubAuth::App {
            app_id,
            private_key_pem,
            installation_id,
        } => {
            let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem)
                .map_err(|e| HammurabiError::Config(format!("invalid PEM key: {}", e)))?;
            let app_crab = octocrab::Octocrab::builder()
                .app(octocrab::models::AppId(*app_id), key)
                .build()
                .map_err(|e| {
                    HammurabiError::GitHub(format!(
                        "failed to create App client for token provider: {}",
                        e
                    ))
                })?;
            Ok(Arc::new(AppTokenProvider::new(
                app_crab,
                octocrab::models::InstallationId(*installation_id),
            )))
        }
    }
}

async fn init_repo_runtime(
    repo_config: &RepoConfig,
    github_auth: &GitHubAuth,
    api_retry_count: u32,
    repos_dir: &Path,
    token_provider: Arc<dyn TokenProvider>,
) -> Result<RepoRuntime, HammurabiError> {
    let github = Arc::new(OctocrabClient::new(
        github_auth,
        &repo_config.owner,
        &repo_config.repo_name,
        api_retry_count,
    )?);

    // Per-repo worktree base: .hammurabi/repos/<owner>/<repo_name>/
    let repo_base_dir = repos_dir
        .join(&repo_config.owner)
        .join(&repo_config.repo_name);

    let worktree_mgr = Arc::new(GitWorktreeManager::new(
        repo_base_dir.clone(),
        token_provider,
    ));

    // ensure_bare_clone returns early if the bare clone already exists,
    // and ensure_default_branch returns early if the ref already exists,
    // so this is cheap on subsequent calls.
    let repo_url = format!("https://x-access-token@github.com/{}.git", repo_config.repo);
    worktree_mgr.ensure_bare_clone(&repo_url).await?;

    let default_branch = github.get_default_branch().await?;
    worktree_mgr.ensure_default_branch(&default_branch).await?;

    let github_dyn: Arc<dyn GitHubClient> = github.clone();
    let publisher: Arc<dyn crate::publisher::Publisher> =
        Arc::new(crate::publisher::GithubPublisher::new(github_dyn.clone()));

    Ok(RepoRuntime {
        github: github_dyn,
        discord: None,
        publisher,
        worktree: worktree_mgr,
        config: Arc::new(repo_config.clone()),
    })
}

/// Check for issues that were closed externally (outside Hammurabi) and
/// mark them as Done. Skips terminal-state issues. API errors are logged
/// but do not fail the poll cycle.
async fn reconcile_closed_issues(ctx: &TransitionContext) -> Result<(), HammurabiError> {
    let repo = &ctx.config.repo;
    let all_tracked = ctx.db.get_all_issues_for_repo(repo)?;
    for issue in &all_tracked {
        if issue.state == IssueState::Done || issue.state == IssueState::Failed {
            continue;
        }
        // Skip rows that haven't been assigned a GitHub issue yet (e.g.
        // a Discord intake still in SpecDrafting/AwaitSpecApproval).
        if issue.github_issue_number == 0 {
            continue;
        }
        match ctx.github.is_issue_open(issue.github_issue_number).await {
            Ok(false) => {
                ctx.db
                    .update_issue_state(issue.id, IssueState::Done, Some(issue.state))?;
                tracing::info!(
                    repo = %repo,
                    issue = issue.github_issue_number,
                    "Issue closed externally, marking as Done"
                );
            }
            Err(e) => {
                tracing::warn!(
                    repo = %repo,
                    issue = issue.github_issue_number,
                    "Failed to check issue status: {}",
                    e
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Process new @mentions in an allowlisted Discord channel. For each
/// qualifying message we open a thread, insert a `TrackedIssue` with
/// `source=Discord`, stash the pitch as `spec_content` so the drafting
/// prompt can reference it, and immediately kick off the spec-drafting
/// transition.
///
/// `since_id` is advanced across calls so already-processed messages
/// aren't re-opened. The caller owns the cursor (see `DiscordIntake`).
#[allow(dead_code)]
pub(crate) async fn discord_intake_once(
    ctx: &TransitionContext,
    discord_cfg: &DiscordChannelConfig,
    since_id: Option<u64>,
) -> Result<Option<u64>, HammurabiError> {
    let Some(discord) = ctx.discord.clone() else {
        tracing::warn!(
            channel = discord_cfg.channel_id,
            "Skipping Discord intake: ctx.discord is None"
        );
        return Ok(since_id);
    };
    let allow: &AllowUsers = &discord_cfg.allow;
    let messages = discord
        .fetch_new_messages(discord_cfg.channel_id, since_id)
        .await?;

    let mut cursor = since_id;
    for msg in messages {
        cursor = Some(msg.id);
        if !msg.mentions_bot {
            continue;
        }
        if !allow.is_allowed(&msg.author_username) {
            tracing::debug!(
                channel = discord_cfg.channel_id,
                user = %msg.author_username,
                "Dropping message: sender not in allowlist"
            );
            continue;
        }
        let thread_name = truncate_thread_name(&msg.content);
        let thread_id = discord
            .start_thread(discord_cfg.channel_id, msg.id, &thread_name)
            .await?;

        ctx.db
            .insert_discord_thread(&ctx.config.repo, thread_id, &thread_name)?;
        let issue = ctx
            .db
            .get_discord_issue(&ctx.config.repo, thread_id)?
            .ok_or_else(|| HammurabiError::Database("Discord row missing after insert".into()))?;
        ctx.db.update_issue_spec_content(issue.id, &msg.content)?;

        // Re-fetch to pick up the updated spec_content before drafting
        let refreshed = ctx
            .db
            .get_issue_by_id(issue.id)?
            .expect("issue present after insert");
        if let Err(e) = transitions::spec_drafting::execute(ctx, &refreshed, None).await {
            tracing::error!(
                thread_id,
                error = %e,
                "spec_drafting failed for Discord intake"
            );
        }
    }
    Ok(cursor)
}

/// Truncate a pitch to a short, Discord-legal thread name.
fn truncate_thread_name(content: &str) -> String {
    let trimmed = content.trim();
    // Drop leading `@…` mention tokens so the thread title starts on the
    // actual idea.
    let stripped = trimmed
        .split_whitespace()
        .skip_while(|w| w.starts_with('@') || w.starts_with("<@"))
        .collect::<Vec<_>>()
        .join(" ");
    let base = if stripped.is_empty() {
        trimmed.to_string()
    } else {
        stripped
    };
    // Discord thread names cap at 100 chars.
    base.chars().take(90).collect::<String>()
}

async fn poll_cycle(ctx: &TransitionContext) -> Result<(), HammurabiError> {
    let repo = &ctx.config.repo;
    tracing::debug!(repo = %repo, "Starting poll cycle");

    // Fetch origin
    ctx.worktree.fetch_origin().await?;

    reconcile_closed_issues(ctx).await?;

    // Discover new issues
    let labeled_issues = ctx
        .github
        .list_labeled_issues(&ctx.config.tracking_label)
        .await?;

    for gh_issue in &labeled_issues {
        // Skip if ANY source already tracks this issue number — e.g. a
        // Discord `/confirm` flow already created the GitHub issue and
        // the Discord row now carries its number.
        if ctx
            .db
            .get_issue_by_github_number_any_source(repo, gh_issue.number)?
            .is_some()
        {
            continue;
        }
        if ctx.db.get_issue(repo, gh_issue.number)?.is_none() {
            // Verify the tracking label was applied by an authorized approver
            match ctx
                .github
                .get_label_adder(gh_issue.number, &ctx.config.tracking_label)
                .await
            {
                Ok(Some(ref adder)) if ctx.config.approvers.contains(adder) => {
                    ctx.db
                        .insert_issue(repo, gh_issue.number, &gh_issue.title)?;
                    tracing::info!(
                        repo = %repo,
                        issue = gh_issue.number,
                        labeled_by = %adder,
                        "Discovered new issue"
                    );

                    // Check if bypass should be activated
                    if let Some(ref bypass_label) = ctx.config.bypass_label {
                        if gh_issue.labels.contains(bypass_label) {
                            if ctx.config.approvers.contains(&gh_issue.user_login) {
                                if let Some(tracked) =
                                    ctx.db.get_issue(&ctx.config.repo, gh_issue.number)?
                                {
                                    ctx.db.set_issue_bypass(tracked.id, true)?;
                                    tracing::info!(
                                        issue = gh_issue.number,
                                        author = %gh_issue.user_login,
                                        "Bypass mode activated: issue has bypass label and was created by approver"
                                    );
                                }
                            } else {
                                tracing::warn!(
                                    issue = gh_issue.number,
                                    author = %gh_issue.user_login,
                                    "Bypass label present but issue creator is not an approver — bypass ignored"
                                );
                            }
                        }
                    }
                }
                Ok(Some(adder)) => {
                    tracing::warn!(
                        repo = %repo,
                        issue = gh_issue.number,
                        labeled_by = %adder,
                        "Ignoring issue: label applied by unauthorized user"
                    );
                }
                Ok(None) => {
                    tracing::warn!(
                        repo = %repo,
                        issue = gh_issue.number,
                        "Ignoring issue: could not determine who applied the label"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        repo = %repo,
                        issue = gh_issue.number,
                        "Skipping issue: label check failed: {}",
                        e
                    );
                }
            }
        }
    }

    // Process each tracked issue concurrently (bounded by max_concurrent_agents)
    let mut all_tracked = ctx.db.get_all_issues_for_repo(repo)?;
    // Sort by issue number — oldest issues get processed first
    all_tracked.sort_by_key(|i| i.github_issue_number);

    // Filter out terminal states that need no processing
    let actionable: Vec<_> = all_tracked
        .into_iter()
        .filter(|i| i.state != IssueState::Done)
        .collect();

    let semaphore = Arc::new(Semaphore::new(ctx.config.max_concurrent_agents as usize));
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
                    repo = %repo,
                    issue = issue.github_issue_number,
                    "Error processing issue: {}",
                    e
                );
                if issue.state.is_active() {
                    let max_retries = ctx.config.ai_max_retries;
                    if issue.retry_count < max_retries {
                        let new_count = ctx
                            .db
                            .increment_retry_count(issue.id)
                            .unwrap_or(issue.retry_count + 1);
                        tracing::warn!(
                            repo = %repo,
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
                            .publisher
                            .post(
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

    tracing::debug!(repo = %repo, "Poll cycle complete");
    Ok(())
}

async fn process_issue(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let repo = &ctx.config.repo;

    // Blocker check: skip active states if issue has "blocked" label
    if issue.state.is_active() {
        match ctx.github.get_issue(issue.github_issue_number).await {
            Ok(gh_issue) => {
                let blocked = gh_issue.labels.iter().any(|l| {
                    let lower = l.to_lowercase();
                    lower == "blocked" || lower.starts_with("blocked:")
                });
                if blocked {
                    tracing::debug!(
                        repo = %repo,
                        issue = issue.github_issue_number,
                        "Skipping issue: has blocked label"
                    );
                    return Ok(());
                }
            }
            Err(e) => {
                tracing::warn!(
                    repo = %repo,
                    issue = issue.github_issue_number,
                    "Failed to check labels for blocker gating: {}", e
                );
                // Continue processing — don't block on label check failure
            }
        }
    }

    match issue.state {
        IssueState::Discovered | IssueState::SpecDrafting => handle_spec_drafting(ctx, issue).await,
        IssueState::AwaitSpecApproval => handle_await_spec_approval(ctx, issue).await,
        IssueState::Implementing => handle_implementing(ctx, issue).await,
        IssueState::Reviewing => handle_reviewing(ctx, issue).await,
        IssueState::AwaitPRApproval => handle_await_pr_approval(ctx, issue).await,
        IssueState::Failed => handle_failed(ctx, issue).await,
        IssueState::Done => Ok(()),
    }
}

async fn handle_spec_drafting(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    transitions::spec_drafting::execute(ctx, issue, None).await
}

/// Open a GitHub issue on behalf of a Discord-sourced row whose spec has
/// been `/confirm`ed. The spec content (already persisted) becomes the
/// issue body; a footer references the originating Discord thread for
/// provenance. Updates the DB row with the new issue number.
///
/// No-op for GitHub-sourced rows (they already have a number).
pub(crate) async fn ensure_github_issue(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<u64, HammurabiError> {
    if issue.source == SourceKind::GitHub {
        return Ok(issue.github_issue_number);
    }
    if issue.github_issue_number > 0 {
        return Ok(issue.github_issue_number);
    }
    let spec = issue
        .spec_content
        .as_deref()
        .unwrap_or("(spec unavailable)");
    let footer = format!(
        "\n\n---\n*Originated from Discord thread `{}` (source: {}).*",
        issue.external_id, issue.source
    );
    let body = format!("{}{}", spec, footer);
    let labels = vec![ctx.config.tracking_label.clone()];
    let issue_number = ctx
        .github
        .create_issue(&issue.title, &body, &labels)
        .await?;
    ctx.db.set_issue_github_number(issue.id, issue_number)?;
    tracing::info!(
        discord_thread = %issue.external_id,
        github_issue = issue_number,
        "Opened GitHub issue for confirmed Discord spec"
    );
    Ok(issue_number)
}

async fn handle_await_spec_approval(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    if issue.source == SourceKind::Discord {
        return handle_await_spec_approval_discord(ctx, issue).await;
    }
    handle_await_spec_approval_github(ctx, issue).await
}

pub(crate) async fn handle_await_spec_approval_discord(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let thread_id = issue.external_id_u64().ok_or_else(|| {
        HammurabiError::Config(format!(
            "Discord row has non-numeric external_id: {}",
            issue.external_id
        ))
    })?;
    let Some(discord) = ctx.discord.clone() else {
        tracing::warn!(
            thread_id,
            "Skipping Discord approval check: no DiscordClient in ctx"
        );
        return Ok(());
    };

    let outcome = approval::check_discord_approval(
        &*discord,
        thread_id,
        issue.last_comment_id,
        &ctx.config.approvers,
        "/",
    )
    .await?;

    match outcome {
        DiscordApprovalResult::Confirmed { message_id } => {
            ctx.db.update_issue_last_comment(issue.id, message_id)?;
            let gh_number = ensure_github_issue(ctx, issue).await?;
            ctx.db.update_issue_state(
                issue.id,
                IssueState::Implementing,
                Some(IssueState::AwaitSpecApproval),
            )?;
            // After ensure_github_issue the Discord row has a real issue
            // number, so publisher_for now falls through to the GitHub
            // publisher. But the confirm-announcement belongs in the
            // thread, so post via DiscordPublisher explicitly.
            let _ = discord
                .post_message(
                    thread_id,
                    &format!(
                        "Spec approved. GitHub issue #{} opened. Starting implementation...",
                        gh_number
                    ),
                )
                .await;
            let updated = ctx.db.get_issue_by_id(issue.id)?.ok_or_else(|| {
                HammurabiError::Database("issue disappeared after /confirm".into())
            })?;
            transitions::implementing::execute(ctx, &updated, None).await?;
        }
        DiscordApprovalResult::Revised {
            feedback,
            message_id,
        } => {
            ctx.db.update_issue_last_comment(issue.id, message_id)?;
            ctx.db.update_issue_state(
                issue.id,
                IssueState::SpecDrafting,
                Some(IssueState::AwaitSpecApproval),
            )?;
            let _ = discord
                .post_message(thread_id, "Feedback received. Revising spec...")
                .await;
            let updated = ctx.db.get_issue_by_id(issue.id)?.ok_or_else(|| {
                HammurabiError::Database("issue disappeared after /revise".into())
            })?;
            transitions::spec_drafting::execute(ctx, &updated, Some(&feedback)).await?;
        }
        DiscordApprovalResult::Cancelled { message_id } => {
            ctx.db.update_issue_last_comment(issue.id, message_id)?;
            ctx.db.update_issue_state(
                issue.id,
                IssueState::Failed,
                Some(IssueState::AwaitSpecApproval),
            )?;
            ctx.db
                .update_issue_error(issue.id, "Discord intake cancelled by approver")?;
            let _ = discord.post_message(thread_id, "Intake cancelled.").await;
        }
        DiscordApprovalResult::Pending => {
            check_stale(ctx, issue).await?;
        }
    }
    Ok(())
}

async fn handle_await_spec_approval_github(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let repo = &ctx.config.repo;

    // Bypass mode: auto-approve spec without waiting for /approve
    if issue.bypass {
        tracing::info!(
            issue = issue.github_issue_number,
            "Bypass mode: auto-approving spec"
        );
        ctx.db.update_issue_state(
            issue.id,
            IssueState::Implementing,
            Some(IssueState::AwaitSpecApproval),
        )?;
        ctx.publisher
            .post(
                issue.github_issue_number,
                "Spec auto-approved (bypass mode). Starting implementation...",
            )
            .await?;
        let updated = ctx.db.get_issue(repo, issue.github_issue_number)?.unwrap();
        transitions::implementing::execute(ctx, &updated, None).await?;
        return Ok(());
    }

    match approval::check_comment_approval(
        &*ctx.github,
        issue.github_issue_number,
        issue.last_comment_id,
        &ctx.config.approvers,
    )
    .await?
    {
        CommentApprovalResult::Approved { comment_id } => {
            ctx.db.update_issue_last_comment(issue.id, comment_id)?;
            ctx.db.update_issue_state(
                issue.id,
                IssueState::Implementing,
                Some(IssueState::AwaitSpecApproval),
            )?;
            ctx.publisher
                .post(
                    issue.github_issue_number,
                    "Spec approved. Starting implementation...",
                )
                .await?;
            let updated = ctx.db.get_issue(repo, issue.github_issue_number)?.unwrap();
            transitions::implementing::execute(ctx, &updated, None).await?;
        }
        CommentApprovalResult::Feedback { body, comment_id } => {
            ctx.db.update_issue_last_comment(issue.id, comment_id)?;
            ctx.db.update_issue_state(
                issue.id,
                IssueState::SpecDrafting,
                Some(IssueState::AwaitSpecApproval),
            )?;
            ctx.publisher
                .post(
                    issue.github_issue_number,
                    "Feedback received. Revising spec...",
                )
                .await?;
            let updated = ctx.db.get_issue(repo, issue.github_issue_number)?.unwrap();
            transitions::spec_drafting::execute(ctx, &updated, Some(&body)).await?;
        }
        CommentApprovalResult::Pending => {
            check_stale(ctx, issue).await?;
        }
    }
    Ok(())
}

async fn handle_implementing(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let feedback = issue.review_feedback.as_deref();
    transitions::implementing::execute(ctx, issue, feedback).await
}

async fn handle_reviewing(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    transitions::reviewing::execute(ctx, issue).await
}

async fn handle_await_pr_approval(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    let repo = &ctx.config.repo;

    // First check if PR was merged or closed
    transitions::completion::check(ctx, issue).await?;
    let updated = ctx.db.get_issue(repo, issue.github_issue_number)?;
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
                // Persist PR feedback before state transition so it survives crashes.
                // The poller reads review_feedback when entering Implementing state.
                let feedback: String = body.chars().take(2000).collect();
                ctx.db
                    .update_issue_review_feedback(issue.id, Some(&feedback))?;
                ctx.db.update_issue_state(
                    issue.id,
                    IssueState::Implementing,
                    Some(IssueState::AwaitPRApproval),
                )?;
                ctx.publisher
                    .post(
                        issue.github_issue_number,
                        "PR feedback received. Revising implementation...",
                    )
                    .await?;
                let updated = ctx.db.get_issue(repo, issue.github_issue_number)?.unwrap();
                transitions::implementing::execute(
                    ctx,
                    &updated,
                    updated.review_feedback.as_deref(),
                )
                .await?;
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
    Ok(())
}

async fn handle_failed(
    ctx: &TransitionContext,
    issue: &TrackedIssue,
) -> Result<(), HammurabiError> {
    // Check for /retry comment
    if let Some(comment_id) = approval::check_retry_comment(
        &*ctx.github,
        issue.github_issue_number,
        issue.last_comment_id,
        &ctx.config.approvers,
    )
    .await?
    {
        ctx.db.update_issue_last_comment(issue.id, comment_id)?;
        if let Some(prev) = issue.previous_state {
            ctx.db.update_issue_state(issue.id, prev, None)?;
            ctx.db.reset_retry_count(issue.id)?;
            ctx.publisher
                .post(
                    issue.github_issue_number,
                    &format!("Retrying from {} state...", prev),
                )
                .await?;
        }
    }
    Ok(())
}

async fn check_stale(ctx: &TransitionContext, issue: &TrackedIssue) -> Result<(), HammurabiError> {
    // Parse updated_at and check if stale
    if let Ok(updated) =
        chrono::NaiveDateTime::parse_from_str(&issue.updated_at, "%Y-%m-%d %H:%M:%S")
    {
        let now = chrono::Utc::now().naive_utc();
        let days_since = (now - updated).num_days();
        if days_since >= ctx.config.stale_timeout_days as i64 {
            ctx.publisher
                .post(
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
    let repo = &ctx.config.repo;
    tracing::info!(repo = %repo, "Running startup reconciliation");

    let issues = ctx.db.get_all_issues_for_repo(repo)?;
    for issue in &issues {
        match issue.state {
            // Active states: will re-execute on next poll cycle (idempotent)
            IssueState::Discovered
            | IssueState::SpecDrafting
            | IssueState::Implementing
            | IssueState::Reviewing => {
                tracing::info!(
                    repo = %repo,
                    issue = issue.github_issue_number,
                    state = %issue.state,
                    "Active state — will re-execute on next poll"
                );
            }
            // Check for new comments since last processed
            IssueState::AwaitSpecApproval => {
                tracing::info!(
                    repo = %repo,
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
                                repo = %repo,
                                issue = issue.github_issue_number,
                                "Implementation PR merged during downtime"
                            );
                        }
                        Ok(PrApprovalResult::ClosedWithoutMerge) => {
                            tracing::info!(
                                repo = %repo,
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
            if let Ok(false) = ctx.github.is_issue_open(issue.github_issue_number).await {
                ctx.db
                    .update_issue_state(issue.id, IssueState::Done, Some(issue.state))?;
                tracing::info!(
                    repo = %repo,
                    issue = issue.github_issue_number,
                    "Issue closed during downtime, marking as Done"
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::mock::MockAiAgent;
    use crate::db::Database;
    use crate::github::mock::MockGitHubClient;
    use crate::github::GitHubIssue;
    use crate::transitions::test_helpers::{test_config, test_registry_with};
    use crate::transitions::TransitionContext;
    use crate::worktree::mock::MockWorktreeManager;

    fn build_ctx(gh: Arc<MockGitHubClient>, db: Arc<Database>) -> TransitionContext {
        TransitionContext {
            github: gh.clone(),
            discord: None,
            publisher: std::sync::Arc::new(crate::publisher::GithubPublisher::new(gh.clone())),
            agents: test_registry_with(Arc::new(MockAiAgent::new())),
            worktree: Arc::new(MockWorktreeManager::new(
                std::env::temp_dir().join("hammurabi-test-poller"),
            )),
            db,
            config: Arc::new(test_config()),
        }
    }

    #[tokio::test]
    async fn test_reconcile_marks_closed_issue_as_done() {
        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());

        // Insert an issue in Implementing state
        db.insert_issue("owner/repo", 1, "Test issue").unwrap();
        db.update_issue_state(1, IssueState::Implementing, Some(IssueState::Discovered))
            .unwrap();

        // Add it to GitHub as a closed issue
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Test issue".to_string(),
            body: String::new(),
            labels: vec!["hammurabi".to_string()],
            state: "Closed".to_string(),
            user_login: "alice".to_string(),
        });

        let ctx = build_ctx(gh, db.clone());
        reconcile_closed_issues(&ctx).await.unwrap();

        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue.state, IssueState::Done);
    }

    #[tokio::test]
    async fn test_reconcile_skips_terminal_state_issues() {
        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());

        db.insert_issue("owner/repo", 1, "Done issue").unwrap();
        db.update_issue_state(1, IssueState::Done, Some(IssueState::Discovered))
            .unwrap();

        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Done issue".to_string(),
            body: String::new(),
            labels: vec!["hammurabi".to_string()],
            state: "Closed".to_string(),
            user_login: "alice".to_string(),
        });

        let ctx = build_ctx(gh, db.clone());
        reconcile_closed_issues(&ctx).await.unwrap();

        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue.state, IssueState::Done);
    }

    #[tokio::test]
    async fn test_reconcile_leaves_open_issues_unchanged() {
        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());

        db.insert_issue("owner/repo", 1, "Open issue").unwrap();
        db.update_issue_state(1, IssueState::Implementing, Some(IssueState::Discovered))
            .unwrap();

        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Open issue".to_string(),
            body: String::new(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });

        let ctx = build_ctx(gh, db.clone());
        reconcile_closed_issues(&ctx).await.unwrap();

        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue.state, IssueState::Implementing);
    }

    #[tokio::test]
    async fn test_reconcile_multiple_issues_mixed_states() {
        let gh = Arc::new(MockGitHubClient::new());
        let db = Arc::new(Database::open(":memory:").unwrap());

        // Issue 1: active, closed externally
        db.insert_issue("owner/repo", 1, "Closed issue").unwrap();
        db.update_issue_state(1, IssueState::Implementing, Some(IssueState::Discovered))
            .unwrap();
        gh.add_issue(GitHubIssue {
            number: 1,
            title: "Closed issue".to_string(),
            body: String::new(),
            labels: vec!["hammurabi".to_string()],
            state: "Closed".to_string(),
            user_login: "alice".to_string(),
        });

        // Issue 2: active, still open
        db.insert_issue("owner/repo", 2, "Open issue").unwrap();
        db.update_issue_state(2, IssueState::SpecDrafting, Some(IssueState::Discovered))
            .unwrap();
        gh.add_issue(GitHubIssue {
            number: 2,
            title: "Open issue".to_string(),
            body: String::new(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });

        // Issue 3: already Done
        db.insert_issue("owner/repo", 3, "Done issue").unwrap();
        db.update_issue_state(3, IssueState::Done, Some(IssueState::Discovered))
            .unwrap();

        let ctx = build_ctx(gh, db.clone());
        reconcile_closed_issues(&ctx).await.unwrap();

        assert_eq!(
            db.get_issue("owner/repo", 1).unwrap().unwrap().state,
            IssueState::Done
        );
        assert_eq!(
            db.get_issue("owner/repo", 2).unwrap().unwrap().state,
            IssueState::SpecDrafting
        );
        assert_eq!(
            db.get_issue("owner/repo", 3).unwrap().unwrap().state,
            IssueState::Done
        );
    }
}
