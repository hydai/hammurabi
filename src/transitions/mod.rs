pub mod completion;
pub mod implementing;
pub mod reviewing;
pub mod spec_drafting;

use std::sync::Arc;

use crate::claude::AiAgent;
use crate::config::RepoConfig;
use crate::db::Database;
use crate::github::GitHubClient;
use crate::worktree::WorktreeManager;

#[derive(Clone)]
pub struct TransitionContext {
    pub github: Arc<dyn GitHubClient>,
    pub ai: Arc<dyn AiAgent>,
    pub worktree: Arc<dyn WorktreeManager>,
    pub db: Arc<Database>,
    pub config: Arc<RepoConfig>,
}
