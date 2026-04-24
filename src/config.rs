use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::access::{AllowUsers, RawAccess};
use crate::acp::session::AcpAgentDef;
use crate::agents::acp::default_agent_def;
use crate::agents::AgentKind;
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
    /// Override which agent implementation to use for this task.
    pub agent_kind: Option<AgentKind>,
}

/// Raw `[agents.*]` section in the config TOML. Each field supplies an
/// override for the subprocess invocation of one ACP agent kind; missing
/// fields fall back to the hard-coded defaults in `default_agent_def`.
#[derive(Debug, Clone, Deserialize, Default)]
struct RawAcpAgentDef {
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
}

impl RawAcpAgentDef {
    fn resolve(self, kind: AgentKind) -> AcpAgentDef {
        let defaults = default_agent_def(kind);
        AcpAgentDef {
            command: self.command.unwrap_or(defaults.command),
            args: self.args.unwrap_or(defaults.args),
            env: self.env.unwrap_or(defaults.env),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawAgentsBlock {
    acp_claude: Option<RawAcpAgentDef>,
    acp_gemini: Option<RawAcpAgentDef>,
    acp_codex: Option<RawAcpAgentDef>,
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
    /// Per-repo default agent kind. Overrides the global default.
    agent_kind: Option<AgentKind>,
}

/// Raw `[[sources]]` entry. Tagged by `kind`; currently only `discord`
/// is supported. Non-Discord kinds parse as their matching variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RawSourceEntry {
    Discord(RawDiscordChannel),
}

#[derive(Debug, Clone, Deserialize)]
struct RawDiscordChannel {
    /// Optional human-readable label; used in logs.
    name: Option<String>,
    channel_id: String,
    /// Points at a configured `[[repos]]` entry (`owner/name`).
    repo: String,
    /// Env-expanded bot token. `${VAR}` is substituted from the
    /// environment at load time.
    bot_token: String,
    /// Users allowed to `/confirm` the spec once it's drafted. Defaults
    /// to the target repo's approvers if empty.
    #[serde(default)]
    approvers: Vec<String>,
    /// Override the target repo's agent kind for Discord-originated work.
    agent_kind: Option<AgentKind>,
    /// Command prefix for `/confirm`, `/revise`, `/cancel`. Default `/`.
    command_prefix: Option<String>,
    /// Safety cap on back-and-forth `/revise` iterations.
    max_draft_revisions: Option<u32>,
    /// Access control (see `AllowUsers`).
    #[serde(flatten)]
    access: RawAccess,
}

#[derive(Debug, Clone, Deserialize)]
struct RawConfig {
    // Legacy single-repo field (backward compat)
    repo: Option<String>,
    // Multi-repo array
    repos: Option<Vec<RawRepoEntry>>,
    /// Alternative intake sources (Discord channels, future platforms).
    /// Each entry references a `[[repos]]` entry by `owner/name`.
    sources: Option<Vec<RawSourceEntry>>,

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
    /// Global default agent kind. Applies to every repo that doesn't set
    /// its own. Missing → `AgentKind::ClaudeCli`.
    agent_kind: Option<AgentKind>,
    /// `[agents.*]` subsections overriding the hard-coded ACP defaults.
    agents: Option<RawAgentsBlock>,
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
    /// Resolved per-repo default agent kind. Initialised at load time from
    /// the per-repo override or the global default.
    pub agent_kind: AgentKind,
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

    /// Resolve the agent kind for a task. Precedence:
    /// per-task override > per-repo default > global default > `ClaudeCli`.
    pub fn agent_kind_for_task(&self, task: &str) -> AgentKind {
        self.task_config(task)
            .and_then(|c| c.agent_kind)
            .unwrap_or(self.agent_kind)
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
                agent_kind: b.agent_kind,
            })
        } else {
            Err(HammurabiError::Config(
                "cannot use 'watch <repo>' without at least a base config with ai_model and approvers".into(),
            ))
        }
    }
}

