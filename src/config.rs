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
}

#[derive(Debug, Clone, Deserialize)]
struct RawConfig {
    repo: Option<String>,
    poll_interval: Option<u64>,
    max_concurrent_agents: Option<usize>,
    tracking_label: Option<String>,
    stale_timeout_days: Option<u64>,
    api_retry_count: Option<u32>,
    ai_model: Option<String>,
    ai_max_turns: Option<u32>,
    ai_effort: Option<String>,
    approvers: Option<Vec<String>>,
    github_token: Option<String>,
    github_app: Option<RawGitHubAppConfig>,
    spec: Option<AiTaskConfig>,
    decompose: Option<AiTaskConfig>,
    implement: Option<AiTaskConfig>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub repo: String,
    pub owner: String,
    pub repo_name: String,
    pub poll_interval: u64,
    pub max_concurrent_agents: usize,
    pub tracking_label: String,
    pub stale_timeout_days: u64,
    pub api_retry_count: u32,
    pub ai_model: String,
    pub ai_max_turns: u32,
    pub ai_effort: String,
    pub approvers: Vec<String>,
    pub github_auth: GitHubAuth,
    pub spec: Option<AiTaskConfig>,
    pub decompose: Option<AiTaskConfig>,
    pub implement: Option<AiTaskConfig>,
}

impl Config {
    pub fn ai_model_for_task(&self, task: &str) -> &str {
        let override_model = match task {
            "spec" => self.spec.as_ref().and_then(|c| c.ai_model.as_deref()),
            "decompose" => self.decompose.as_ref().and_then(|c| c.ai_model.as_deref()),
            "implement" => self.implement.as_ref().and_then(|c| c.ai_model.as_deref()),
            _ => None,
        };
        override_model.unwrap_or(&self.ai_model)
    }

    pub fn ai_max_turns_for_task(&self, task: &str) -> u32 {
        let override_turns = match task {
            "spec" => self.spec.as_ref().and_then(|c| c.ai_max_turns),
            "decompose" => self.decompose.as_ref().and_then(|c| c.ai_max_turns),
            "implement" => self.implement.as_ref().and_then(|c| c.ai_max_turns),
            _ => None,
        };
        override_turns.unwrap_or(self.ai_max_turns)
    }

