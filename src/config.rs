use serde::Deserialize;
use std::path::PathBuf;

use crate::error::HammurabiError;

#[derive(Debug, Clone)]
pub enum GitHubAuth {
    Token(String),
    App {
        app_id: u64,
        private_key_pem: Vec<u8>,
        installation_id: u64,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct RawGitHubAppConfig {
    app_id: Option<u64>,
    private_key_path: Option<String>,
    installation_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiTaskConfig {
    pub ai_model: Option<String>,
    pub ai_max_turns: Option<u32>,
    pub ai_effort: Option<String>,
    pub ai_timeout_secs: Option<u64>,
    pub ai_stall_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawRepoEntry {
    repo: Option<String>,
    tracking_label: Option<String>,
    approvers: Option<Vec<String>>,
    ai_model: Option<String>,
    ai_max_turns: Option<u32>,
    ai_effort: Option<String>,
    ai_timeout_secs: Option<u64>,
    ai_stall_timeout_secs: Option<u64>,
    ai_max_retries: Option<u32>,
    max_concurrent_agents: Option<u32>,
    hooks: Option<HooksConfig>,
    review: Option<AiTaskConfig>,
    review_max_iterations: Option<u32>,
    spec: Option<AiTaskConfig>,
    implement: Option<AiTaskConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawConfig {
    // Legacy single-repo field (backward compat)
    repo: Option<String>,
    // Multi-repo array
    repos: Option<Vec<RawRepoEntry>>,

    poll_interval: Option<u64>,
    tracking_label: Option<String>,
    stale_timeout_days: Option<u64>,
    api_retry_count: Option<u32>,
    ai_model: Option<String>,
    ai_max_turns: Option<u32>,
    ai_effort: Option<String>,
    ai_timeout_secs: Option<u64>,
    ai_stall_timeout_secs: Option<u64>,
    ai_max_retries: Option<u32>,
    max_concurrent_agents: Option<u32>,
    approvers: Option<Vec<String>>,
    github_token: Option<String>,
    github_app: Option<RawGitHubAppConfig>,
    bypass_label: Option<String>,
    hooks: Option<HooksConfig>,
    review: Option<AiTaskConfig>,
    review_max_iterations: Option<u32>,
    spec: Option<AiTaskConfig>,
    implement: Option<AiTaskConfig>,
}

/// Per-repo resolved configuration.
#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub repo: String,
    pub owner: String,
    pub repo_name: String,
    pub tracking_label: String,
    pub stale_timeout_days: u64,
    pub ai_model: String,
    pub ai_max_turns: u32,
    pub ai_effort: String,
    pub ai_timeout_secs: u64,
    pub ai_stall_timeout_secs: u64,
    pub ai_max_retries: u32,
    pub max_concurrent_agents: u32,
    pub approvers: Vec<String>,
    pub bypass_label: Option<String>,
    pub hooks: HooksConfig,
    pub review: Option<AiTaskConfig>,
    pub review_max_iterations: u32,
    pub spec: Option<AiTaskConfig>,
    pub implement: Option<AiTaskConfig>,
}

impl RepoConfig {
    fn task_config(&self, task: &str) -> Option<&AiTaskConfig> {
        match task {
            "spec" => self.spec.as_ref(),
            "implement" => self.implement.as_ref(),
            "review" => self.review.as_ref(),
            _ => None,
        }
    }

    pub fn ai_model_for_task(&self, task: &str) -> &str {
        self.task_config(task)
            .and_then(|c| c.ai_model.as_deref())
            .unwrap_or(&self.ai_model)
    }

    pub fn ai_max_turns_for_task(&self, task: &str) -> u32 {
        self.task_config(task)
            .and_then(|c| c.ai_max_turns)
            .unwrap_or(self.ai_max_turns)
    }

    pub fn ai_effort_for_task(&self, task: &str) -> &str {
        self.task_config(task)
            .and_then(|c| c.ai_effort.as_deref())
            .unwrap_or(&self.ai_effort)
    }

    pub fn ai_timeout_for_task(&self, task: &str) -> u64 {
        self.task_config(task)
            .and_then(|c| c.ai_timeout_secs)
            .unwrap_or(self.ai_timeout_secs)
    }

    pub fn ai_stall_timeout_for_task(&self, task: &str) -> u64 {
        self.task_config(task)
            .and_then(|c| c.ai_stall_timeout_secs)
            .unwrap_or(self.ai_stall_timeout_secs)
    }

    /// Create a RepoConfig for a CLI-provided repo, using an existing config as
    /// defaults (if available) or sensible defaults otherwise.
    pub fn from_cli_override(
        repo_str: &str,
        base: Option<&RepoConfig>,
    ) -> Result<RepoConfig, HammurabiError> {
        let (owner, repo_name) = parse_owner_repo(repo_str)?;

        if let Some(b) = base {
            Ok(RepoConfig {
                repo: repo_str.to_string(),
                owner,
                repo_name,
                tracking_label: b.tracking_label.clone(),
                stale_timeout_days: b.stale_timeout_days,
                ai_model: b.ai_model.clone(),
                ai_max_turns: b.ai_max_turns,
                ai_effort: b.ai_effort.clone(),
                ai_timeout_secs: b.ai_timeout_secs,
                ai_stall_timeout_secs: b.ai_stall_timeout_secs,
                ai_max_retries: b.ai_max_retries,
                max_concurrent_agents: b.max_concurrent_agents,
                approvers: b.approvers.clone(),
                bypass_label: b.bypass_label.clone(),
                hooks: b.hooks.clone(),
                review: b.review.clone(),
                review_max_iterations: b.review_max_iterations,
                spec: b.spec.clone(),
                implement: b.implement.clone(),
            })
        } else {
            Err(HammurabiError::Config(
                "cannot use 'watch <repo>' without at least a base config with ai_model and approvers".into(),
            ))
        }
    }
}

/// Global daemon configuration (shared across all repos).
#[derive(Debug, Clone)]
pub struct Config {
    pub poll_interval: u64,
    pub api_retry_count: u32,
    pub github_auth: GitHubAuth,
    pub repos: Vec<RepoConfig>,
}

impl Config {
    /// Backward-compat helper: return the first (and possibly only) repo config.
    /// Panics if repos is empty (should be validated during load).
    pub fn first_repo(&self) -> &RepoConfig {
        &self.repos[0]
    }
}

fn find_config_file() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let cwd_config = cwd.join("hammurabi.toml");
    if cwd_config.exists() {
        return Some(cwd_config);
    }

    let home = dirs_path().join("hammurabi").join("hammurabi.toml");
    if home.exists() {
        return Some(home);
    }

    None
}

fn dirs_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config").join("hammurabi")
    } else {
        PathBuf::from(".config").join("hammurabi")
    }
}

fn env_override<T: std::str::FromStr>(key: &str, value: &mut T) {
    let env_key = format!("HAMMURABI_{}", key.to_uppercase());
    if let Ok(val) = std::env::var(&env_key) {
        if let Ok(parsed) = val.parse() {
            *value = parsed;
        }
    }
}

fn env_override_string(key: &str, value: &mut String) {
    let env_key = format!("HAMMURABI_{}", key.to_uppercase());
    if let Ok(val) = std::env::var(&env_key) {
        if !val.is_empty() {
            *value = val;
        }
    }
}

fn env_override_option_string(key: &str) -> Option<String> {
    let env_key = format!("HAMMURABI_{}", key.to_uppercase());
    std::env::var(&env_key).ok().filter(|v| !v.is_empty())
}

fn parse_owner_repo(repo: &str) -> Result<(String, String), HammurabiError> {
    let (owner, repo_name) = repo
        .split_once('/')
        .ok_or_else(|| HammurabiError::Config("repo must be in owner/repo format".into()))?;

    if owner.is_empty() || repo_name.is_empty() || repo_name.contains('/') {
        return Err(HammurabiError::Config(
            "repo must be in owner/repo format".into(),
        ));
    }

    // Reject path traversal and unsafe filesystem components
    for part in [owner, repo_name] {
        if part == "." || part == ".." || part.contains('\\') || part.contains('\0') {
            return Err(HammurabiError::Config(
                "repo owner/name must not contain path traversal sequences".into(),
            ));
        }
    }

    Ok((owner.to_string(), repo_name.to_string()))
}

pub fn load() -> Result<Config, HammurabiError> {
    let raw: RawConfig = if let Some(path) = find_config_file() {
        let content = std::fs::read_to_string(&path).map_err(|e| {
            HammurabiError::Config(format!("failed to read {}: {}", path.display(), e))
        })?;
        toml::from_str(&content)
            .map_err(|e| HammurabiError::Config(format!("failed to parse config: {}", e)))?
    } else {
        RawConfig {
            repo: None,
            repos: None,
            poll_interval: None,
            tracking_label: None,
            stale_timeout_days: None,
            api_retry_count: None,
            ai_model: None,
            ai_max_turns: None,
            ai_effort: None,
            ai_timeout_secs: None,
            ai_stall_timeout_secs: None,
            ai_max_retries: None,
            max_concurrent_agents: None,
            approvers: None,
            github_token: None,
            github_app: None,
            bypass_label: None,
            hooks: None,
            review: None,
            review_max_iterations: None,
            spec: None,
            implement: None,
        }
    };

    // --- Global defaults ---
    let mut poll_interval = raw.poll_interval.unwrap_or(60);
    env_override("poll_interval", &mut poll_interval);

    let mut global_tracking_label = raw
        .tracking_label
        .unwrap_or_else(|| "hammurabi".to_string());
    env_override_string("tracking_label", &mut global_tracking_label);

    let mut stale_timeout_days = raw.stale_timeout_days.unwrap_or(7);
    env_override("stale_timeout_days", &mut stale_timeout_days);

    let mut api_retry_count = raw.api_retry_count.unwrap_or(3);
    env_override("api_retry_count", &mut api_retry_count);

    let mut global_ai_model = raw.ai_model.unwrap_or_default();
    env_override_string("ai_model", &mut global_ai_model);

    let mut global_ai_max_turns = raw.ai_max_turns.unwrap_or(50);
    env_override("ai_max_turns", &mut global_ai_max_turns);

    let mut global_ai_effort = raw
        .ai_effort
        .unwrap_or_else(|| "high".to_string());
    env_override_string("ai_effort", &mut global_ai_effort);

    let mut global_ai_timeout_secs = raw.ai_timeout_secs.unwrap_or(3600);
    env_override("ai_timeout_secs", &mut global_ai_timeout_secs);

    let mut global_ai_stall_timeout_secs = raw.ai_stall_timeout_secs.unwrap_or(0);
    env_override("ai_stall_timeout_secs", &mut global_ai_stall_timeout_secs);

    let mut global_ai_max_retries = raw.ai_max_retries.unwrap_or(2);
    env_override("ai_max_retries", &mut global_ai_max_retries);

    let mut global_max_concurrent_agents = raw.max_concurrent_agents.unwrap_or(5);
    env_override("max_concurrent_agents", &mut global_max_concurrent_agents);

    let global_approvers = raw.approvers.unwrap_or_default();
    let global_hooks = raw.hooks.unwrap_or_default();
    let global_review = raw.review;
    let mut global_review_max_iterations = raw.review_max_iterations.unwrap_or(2);
    env_override("review_max_iterations", &mut global_review_max_iterations);
    if global_review_max_iterations < 1 {
        global_review_max_iterations = 1;
    }
    let global_spec = raw.spec;
    let global_implement = raw.implement;

    let bypass_label = raw
        .bypass_label
        .or_else(|| env_override_option_string("bypass_label"));

    // --- GitHub authentication ---
    let mut github_token = raw.github_token.unwrap_or_default();
    if github_token.is_empty() {
        github_token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    }
    env_override_string("github_token", &mut github_token);

    let app_id = raw
        .github_app
        .as_ref()
        .and_then(|a| a.app_id)
        .or_else(|| {
            env_override_option_string("github_app_id")
                .and_then(|v| v.parse().ok())
        });
    let app_key_path = raw
        .github_app
        .as_ref()
        .and_then(|a| a.private_key_path.clone())
        .or_else(|| env_override_option_string("github_app_private_key_path"));
    let app_installation_id = raw
        .github_app
        .as_ref()
        .and_then(|a| a.installation_id)
        .or_else(|| {
            env_override_option_string("github_app_installation_id")
                .and_then(|v| v.parse().ok())
        });

    let has_app_config = app_id.is_some() || app_key_path.is_some() || app_installation_id.is_some();
    let has_token = !github_token.is_empty();

    let github_auth = if has_app_config && has_token {
        return Err(HammurabiError::Config(
            "set either github_token or [github_app], not both".into(),
        ));
    } else if has_app_config {
        let app_id = app_id.ok_or_else(|| {
            HammurabiError::Config("[github_app] requires app_id".into())
        })?;
        let key_path = app_key_path.ok_or_else(|| {
            HammurabiError::Config("[github_app] requires private_key_path".into())
        })?;
        let installation_id = app_installation_id.ok_or_else(|| {
            HammurabiError::Config("[github_app] requires installation_id".into())
        })?;
        let private_key_pem = std::fs::read(&key_path).map_err(|e| {
            HammurabiError::Config(format!("failed to read private key {}: {}", key_path, e))
        })?;
        GitHubAuth::App {
            app_id,
            private_key_pem,
            installation_id,
        }
    } else if has_token {
        GitHubAuth::Token(github_token)
    } else {
        return Err(HammurabiError::Config(
            "github_token or [github_app] is required".into(),
        ));
    };

    // --- Build repo configs ---
    // Determine repo entries: either from [[repos]] array or legacy single `repo` field
    let toml_repo = raw.repo.unwrap_or_default();
    let mut legacy_repo = toml_repo.clone();
    env_override_string("repo", &mut legacy_repo);

    let raw_repo_entries: Vec<RawRepoEntry> = if let Some(repos) = raw.repos {
        if repos.is_empty() {
            return Err(HammurabiError::Config(
                "[[repos]] array must contain at least one entry".into(),
            ));
        }
        // Only error if the TOML file itself has both `repo` and `[[repos]]`.
        // HAMMURABI_REPO (env var / CLI override) is allowed alongside [[repos]]
        // because the CLI `watch <repo>` override replaces the repos list after load.
        if !toml_repo.is_empty() {
            return Err(HammurabiError::Config(
                "cannot set both 'repo' and '[[repos]]' in config file; use one or the other".into(),
            ));
        }
        repos
    } else if !legacy_repo.is_empty() {
        // Backward compat: single repo field → single-element array
        vec![RawRepoEntry {
            repo: Some(legacy_repo),
            tracking_label: None,
            approvers: if global_approvers.is_empty() { None } else { Some(global_approvers.clone()) },
            ai_model: None,
            ai_max_turns: None,
            ai_effort: None,
            ai_timeout_secs: None,
            ai_stall_timeout_secs: None,
            ai_max_retries: None,
            max_concurrent_agents: None,
            hooks: None,
            review: None,
            review_max_iterations: None,
            spec: None,
            implement: None,
        }]
    } else {
        return Err(HammurabiError::Config(
            "either 'repo' or '[[repos]]' is required in config".into(),
        ));
    };

    // Validate global ai_model
    if global_ai_model.is_empty() {
        // Check if all per-repo entries provide their own ai_model
        let all_have_model = raw_repo_entries.iter().all(|r| r.ai_model.is_some());
        if !all_have_model {
            return Err(HammurabiError::Config(
                "ai_model is required (set globally or per-repo in hammurabi.toml or HAMMURABI_AI_MODEL)".into(),
            ));
        }
    }

    // Reject duplicate repo entries
    {
        let mut seen = std::collections::HashSet::new();
        for entry in &raw_repo_entries {
            if let Some(ref repo) = entry.repo {
                if !seen.insert(repo.clone()) {
                    return Err(HammurabiError::Config(format!(
                        "duplicate [[repos]] entry: {}",
                        repo
                    )));
                }
            }
        }
    }

    let mut repo_configs = Vec::with_capacity(raw_repo_entries.len());

    for entry in &raw_repo_entries {
        let repo = entry.repo.as_deref().unwrap_or("");
        if repo.is_empty() {
            return Err(HammurabiError::Config(
                "each [[repos]] entry must have a 'repo' field in owner/repo format".into(),
            ));
        }
        let (owner, repo_name) = parse_owner_repo(repo)?;

        let approvers = entry
            .approvers
            .clone()
            .unwrap_or_else(|| global_approvers.clone());
        if approvers.is_empty() {
            return Err(HammurabiError::Config(format!(
                "approvers must contain at least one GitHub username (repo: {})",
                repo
            )));
        }

        let ai_model = entry
            .ai_model
            .clone()
            .unwrap_or_else(|| global_ai_model.clone());
        if ai_model.is_empty() {
            return Err(HammurabiError::Config(format!(
                "ai_model is required for repo {}",
                repo
            )));
        }

        repo_configs.push(RepoConfig {
            repo: repo.to_string(),
            owner,
            repo_name,
            tracking_label: entry
                .tracking_label
                .clone()
                .unwrap_or_else(|| global_tracking_label.clone()),
            stale_timeout_days,
            ai_model,
            ai_max_turns: entry.ai_max_turns.unwrap_or(global_ai_max_turns),
            ai_effort: entry
                .ai_effort
                .clone()
                .unwrap_or_else(|| global_ai_effort.clone()),
            ai_timeout_secs: entry.ai_timeout_secs.unwrap_or(global_ai_timeout_secs),
            ai_stall_timeout_secs: entry
                .ai_stall_timeout_secs
                .unwrap_or(global_ai_stall_timeout_secs),
            ai_max_retries: entry.ai_max_retries.unwrap_or(global_ai_max_retries),
            max_concurrent_agents: entry
                .max_concurrent_agents
                .unwrap_or(global_max_concurrent_agents),
            approvers,
            bypass_label: bypass_label.clone(),
            hooks: entry.hooks.clone().unwrap_or_else(|| global_hooks.clone()),
            review: entry.review.clone().or_else(|| global_review.clone()),
            review_max_iterations: entry
                .review_max_iterations
                .unwrap_or(global_review_max_iterations)
                .max(1),
            spec: entry.spec.clone().or_else(|| global_spec.clone()),
            implement: entry.implement.clone().or_else(|| global_implement.clone()),
        });
    }

    Ok(Config {
        poll_interval,
        api_retry_count,
        github_auth,
        repos: repo_configs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_raw(toml_str: &str) -> Result<Config, HammurabiError> {
        let raw: RawConfig =
            toml::from_str(toml_str).map_err(|e| HammurabiError::Config(e.to_string()))?;

        // Simplified parser for tests — uses Token auth, no env overrides
        let github_auth = GitHubAuth::Token(
            raw.github_token
                .unwrap_or_else(|| "test-token".to_string()),
        );

        let global_tracking_label = raw
            .tracking_label
            .unwrap_or_else(|| "hammurabi".to_string());
        let global_ai_model = raw.ai_model.unwrap_or_default();
        let global_ai_max_turns = raw.ai_max_turns.unwrap_or(50);
        let global_ai_effort = raw.ai_effort.unwrap_or_else(|| "high".to_string());
        let global_ai_timeout_secs = raw.ai_timeout_secs.unwrap_or(3600);
        let global_ai_stall_timeout_secs = raw.ai_stall_timeout_secs.unwrap_or(0);
        let global_ai_max_retries = raw.ai_max_retries.unwrap_or(2);
        let global_max_concurrent_agents = raw.max_concurrent_agents.unwrap_or(5);
        let global_approvers = raw.approvers.unwrap_or_default();
        let global_hooks = raw.hooks.unwrap_or_default();
        let global_review = raw.review;
        let global_review_max_iterations = raw.review_max_iterations.unwrap_or(2);
        let global_spec = raw.spec;
        let global_implement = raw.implement;

        let legacy_repo = raw.repo.unwrap_or_default();

        let raw_repo_entries: Vec<RawRepoEntry> = if let Some(repos) = raw.repos {
            repos
        } else if !legacy_repo.is_empty() {
            vec![RawRepoEntry {
                repo: Some(legacy_repo),
                tracking_label: None,
                approvers: if global_approvers.is_empty() { None } else { Some(global_approvers.clone()) },
                ..Default::default()
            }]
        } else {
            return Err(HammurabiError::Config("repo is required".into()));
        };

        // Reject duplicate repos
        {
            let mut seen = std::collections::HashSet::new();
            for entry in &raw_repo_entries {
                if let Some(ref repo) = entry.repo {
                    if !seen.insert(repo.clone()) {
                        return Err(HammurabiError::Config(format!(
                            "duplicate [[repos]] entry: {}", repo
                        )));
                    }
                }
            }
        }

        let mut repo_configs = Vec::new();
        for entry in &raw_repo_entries {
            let repo = entry.repo.as_deref().unwrap_or("");
            if repo.is_empty() {
                return Err(HammurabiError::Config("repo is required in each [[repos]] entry".into()));
            }
            let (owner, repo_name) = parse_owner_repo(repo)?;

            let approvers = entry.approvers.clone().unwrap_or_else(|| global_approvers.clone());
            if approvers.is_empty() {
                return Err(HammurabiError::Config("approvers required".into()));
            }

            let ai_model = entry.ai_model.clone().unwrap_or_else(|| global_ai_model.clone());
            if ai_model.is_empty() {
                return Err(HammurabiError::Config("ai_model is required".into()));
            }

            repo_configs.push(RepoConfig {
                repo: repo.to_string(),
                owner,
                repo_name,
                tracking_label: entry.tracking_label.clone().unwrap_or_else(|| global_tracking_label.clone()),
                stale_timeout_days: raw.stale_timeout_days.unwrap_or(7),
                ai_model,
                ai_max_turns: entry.ai_max_turns.unwrap_or(global_ai_max_turns),
                ai_effort: entry.ai_effort.clone().unwrap_or_else(|| global_ai_effort.clone()),
                ai_timeout_secs: entry.ai_timeout_secs.unwrap_or(global_ai_timeout_secs),
                ai_stall_timeout_secs: entry.ai_stall_timeout_secs.unwrap_or(global_ai_stall_timeout_secs),
                ai_max_retries: entry.ai_max_retries.unwrap_or(global_ai_max_retries),
                max_concurrent_agents: entry.max_concurrent_agents.unwrap_or(global_max_concurrent_agents),
                approvers,
                bypass_label: raw.bypass_label.clone(),
                hooks: entry.hooks.clone().unwrap_or_else(|| global_hooks.clone()),
                review: entry.review.clone().or_else(|| global_review.clone()),
                review_max_iterations: entry.review_max_iterations.unwrap_or(global_review_max_iterations).max(1),
                spec: entry.spec.clone().or_else(|| global_spec.clone()),
                implement: entry.implement.clone().or_else(|| global_implement.clone()),
            });
        }

        Ok(Config {
            poll_interval: raw.poll_interval.unwrap_or(60),
            api_retry_count: raw.api_retry_count.unwrap_or(3),
            github_auth,
            repos: repo_configs,
        })
    }

    #[test]
    fn test_valid_config() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.repo, "owner/repo");
        assert_eq!(rc.owner, "owner");
        assert_eq!(rc.repo_name, "repo");
        assert_eq!(rc.ai_model, "claude-sonnet-4-6");
        assert_eq!(rc.approvers, vec!["alice"]);
        assert_eq!(config.poll_interval, 60);
        assert_eq!(rc.tracking_label, "hammurabi");
        assert_eq!(rc.stale_timeout_days, 7);
        assert_eq!(config.api_retry_count, 3);
        assert_eq!(rc.ai_max_turns, 50);
    }

    #[test]
    fn test_missing_repo() {
        let toml = r#"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("repo"));
    }

    #[test]
    fn test_invalid_repo_format() {
        let toml = r#"
            repo = "noslash"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("owner/repo"));
    }

