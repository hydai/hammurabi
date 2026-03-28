mod approval;
mod claude;
mod config;
mod db;
mod error;
mod github;
mod lock;
mod models;
mod poller;
mod prompts;
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
    /// Start the daemon, monitoring the specified repository
    Watch {
        /// GitHub repository in owner/repo format
        repo: String,
    },
    /// Display all tracked issues with current state and last activity
    Status,
    /// Reset a failed issue to its previous active state
    Retry {
        /// GitHub issue number
        issue_number: u64,
    },
    /// Reset an issue to Discovered state
    Reset {
        /// GitHub issue number
        issue_number: u64,
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
            tracing::info!("Starting daemon for {}", repo);

            // Override repo in config from CLI arg
            std::env::set_var("HAMMURABI_REPO", &repo);
            let config = config::load()?;

            poller::run_daemon(config).await?;
        }
        Commands::Status => {
            let db = open_db()?;
            let mut issues = db.get_all_issues()?;
            issues.sort_by(|a, b| {
                a.state
                    .sort_priority()
                    .cmp(&b.state.sort_priority())
                    .then(a.github_issue_number.cmp(&b.github_issue_number))
            });

            if issues.is_empty() {
                println!("No tracked issues.");
                return Ok(());
            }

            println!(
                "{:<8} {:<52} {:<22} {:<12} {}",
                "Issue #", "Title", "State", "Age", "Last Activity"
            );
            println!("{}", "-".repeat(110));

            let now = chrono::Utc::now().naive_utc();
            for issue in &issues {
                let title = if issue.title.len() > 50 {
                    format!("{}...", &issue.title[..47])
                } else {
                    issue.title.clone()
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
                    "{:<8} {:<52} {:<22} {:<12} {}",
                    format!("#{}", issue.github_issue_number),
                    title,
                    issue.state,
                    age,
                    last_activity
                );
            }
        }
        Commands::Retry { issue_number } => {
            let db = open_db()?;
            let issue = db
                .get_issue(issue_number)?
                .ok_or_else(|| anyhow::anyhow!("issue #{} not tracked", issue_number))?;

            if issue.state != IssueState::Failed {
                anyhow::bail!(
                    "issue #{} is in {} state, not Failed. Only Failed issues can be retried.",
                    issue_number,
                    issue.state
                );
            }

            let prev = issue.previous_state.ok_or_else(|| {
                anyhow::anyhow!("issue #{} has no previous state to retry from", issue_number)
            })?;

            if prev == IssueState::AgentsWorking {
                db.reset_failed_sub_issues(issue.id)?;
            }

            db.update_issue_state(issue.id, prev, None)?;
            println!(
                "Issue #{} reset from Failed to {}. Will be processed on next poll cycle.",
                issue_number, prev
            );
        }
        Commands::Reset { issue_number } => {
            let db = open_db()?;
            let issue = db
                .get_issue(issue_number)?
                .ok_or_else(|| anyhow::anyhow!("issue #{} not tracked", issue_number))?;

            db.update_issue_state(issue.id, IssueState::Discovered, None)?;
            println!(
                "Issue #{} reset to Discovered. Will be processed on next poll cycle.",
                issue_number
            );
        }
    }

    Ok(())
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