    pub fn ai_effort_for_task(&self, task: &str) -> &str {
        let override_effort = match task {
            "spec" => self.spec.as_ref().and_then(|c| c.ai_effort.as_deref()),
            "decompose" => self.decompose.as_ref().and_then(|c| c.ai_effort.as_deref()),
            "implement" => self.implement.as_ref().and_then(|c| c.ai_effort.as_deref()),
            _ => None,
        };
        override_effort.unwrap_or(&self.ai_effort)
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
            poll_interval: None,
            max_concurrent_agents: None,
            tracking_label: None,
            stale_timeout_days: None,
            api_retry_count: None,
            ai_model: None,
            ai_max_turns: None,
            ai_effort: None,
            approvers: None,
            github_token: None,
            github_app: None,
            spec: None,
            decompose: None,
            implement: None,
        }
    };

    let mut repo = raw
        .repo
        .or_else(|| env_override_option_string("repo"))
        .unwrap_or_default();
    env_override_string("repo", &mut repo);

    let mut poll_interval = raw.poll_interval.unwrap_or(60);
    env_override("poll_interval", &mut poll_interval);

    let mut max_concurrent_agents = raw.max_concurrent_agents.unwrap_or(3);
    env_override("max_concurrent_agents", &mut max_concurrent_agents);

    let mut tracking_label = raw
        .tracking_label
        .unwrap_or_else(|| "hammurabi".to_string());
    env_override_string("tracking_label", &mut tracking_label);

    let mut stale_timeout_days = raw.stale_timeout_days.unwrap_or(7);
    env_override("stale_timeout_days", &mut stale_timeout_days);

    let mut api_retry_count = raw.api_retry_count.unwrap_or(3);
    env_override("api_retry_count", &mut api_retry_count);

    let mut ai_model = raw.ai_model.unwrap_or_default();
    env_override_string("ai_model", &mut ai_model);

    let mut ai_max_turns = raw.ai_max_turns.unwrap_or(50);
    env_override("ai_max_turns", &mut ai_max_turns);

    let mut ai_effort = raw
        .ai_effort
        .unwrap_or_else(|| "high".to_string());
    env_override_string("ai_effort", &mut ai_effort);

    let approvers = raw.approvers.unwrap_or_default();

    // Resolve GitHub authentication: App mode or Token mode
    let mut github_token = raw.github_token.unwrap_or_default();
    if github_token.is_empty() {
        github_token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    }
    env_override_string("github_token", &mut github_token);

    // Check for GitHub App config (from TOML or env vars)
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

    if repo.is_empty() {
        return Err(HammurabiError::Config(
            "repo is required (set in hammurabi.toml or HAMMURABI_REPO)".into(),
        ));
    }

    let (owner, repo_name) = repo
        .split_once('/')
        .ok_or_else(|| HammurabiError::Config("repo must be in owner/repo format".into()))?;

    if ai_model.is_empty() {
        return Err(HammurabiError::Config(
            "ai_model is required (set in hammurabi.toml or HAMMURABI_AI_MODEL)".into(),
        ));
    }

    if approvers.is_empty() {
        return Err(HammurabiError::Config(
            "approvers must contain at least one GitHub username".into(),
        ));
    }

    Ok(Config {
        repo: repo.clone(),
        owner: owner.to_string(),
        repo_name: repo_name.to_string(),
        poll_interval,
        max_concurrent_agents,
        tracking_label,
        stale_timeout_days,
        api_retry_count,
        ai_model,
        ai_max_turns,
        ai_effort,
        approvers,
        github_auth,
        spec: raw.spec,
        decompose: raw.decompose,
        implement: raw.implement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn parse_raw(toml_str: &str) -> Result<Config, HammurabiError> {
        let raw: RawConfig =
            toml::from_str(toml_str).map_err(|e| HammurabiError::Config(e.to_string()))?;

        let repo = raw.repo.unwrap_or_default();
        if repo.is_empty() {
            return Err(HammurabiError::Config("repo is required".into()));
        }
        let (owner, repo_name) = repo
            .split_once('/')
            .ok_or_else(|| HammurabiError::Config("repo must be in owner/repo format".into()))?;
        let ai_model = raw.ai_model.unwrap_or_default();
        if ai_model.is_empty() {
            return Err(HammurabiError::Config("ai_model is required".into()));
        }
        let approvers = raw.approvers.unwrap_or_default();
        if approvers.is_empty() {
            return Err(HammurabiError::Config("approvers required".into()));
        }
        let github_auth = GitHubAuth::Token(
            raw.github_token
                .unwrap_or_else(|| "test-token".to_string()),
        );

        Ok(Config {
            repo: repo.clone(),
            owner: owner.to_string(),
            repo_name: repo_name.to_string(),
            poll_interval: raw.poll_interval.unwrap_or(60),
            max_concurrent_agents: raw.max_concurrent_agents.unwrap_or(3),
            tracking_label: raw
                .tracking_label
                .unwrap_or_else(|| "hammurabi".to_string()),
            stale_timeout_days: raw.stale_timeout_days.unwrap_or(7),
            api_retry_count: raw.api_retry_count.unwrap_or(3),
            ai_model,
            ai_max_turns: raw.ai_max_turns.unwrap_or(50),
            ai_effort: raw.ai_effort.unwrap_or_else(|| "high".to_string()),
            approvers,
            github_auth,
            spec: raw.spec,
            decompose: raw.decompose,
            implement: raw.implement,
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
        assert_eq!(config.repo, "owner/repo");
        assert_eq!(config.owner, "owner");
        assert_eq!(config.repo_name, "repo");
        assert_eq!(config.ai_model, "claude-sonnet-4-6");
        assert_eq!(config.approvers, vec!["alice"]);
        assert_eq!(config.poll_interval, 60);
        assert_eq!(config.max_concurrent_agents, 3);
        assert_eq!(config.tracking_label, "hammurabi");
        assert_eq!(config.stale_timeout_days, 7);
        assert_eq!(config.api_retry_count, 3);
        assert_eq!(config.ai_max_turns, 50);
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

            [decompose]
            ai_max_turns = 30
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(config.ai_model_for_task("spec"), "claude-opus-4-6");
        assert_eq!(config.ai_max_turns_for_task("spec"), 100);
        assert_eq!(config.ai_model_for_task("decompose"), "claude-sonnet-4-6");
        assert_eq!(config.ai_max_turns_for_task("decompose"), 30);
        assert_eq!(config.ai_model_for_task("implement"), "claude-sonnet-4-6");
        assert_eq!(config.ai_max_turns_for_task("implement"), 50);
    }

    #[test]
    fn test_custom_values() {
        let toml = r#"
            repo = "org/project"
            poll_interval = 120
            max_concurrent_agents = 5
            tracking_label = "auto"
            stale_timeout_days = 14
            api_retry_count = 5
            ai_model = "claude-opus-4-6"
            ai_max_turns = 100
            approvers = ["alice", "bob"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(config.poll_interval, 120);
        assert_eq!(config.max_concurrent_agents, 5);
        assert_eq!(config.tracking_label, "auto");
        assert_eq!(config.stale_timeout_days, 14);
        assert_eq!(config.api_retry_count, 5);
        assert_eq!(config.ai_max_turns, 100);
        assert_eq!(config.approvers.len(), 2);
    }

    #[test]
    fn test_env_override() {
        // Test the env_override helper
        let mut val: u64 = 60;
        env::set_var("HAMMURABI_POLL_INTERVAL", "120");
        env_override("poll_interval", &mut val);
        assert_eq!(val, 120);
        env::remove_var("HAMMURABI_POLL_INTERVAL");
    }
}
