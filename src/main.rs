mod approval;
mod claude;
mod config;
mod db;
mod error;
mod github;
mod models;
mod prompts;
mod state_machine;
mod transitions;
mod worktree;

use clap::{Parser, Subcommand};

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
            // TODO: Phase 9 — wire to poller::run_daemon()
            eprintln!("watch command not yet implemented");
        }
        Commands::Status => {
            // TODO: Phase 9 — query DB and print table
            eprintln!("status command not yet implemented");
        }
        Commands::Retry { issue_number } => {
            tracing::info!("Retrying issue #{}", issue_number);
            // TODO: Phase 9 — update Failed → previous_state
            eprintln!("retry command not yet implemented");
        }
        Commands::Reset { issue_number } => {
            tracing::info!("Resetting issue #{}", issue_number);
            // TODO: Phase 9 — update → Discovered
            eprintln!("reset command not yet implemented");
        }
    }

    Ok(())
}