/// Resolved Discord intake configuration. One entry per Discord channel
/// Hammurabi should listen on. Holds every field needed to (a) connect
/// the bot, (b) enforce access control, and (c) route approved specs
/// into the target GitHub repo at `/confirm`.
#[derive(Clone)]
#[allow(dead_code)]
pub struct DiscordChannelConfig {
    pub name: String,
    pub channel_id: u64,
    /// `owner/name` — must resolve to a `RepoConfig` in `Config::repos`.
    pub repo: String,
    pub bot_token: String,
    pub approvers: Vec<String>,
    pub agent_kind: Option<AgentKind>,
    pub command_prefix: String,
    pub max_draft_revisions: u32,
    pub allow: AllowUsers,
}

// Manual Debug so bot_token never leaks into tracing output.
impl std::fmt::Debug for DiscordChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordChannelConfig")
            .field("name", &self.name)
            .field("channel_id", &self.channel_id)
            .field("repo", &self.repo)
            .field("bot_token", &"<redacted>")
            .field("approvers", &self.approvers)
            .field("agent_kind", &self.agent_kind)
            .field("command_prefix", &self.command_prefix)
            .field("max_draft_revisions", &self.max_draft_revisions)
            .field("allow", &self.allow)
            .finish()
    }
}

/// Resolved intake source. Kept as an enum so future platforms (Slack,
/// Teams, generic webhooks) slot in without breaking the downstream shape.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SourceEntry {
    Discord(DiscordChannelConfig),
}

/// Global daemon configuration (shared across all repos).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    pub poll_interval: u64,
    pub api_retry_count: u32,
    pub github_auth: GitHubAuth,
    pub repos: Vec<RepoConfig>,
    /// Non-GitHub-issue intake sources (Discord channels, ...). Empty by default.
    pub sources: Vec<SourceEntry>,
    /// Resolved subprocess definitions for each ACP kind. Built from
    /// hard-coded defaults, overridden by any `[agents.*]` sections in the
    /// config TOML.
    pub agents: HashMap<AgentKind, AcpAgentDef>,
}

/// Build the ACP agent definition map from an optional `[agents]` block in
/// the raw config, falling back to hard-coded defaults for missing kinds.
fn resolve_agent_defs(raw: Option<RawAgentsBlock>) -> HashMap<AgentKind, AcpAgentDef> {
    let raw = raw.unwrap_or_default();
    let mut out = HashMap::new();
    out.insert(
        AgentKind::AcpClaude,
        raw.acp_claude
            .unwrap_or_default()
            .resolve(AgentKind::AcpClaude),
    );
    out.insert(
        AgentKind::AcpGemini,
        raw.acp_gemini
            .unwrap_or_default()
            .resolve(AgentKind::AcpGemini),
    );
    out.insert(
        AgentKind::AcpCodex,
        raw.acp_codex
            .unwrap_or_default()
            .resolve(AgentKind::AcpCodex),
    );
    out
}

impl Config {
    /// Backward-compat helper: return the first (and possibly only) repo config.
    /// Panics if repos is empty (should be validated during load).
    #[allow(dead_code)]
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

/// Where to read the Hammurabi config from.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// Explicit local path, or `None` to fall through to `HAMMURABI_CONFIG_PATH`
    /// then autodetect (`./hammurabi.toml`, `$HOME/.config/hammurabi/hammurabi.toml`).
    Path(Option<PathBuf>),
    /// Remote HTTPS URL.
    Url(String),
}

impl ConfigSource {
    /// Parse a raw string from `--config` / `HAMMURABI_CONFIG_PATH`. Strings
    /// beginning with `https://` (or `http://`, which is rejected downstream)
    /// become `Url`; everything else is treated as a filesystem path.
    pub fn from_raw(raw: &str) -> Self {
        if raw.starts_with("https://") || raw.starts_with("http://") {
            ConfigSource::Url(raw.to_string())
        } else {
            ConfigSource::Path(Some(PathBuf::from(raw)))
        }
    }
}

/// Load the resolved `Config` from the given source.
pub async fn load(source: &ConfigSource) -> Result<Config, HammurabiError> {
    match source {
        ConfigSource::Path(p) => load_from(p.as_deref()),
        ConfigSource::Url(u) => load_from_url(u).await,
    }
}

