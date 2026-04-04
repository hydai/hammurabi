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

#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::config::RepoConfig;

    pub fn test_config() -> RepoConfig {
        RepoConfig {
            repo: "owner/repo".to_string(),
            owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            tracking_label: "hammurabi".to_string(),
            stale_timeout_days: 7,
            ai_model: "test-model".to_string(),
            ai_max_turns: 50,
            ai_effort: "high".to_string(),
            ai_timeout_secs: 3600,
            ai_stall_timeout_secs: 0,
            ai_max_retries: 2,
            max_concurrent_agents: 5,
            hooks: crate::config::HooksConfig::default(),
            approvers: vec!["alice".to_string()],
            bypass_label: None,
            review: None,
            review_max_iterations: 2,
            spec: None,
            implement: None,
        }
    }
}