    #[test]
    fn test_missing_ai_model() {
        let toml = r#"
            repo = "owner/repo"
            approvers = ["alice"]
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("ai_model"));
    }

    #[test]
    fn test_missing_approvers() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("approvers"));
    }

    #[test]
    fn test_per_task_overrides() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            ai_max_turns = 50
            approvers = ["alice"]
            github_token = "ghp_test"

            [spec]
            ai_model = "claude-opus-4-6"
            ai_max_turns = 100
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.ai_model_for_task("spec"), "claude-opus-4-6");
        assert_eq!(rc.ai_max_turns_for_task("spec"), 100);
        assert_eq!(rc.ai_model_for_task("implement"), "claude-sonnet-4-6");
        assert_eq!(rc.ai_max_turns_for_task("implement"), 50);
    }

    #[test]
    fn test_custom_values() {
        let toml = r#"
            repo = "org/project"
            poll_interval = 120
            tracking_label = "auto"
            stale_timeout_days = 14
            api_retry_count = 5
            ai_model = "claude-opus-4-6"
            ai_max_turns = 100
            approvers = ["alice", "bob"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(config.poll_interval, 120);
        assert_eq!(rc.tracking_label, "auto");
        assert_eq!(rc.stale_timeout_days, 14);
        assert_eq!(config.api_retry_count, 5);
        assert_eq!(rc.ai_max_turns, 100);
        assert_eq!(rc.approvers.len(), 2);
    }

    #[test]
    fn test_timeout_defaults() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.ai_timeout_secs, 3600);
        assert_eq!(rc.ai_stall_timeout_secs, 0);
        assert_eq!(rc.ai_max_retries, 2);
    }

    #[test]
    fn test_timeout_custom_values() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            ai_timeout_secs = 7200
            ai_stall_timeout_secs = 600
            ai_max_retries = 5
            approvers = ["alice"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.ai_timeout_secs, 7200);
        assert_eq!(rc.ai_stall_timeout_secs, 600);
        assert_eq!(rc.ai_max_retries, 5);
    }

    #[test]
    fn test_timeout_per_task_overrides() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            ai_timeout_secs = 3600
            ai_stall_timeout_secs = 300
            approvers = ["alice"]
            github_token = "ghp_test"

            [spec]
            ai_timeout_secs = 1800
            ai_stall_timeout_secs = 120

            [implement]
            ai_timeout_secs = 7200
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.ai_timeout_for_task("spec"), 1800);
        assert_eq!(rc.ai_stall_timeout_for_task("spec"), 120);
        assert_eq!(rc.ai_timeout_for_task("implement"), 7200);
        assert_eq!(rc.ai_stall_timeout_for_task("implement"), 300); // falls back to global
        assert_eq!(rc.ai_timeout_for_task("other"), 3600); // falls back to global
    }

    #[test]
    fn test_env_override() {
        let mut val: u64 = 60;
        std::env::set_var("HAMMURABI_POLL_INTERVAL", "120");
        env_override("poll_interval", &mut val);
        assert_eq!(val, 120);
        std::env::remove_var("HAMMURABI_POLL_INTERVAL");
    }

    // --- Multi-repo tests ---

    #[test]
    fn test_multi_repo_config() {
        let toml = r#"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"

            [[repos]]
            repo = "owner/repo-a"

            [[repos]]
            repo = "owner/repo-b"
            tracking_label = "auto"
            approvers = ["bob"]
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(config.repos.len(), 2);

        let a = &config.repos[0];
        assert_eq!(a.repo, "owner/repo-a");
        assert_eq!(a.tracking_label, "hammurabi");
        assert_eq!(a.approvers, vec!["alice"]);

        let b = &config.repos[1];
        assert_eq!(b.repo, "owner/repo-b");
        assert_eq!(b.tracking_label, "auto");
        assert_eq!(b.approvers, vec!["bob"]);
    }

    #[test]
    fn test_multi_repo_per_repo_overrides() {
        let toml = r#"
            ai_model = "claude-sonnet-4-6"
            ai_max_turns = 50
            max_concurrent_agents = 5
            approvers = ["alice"]
            github_token = "ghp_test"

            [[repos]]
            repo = "owner/repo-a"

            [[repos]]
            repo = "owner/repo-b"
            ai_model = "claude-opus-4-6"
            ai_max_turns = 100
            max_concurrent_agents = 2
        "#;
        let config = parse_raw(toml).unwrap();

        let a = &config.repos[0];
        assert_eq!(a.ai_model, "claude-sonnet-4-6");
        assert_eq!(a.ai_max_turns, 50);
        assert_eq!(a.max_concurrent_agents, 5);

        let b = &config.repos[1];
        assert_eq!(b.ai_model, "claude-opus-4-6");
        assert_eq!(b.ai_max_turns, 100);
        assert_eq!(b.max_concurrent_agents, 2);
    }

    #[test]
    fn test_multi_repo_missing_repo_field() {
        let toml = r#"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"

            [[repos]]
            tracking_label = "auto"
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("repo"));
    }

    #[test]
    fn test_legacy_single_repo_compat() {
        // Existing single-repo config should still work
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].repo, "owner/repo");
    }

    #[test]
    fn test_duplicate_repos_rejected() {
        let toml = r#"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"

            [[repos]]
            repo = "owner/repo-a"

            [[repos]]
            repo = "owner/repo-a"
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_bypass_label_default_none() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        assert!(config.repos[0].bypass_label.is_none());
    }

    #[test]
    fn test_bypass_label_configured() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "claude-sonnet-4-6"
            approvers = ["alice"]
            github_token = "ghp_test"
            bypass_label = "hammurabi-bypass"
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(config.repos[0].bypass_label.as_deref(), Some("hammurabi-bypass"));
    }
}