/// Load the resolved `Config`, optionally from an explicit path supplied by
/// the caller (`hammurabi --config <path>`). Precedence for the source file:
/// 1. `explicit_path` (from CLI flag)
/// 2. `HAMMURABI_CONFIG_PATH` env var (only interpreted as a path; URLs must
///    flow in through `ConfigSource::Url` because this entry point is sync)
/// 3. `./hammurabi.toml` in CWD
/// 4. `$HOME/.config/hammurabi/hammurabi.toml`
/// When none of the above is set or readable, an empty `RawConfig` is used
/// so env-var-only operation (`HAMMURABI_*` overrides) still works.
pub fn load_from(explicit_path: Option<&Path>) -> Result<Config, HammurabiError> {
    build_config(read_raw_from_path(explicit_path)?)
}

/// Fetch a remote `hammurabi.toml` over HTTPS and build a resolved `Config`.
/// Enforces `https://` only, a 10 s connect / 30 s total timeout, and a
/// 1 MiB body cap. The downloaded body goes through the same parser and
/// validation pipeline as a local config file.
pub async fn load_from_url(url: &str) -> Result<Config, HammurabiError> {
    let body = fetch_remote_config(url).await?;
    let raw: RawConfig = toml::from_str(&body).map_err(|e| {
        HammurabiError::Config(format!("failed to parse remote config ({}): {}", url, e))
    })?;
    build_config(raw)
}

/// Resolve a source file path with the documented precedence, then read and
/// parse it. When no path is resolved, returns an empty `RawConfig` so the
/// downstream validation runs purely on env-var overrides and defaults.
fn read_raw_from_path(explicit_path: Option<&Path>) -> Result<RawConfig, HammurabiError> {
    let resolved_path: Option<PathBuf> = explicit_path
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var_os("HAMMURABI_CONFIG_PATH").map(PathBuf::from))
        .or_else(find_config_file);

    if let Some(path) = resolved_path {
        let content = std::fs::read_to_string(&path).map_err(|e| {
            HammurabiError::Config(format!("failed to read {}: {}", path.display(), e))
        })?;
        toml::from_str(&content)
            .map_err(|e| HammurabiError::Config(format!("failed to parse config: {}", e)))
    } else {
        Ok(empty_raw_config())
    }
}

fn empty_raw_config() -> RawConfig {
    RawConfig {
        repo: None,
        repos: None,
        sources: None,
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
        agent_kind: None,
        agents: None,
    }
}

/// Download the body at `url` with tight timeouts and a 1 MiB cap. HTTPS-only
/// to keep the token interpolation pipeline from pulling secret-bearing TOML
/// over cleartext HTTP.
async fn fetch_remote_config(url: &str) -> Result<String, HammurabiError> {
    if !url.starts_with("https://") {
        return Err(HammurabiError::Config(format!(
            "remote config must use https:// (got {})",
            url
        )));
    }

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| HammurabiError::Config(format!("failed to build HTTP client: {}", e)))?;

    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| HammurabiError::Config(format!("failed to fetch {}: {}", url, e)))?;

    let status = response.status();
    if !status.is_success() {
        return Err(HammurabiError::Config(format!(
            "fetching {} returned HTTP {}",
            url, status
        )));
    }

    // Stream chunks with a running size cap so a hostile or misconfigured
    // server cannot OOM us with an unbounded body.
    const MAX_BYTES: usize = 1024 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| HammurabiError::Config(format!("failed to read {}: {}", url, e)))?
    {
        if buf.len() + chunk.len() > MAX_BYTES {
            return Err(HammurabiError::Config(format!(
                "remote config exceeds 1 MiB cap ({})",
                url
            )));
        }
        buf.extend_from_slice(&chunk);
    }

    String::from_utf8(buf).map_err(|e| {
        HammurabiError::Config(format!("remote config is not valid UTF-8 ({}): {}", url, e))
    })
}

