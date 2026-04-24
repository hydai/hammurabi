mod acp;
mod agents;
mod approval;
mod config;
mod db;
mod error;
mod github;
mod hooks;
mod lock;
mod models;
mod poller;
mod prompts;
mod publisher;
mod state_machine;
mod transitions;
mod worktree;

use clap::{Parser, Subcommand};

use crate::db::Database;
use crate::models::IssueState;

#[derive(Parser)]
#[command(name = "hammurabi", about = "AI-powered GitHub issue lifecycle daemon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon, monitoring configured repositories
    Watch {
        /// GitHub repository in owner/repo format (overrides config; for single-repo mode)
        repo: Option<String>,
    },
    /// Display all tracked issues with current state and last activity
    Status {
        /// Filter by repository (owner/repo format)
        #[arg(long)]
        repo: Option<String>,
    },
    /// Reset a failed issue to its previous active state
    Retry {
        /// GitHub issue number
        issue_number: u64,
        /// Repository (owner/repo) — required if issue number is ambiguous
        #[arg(long)]
        repo: Option<String>,
    },
    /// Reset an issue to Discovered state
    Reset {
        /// GitHub issue number
        issue_number: u64,
        /// Repository (owner/repo) — required if issue number is ambiguous
        #[arg(long)]
        repo: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Watch { repo } => {
            // Set HAMMURABI_REPO before config::load() so the loader can use
            // it as the legacy repo when no repo/[[repos]] is in the config file.
            if let Some(ref r) = repo {
                std::env::set_var("HAMMURABI_REPO", r);
            }

            let mut config = config::load()?;

            // If CLI repo was given and config has [[repos]], override the list
            if let Some(ref r) = repo {
                tracing::info!("Starting daemon for {}", r);
                let base = config.repos.first();
                let repo_config = config::RepoConfig::from_cli_override(r, base)?;
                config.repos = vec![repo_config];
            } else {
                tracing::info!("Starting daemon (repos from config)");
            }

            tracing::info!(
                repos = config
                    .repos
                    .iter()
                    .map(|r| r.repo.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                "Monitoring repositories"
            );

            poller::run_daemon(config).await?;
        }
        Commands::Status { repo } => {
            let db = open_db()?;
            let mut issues = if let Some(ref r) = repo {
                db.get_all_issues_for_repo(r)?
            } else {
                db.get_all_issues()?
            };
            issues.sort_by(|a, b| {
                a.state
                    .sort_priority()
                    .cmp(&b.state.sort_priority())
                    .then(a.repo.cmp(&b.repo))
                    .then(a.github_issue_number.cmp(&b.github_issue_number))
            });

            if issues.is_empty() {
                println!("No tracked issues.");
                return Ok(());
            }

            println!(
                "{:<25} {:<8} {:<42} {:<22} {:<12} Last Activity",
                "Repo", "Issue #", "Title", "State", "Age"
            );
            println!("{}", "-".repeat(125));

            let now = chrono::Utc::now().naive_utc();
            for issue in &issues {
                let title = if issue.title.chars().count() > 40 {
                    let truncated: String = issue.title.chars().take(37).collect();
                    format!("{}...", truncated)
                } else {
                    issue.title.clone()
                };

                let repo_display = if issue.repo.chars().count() > 23 {
                    let truncated: String = issue.repo.chars().take(20).collect();
                    format!("{}...", truncated)
                } else {
                    issue.repo.clone()
                };

                let age = if let Ok(created) =
                    chrono::NaiveDateTime::parse_from_str(&issue.created_at, "%Y-%m-%d %H:%M:%S")
                {
                    format_duration(now - created)
                } else {
                    "?".to_string()
                };

                let last_activity = if let Ok(updated) =
                    chrono::NaiveDateTime::parse_from_str(&issue.updated_at, "%Y-%m-%d %H:%M:%S")
                {
                    format_duration(now - updated)
                } else {
                    "?".to_string()
                };

                println!(
                    "{:<25} {:<8} {:<42} {:<22} {:<12} {}",
                    repo_display,
                    format!("#{}", issue.github_issue_number),
                    title,
                    issue.state,
                    age,
                    last_activity
                );
            }
        }
        Commands::Retry { issue_number, repo } => {
            let db = open_db()?;
            let issue = resolve_issue(&db, issue_number, repo.as_deref())?;

            if issue.state != IssueState::Failed {
                anyhow::bail!(
                    "issue #{} ({}) is in {} state, not Failed. Only Failed issues can be retried.",
                    issue_number,
                    issue.repo,
                    issue.state
                );
            }

            let prev = issue.previous_state.ok_or_else(|| {
                anyhow::anyhow!(
                    "issue #{} has no previous state to retry from",
                    issue_number
                )
            })?;

            db.update_issue_state(issue.id, prev, None)?;
            println!(
                "Issue #{} ({}) reset from Failed to {}. Will be processed on next poll cycle.",
                issue_number, issue.repo, prev
            );
        }
        Commands::Reset { issue_number, repo } => {
            let db = open_db()?;
            let issue = resolve_issue(&db, issue_number, repo.as_deref())?;

            db.update_issue_state(issue.id, IssueState::Discovered, None)?;
            println!(
                "Issue #{} ({}) reset to Discovered. Will be processed on next poll cycle.",
                issue_number, issue.repo
            );
        }
    }

    Ok(())
}

/// Resolve an issue by number, optionally filtered by repo.
/// If no repo is specified and the issue number is ambiguous, returns an error.
fn resolve_issue(
    db: &Database,
    issue_number: u64,
    repo: Option<&str>,
) -> Result<crate::models::TrackedIssue, anyhow::Error> {
    if let Some(r) = repo {
        db.get_issue(r, issue_number)?
            .ok_or_else(|| anyhow::anyhow!("issue #{} not tracked in repo {}", issue_number, r))
    } else {
        let issues = db.get_issue_any_repo(issue_number)?;
        match issues.len() {
            0 => Err(anyhow::anyhow!("issue #{} not tracked", issue_number)),
            1 => Ok(issues.into_iter().next().unwrap()),
            n => {
                let repos: Vec<_> = issues.iter().map(|i| i.repo.as_str()).collect();
                Err(anyhow::anyhow!(
                    "issue #{} exists in {} repos: {}. Use --repo to disambiguate.",
                    issue_number,
                    n,
                    repos.join(", ")
                ))
            }
        }
    }
}

fn open_db() -> Result<Database, anyhow::Error> {
    let db_path = ".hammurabi/hammurabi.db";
    if !std::path::Path::new(db_path).exists() {
        anyhow::bail!(
            "No database found at {}. Run `hammurabi watch` first to initialize.",
            db_path
        );
    }
    Ok(Database::open(db_path)?)
}

fn format_duration(d: chrono::Duration) -> String {
    let days = d.num_days();
    if days > 0 {
        return format!("{}d ago", days);
    }
    let hours = d.num_hours();
    if hours > 0 {
        return format!("{}h ago", hours);
    }
    let minutes = d.num_minutes();
    format!("{}m ago", minutes)
}