fn build_config(raw: RawConfig) -> Result<Config, HammurabiError> {
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

    let mut global_ai_effort = raw.ai_effort.unwrap_or_else(|| "high".to_string());
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
    let global_agent_kind = raw.agent_kind.unwrap_or(AgentKind::ClaudeCli);
    let agents = resolve_agent_defs(raw.agents);

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
        .or_else(|| env_override_option_string("github_app_id").and_then(|v| v.parse().ok()));
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
            env_override_option_string("github_app_installation_id").and_then(|v| v.parse().ok())
        });

    let has_app_config =
        app_id.is_some() || app_key_path.is_some() || app_installation_id.is_some();
    let has_token = !github_token.is_empty();

    let github_auth = if has_app_config && has_token {
        return Err(HammurabiError::Config(
            "set either github_token or [github_app], not both".into(),
        ));
    } else if has_app_config {
        let app_id =
            app_id.ok_or_else(|| HammurabiError::Config("[github_app] requires app_id".into()))?;
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
                "cannot set both 'repo' and '[[repos]]' in config file; use one or the other"
                    .into(),
            ));
        }
        repos
    } else if !legacy_repo.is_empty() {
        // Backward compat: single repo field → single-element array
        vec![RawRepoEntry {
            repo: Some(legacy_repo),
            approvers: if global_approvers.is_empty() {
                None
            } else {
                Some(global_approvers.clone())
            },
            ..Default::default()
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
            agent_kind: entry.agent_kind.unwrap_or(global_agent_kind),
        });
    }

    let sources = resolve_sources(raw.sources, &repo_configs)?;

    Ok(Config {
        poll_interval,
        api_retry_count,
        github_auth,
        repos: repo_configs,
        sources,
        agents,
    })
}

/// Resolve raw `[[sources]]` entries into runtime `SourceEntry`s. Errors if
/// a source references a `repo` that isn't declared in `[[repos]]`, if
/// access rules conflict (both `allow_all_users` and `allow_users` set),
/// or if a Discord source is missing required fields (channel_id, token).
fn resolve_sources(
    raw: Option<Vec<RawSourceEntry>>,
    repos: &[RepoConfig],
) -> Result<Vec<SourceEntry>, HammurabiError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(raw.len());
    let mut seen_channels = std::collections::HashSet::new();
    for entry in raw {
        match entry {
            RawSourceEntry::Discord(d) => {
                if !repos.iter().any(|r| r.repo == d.repo) {
                    return Err(HammurabiError::Config(format!(
                        "source references repo '{}' which is not declared in [[repos]]",
                        d.repo
                    )));
                }
                let channel_id: u64 = d.channel_id.parse().map_err(|e| {
                    HammurabiError::Config(format!(
                        "invalid channel_id '{}' for Discord source: {}",
                        d.channel_id, e
                    ))
                })?;
                if !seen_channels.insert(channel_id) {
                    return Err(HammurabiError::Config(format!(
                        "duplicate Discord channel_id: {}",
                        channel_id
                    )));
                }
                if d.access.allow_all_users && !d.access.allow_users.is_empty() {
                    return Err(HammurabiError::Config(
                        "set either 'allow_all_users = true' or 'allow_users = [..]', not both"
                            .into(),
                    ));
                }
                let bot_token = expand_env(&d.bot_token);
                if bot_token.trim().is_empty() {
                    return Err(HammurabiError::Config(format!(
                        "Discord source for channel {} is missing bot_token",
                        channel_id
                    )));
                }
                let approvers = if d.approvers.is_empty() {
                    // Inherit from the target repo's approvers.
                    repos
                        .iter()
                        .find(|r| r.repo == d.repo)
                        .map(|r| r.approvers.clone())
                        .unwrap_or_default()
                } else {
                    d.approvers
                };
                out.push(SourceEntry::Discord(DiscordChannelConfig {
                    name: d.name.unwrap_or_else(|| format!("discord:{}", channel_id)),
                    channel_id,
                    repo: d.repo,
                    bot_token,
                    approvers,
                    agent_kind: d.agent_kind,
                    command_prefix: d.command_prefix.unwrap_or_else(|| "/".to_string()),
                    max_draft_revisions: d.max_draft_revisions.unwrap_or(5),
                    allow: AllowUsers::from_raw(d.access.allow_all_users, d.access.allow_users),
                }));
            }
        }
    }
    Ok(out)
}

/// Expand a single `${VAR}` form from the process environment. Leaves
/// strings without `${` untouched. Used for `bot_token = "${DISCORD_BOT_TOKEN}"`.
fn expand_env(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("${") {
        if let Some(var) = rest.strip_suffix('}') {
            return std::env::var(var).unwrap_or_default();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_source_from_raw_detects_scheme() {
        match ConfigSource::from_raw("https://example.com/hammurabi.toml") {
            ConfigSource::Url(u) => assert_eq!(u, "https://example.com/hammurabi.toml"),
            _ => panic!("expected Url variant"),
        }
        match ConfigSource::from_raw("http://insecure.example/hammurabi.toml") {
            ConfigSource::Url(u) => assert_eq!(u, "http://insecure.example/hammurabi.toml"),
            _ => panic!("expected Url variant (scheme rejected at fetch time)"),
        }
        match ConfigSource::from_raw("/etc/hammurabi/hammurabi.toml") {
            ConfigSource::Path(Some(p)) => {
                assert_eq!(p, PathBuf::from("/etc/hammurabi/hammurabi.toml"))
            }
            _ => panic!("expected Path variant"),
        }
        match ConfigSource::from_raw("hammurabi.toml") {
            ConfigSource::Path(Some(p)) => assert_eq!(p, PathBuf::from("hammurabi.toml")),
            _ => panic!("expected Path variant"),
        }
    }

    #[tokio::test]
    async fn fetch_remote_config_rejects_http() {
        let err = fetch_remote_config("http://example.com/hammurabi.toml")
            .await
            .expect_err("http:// must be rejected");
        let msg = format!("{}", err);
        assert!(
            msg.contains("https://"),
            "error should mention https:// requirement, got: {}",
            msg
        );
    }

    fn parse_raw(toml_str: &str) -> Result<Config, HammurabiError> {
        let raw: RawConfig =
            toml::from_str(toml_str).map_err(|e| HammurabiError::Config(e.to_string()))?;

        // Simplified parser for tests — uses Token auth, no env overrides
        let github_auth =
            GitHubAuth::Token(raw.github_token.unwrap_or_else(|| "test-token".to_string()));

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
        let global_agent_kind = raw.agent_kind.unwrap_or(AgentKind::ClaudeCli);
        let agents = resolve_agent_defs(raw.agents);

        let legacy_repo = raw.repo.unwrap_or_default();

        let raw_repo_entries: Vec<RawRepoEntry> = if let Some(repos) = raw.repos {
            repos
        } else if !legacy_repo.is_empty() {
            vec![RawRepoEntry {
                repo: Some(legacy_repo),
                tracking_label: None,
                approvers: if global_approvers.is_empty() {
                    None
                } else {
                    Some(global_approvers.clone())
                },
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
                            "duplicate [[repos]] entry: {}",
                            repo
                        )));
                    }
                }
            }
        }

        let mut repo_configs = Vec::new();
        for entry in &raw_repo_entries {
            let repo = entry.repo.as_deref().unwrap_or("");
            if repo.is_empty() {
                return Err(HammurabiError::Config(
                    "repo is required in each [[repos]] entry".into(),
                ));
            }
            let (owner, repo_name) = parse_owner_repo(repo)?;

            let approvers = entry
                .approvers
                .clone()
                .unwrap_or_else(|| global_approvers.clone());
            if approvers.is_empty() {
                return Err(HammurabiError::Config("approvers required".into()));
            }

            let ai_model = entry
                .ai_model
                .clone()
                .unwrap_or_else(|| global_ai_model.clone());
            if ai_model.is_empty() {
                return Err(HammurabiError::Config("ai_model is required".into()));
            }

            repo_configs.push(RepoConfig {
                repo: repo.to_string(),
                owner,
                repo_name,
                tracking_label: entry
                    .tracking_label
                    .clone()
                    .unwrap_or_else(|| global_tracking_label.clone()),
                stale_timeout_days: raw.stale_timeout_days.unwrap_or(7),
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
                bypass_label: raw.bypass_label.clone(),
                hooks: entry.hooks.clone().unwrap_or_else(|| global_hooks.clone()),
                review: entry.review.clone().or_else(|| global_review.clone()),
                review_max_iterations: entry
                    .review_max_iterations
                    .unwrap_or(global_review_max_iterations)
                    .max(1),
                spec: entry.spec.clone().or_else(|| global_spec.clone()),
                implement: entry.implement.clone().or_else(|| global_implement.clone()),
                agent_kind: entry.agent_kind.unwrap_or(global_agent_kind),
            });
        }

        let sources = resolve_sources(raw.sources, &repo_configs)?;

        Ok(Config {
            poll_interval: raw.poll_interval.unwrap_or(60),
            api_retry_count: raw.api_retry_count.unwrap_or(3),
            github_auth,
            repos: repo_configs,
            sources,
            agents,
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
    fn test_agent_kind_defaults_to_claude_cli() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "m"
            approvers = ["alice"]
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.agent_kind, AgentKind::ClaudeCli);
        assert_eq!(rc.agent_kind_for_task("spec"), AgentKind::ClaudeCli);
    }

    #[test]
    fn test_global_agent_kind_applies_to_repo() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "m"
            approvers = ["alice"]
            agent_kind = "acp-claude"
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.agent_kind, AgentKind::AcpClaude);
        assert_eq!(rc.agent_kind_for_task("spec"), AgentKind::AcpClaude);
    }

    #[test]
    fn test_per_repo_agent_kind_overrides_global() {
        let toml = r#"
            ai_model = "m"
            approvers = ["alice"]
            agent_kind = "acp-claude"

            [[repos]]
            repo = "owner/a"

            [[repos]]
            repo = "owner/b"
            agent_kind = "acp-gemini"
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(config.repos[0].agent_kind, AgentKind::AcpClaude);
        assert_eq!(config.repos[1].agent_kind, AgentKind::AcpGemini);
    }

    #[test]
    fn test_per_task_agent_kind_overrides_repo() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "m"
            approvers = ["alice"]
            agent_kind = "acp-claude"

            [spec]
            agent_kind = "acp-gemini"

            [implement]
            agent_kind = "acp-codex"
        "#;
        let config = parse_raw(toml).unwrap();
        let rc = config.first_repo();
        assert_eq!(rc.agent_kind, AgentKind::AcpClaude);
        assert_eq!(rc.agent_kind_for_task("spec"), AgentKind::AcpGemini);
        assert_eq!(rc.agent_kind_for_task("implement"), AgentKind::AcpCodex);
        assert_eq!(rc.agent_kind_for_task("review"), AgentKind::AcpClaude);
    }

    #[test]
    fn test_agents_block_overrides_defaults() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "m"
            approvers = ["alice"]

            [agents.acp_gemini]
            command = "/opt/custom/gemini-wrapper"
            args = ["--acp", "--verbose"]
        "#;
        let config = parse_raw(toml).unwrap();
        let gemini = config.agents.get(&AgentKind::AcpGemini).unwrap();
        assert_eq!(gemini.command, "/opt/custom/gemini-wrapper");
        assert_eq!(
            gemini.args,
            vec!["--acp".to_string(), "--verbose".to_string()]
        );
        // Other kinds still get their defaults.
        let claude = config.agents.get(&AgentKind::AcpClaude).unwrap();
        assert_eq!(claude.command, "claude-agent-acp");
    }

    #[test]
    fn test_missing_agents_block_uses_all_defaults() {
        let toml = r#"
            repo = "owner/repo"
            ai_model = "m"
            approvers = ["alice"]
        "#;
        let config = parse_raw(toml).unwrap();
        assert_eq!(
            config.agents.get(&AgentKind::AcpClaude).unwrap().command,
            "claude-agent-acp"
        );
        assert_eq!(
            config.agents.get(&AgentKind::AcpGemini).unwrap().command,
            "gemini"
        );
        assert_eq!(
            config.agents.get(&AgentKind::AcpCodex).unwrap().command,
            "codex-acp"
        );
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
        assert_eq!(
            config.repos[0].bypass_label.as_deref(),
            Some("hammurabi-bypass")
        );
    }

    // --- Discord source parsing tests ---

    #[test]
    fn test_discord_source_parses() {
        std::env::set_var("HAMMURABI_TEST_TOKEN", "bot-abc");
        let toml = r#"
            repo = "owner/hammurabi"
            ai_model = "claude-sonnet-4-6"
            approvers = ["hydai"]
            github_token = "ghp_test"

            [[sources]]
            kind = "discord"
            name = "intake"
            channel_id = "1234567890"
            repo = "owner/hammurabi"
            bot_token = "${HAMMURABI_TEST_TOKEN}"
            approvers = ["hydai"]
            allow_users = ["hydai"]
        "#;
        let config = parse_raw(toml).unwrap();
        std::env::remove_var("HAMMURABI_TEST_TOKEN");
        assert_eq!(config.sources.len(), 1);
        match &config.sources[0] {
            SourceEntry::Discord(d) => {
                assert_eq!(d.name, "intake");
                assert_eq!(d.channel_id, 1234567890);
                assert_eq!(d.repo, "owner/hammurabi");
                assert_eq!(d.bot_token, "bot-abc");
                assert_eq!(d.approvers, vec!["hydai".to_string()]);
                assert_eq!(d.command_prefix, "/");
                assert_eq!(d.max_draft_revisions, 5);
                assert!(!d.allow.is_allowed("eve"));
                assert!(d.allow.is_allowed("hydai"));
            }
        }
    }

    #[test]
    fn test_discord_source_rejects_unknown_repo() {
        let toml = r#"
            repo = "owner/hammurabi"
            ai_model = "claude-sonnet-4-6"
            approvers = ["hydai"]
            github_token = "ghp_test"

            [[sources]]
            kind = "discord"
            channel_id = "1"
            repo = "owner/other"
            bot_token = "tok"
            allow_users = ["hydai"]
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("owner/other"));
    }

    #[test]
    fn test_discord_source_allow_all_and_allow_users_conflicts() {
        let toml = r#"
            repo = "owner/hammurabi"
            ai_model = "claude-sonnet-4-6"
            approvers = ["hydai"]
            github_token = "ghp_test"

            [[sources]]
            kind = "discord"
            channel_id = "1"
            repo = "owner/hammurabi"
            bot_token = "tok"
            allow_all_users = true
            allow_users = ["hydai"]
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("allow_all_users"));
    }

    #[test]
    fn test_discord_source_duplicate_channel_rejected() {
        let toml = r#"
            repo = "owner/hammurabi"
            ai_model = "claude-sonnet-4-6"
            approvers = ["hydai"]
            github_token = "ghp_test"

            [[sources]]
            kind = "discord"
            channel_id = "111"
            repo = "owner/hammurabi"
            bot_token = "tok"
            allow_users = ["hydai"]

            [[sources]]
            kind = "discord"
            channel_id = "111"
            repo = "owner/hammurabi"
            bot_token = "tok"
            allow_users = ["hydai"]
        "#;
        let err = parse_raw(toml).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_discord_source_empty_approvers_inherits_from_repo() {
        let toml = r#"
            repo = "owner/hammurabi"
            ai_model = "claude-sonnet-4-6"
            approvers = ["hydai", "teammate"]
            github_token = "ghp_test"

            [[sources]]
            kind = "discord"
            channel_id = "1"
            repo = "owner/hammurabi"
            bot_token = "tok"
            allow_users = ["hydai"]
        "#;
        let config = parse_raw(toml).unwrap();
        match &config.sources[0] {
            SourceEntry::Discord(d) => {
                assert_eq!(
                    d.approvers,
                    vec!["hydai".to_string(), "teammate".to_string()]
                );
            }
        }
    }

    #[test]
    fn test_discord_debug_redacts_token() {
        let cfg = DiscordChannelConfig {
            name: "n".into(),
            channel_id: 1,
            repo: "o/r".into(),
            bot_token: "super-secret-token".into(),
            approvers: vec![],
            agent_kind: None,
            command_prefix: "/".into(),
            max_draft_revisions: 5,
            allow: AllowUsers::All,
        };
        let rendered = format!("{:?}", cfg);
        assert!(!rendered.contains("super-secret-token"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn test_no_sources_parses_as_empty_vec() {
        let toml = r#"
            repo = "owner/r"
            ai_model = "m"
            approvers = ["alice"]
            github_token = "ghp_test"
        "#;
        let config = parse_raw(toml).unwrap();
        assert!(config.sources.is_empty());
    }
}
