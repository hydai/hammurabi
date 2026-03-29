use std::path::Path;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::config::HooksConfig;
use crate::error::HammurabiError;

/// Run a workspace lifecycle hook if configured.
/// Returns Ok(()) if no hook is configured or if the hook succeeds.
/// Returns Err on hook failure or timeout.
pub async fn run_hook(
    hook_name: &str,
    script: Option<&str>,
    workspace_path: &Path,
    timeout_secs: u64,
) -> Result<(), HammurabiError> {
    let script = match script {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };

    tracing::debug!(hook = hook_name, "Running workspace hook");

    let result = timeout(
        Duration::from_secs(timeout_secs),
        Command::new("sh")
            .arg("-c")
            .arg(script)
            .current_dir(workspace_path)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            if output.status.success() {
                tracing::debug!(hook = hook_name, "Hook completed successfully");
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(HammurabiError::Ai(format!(
                    "{} hook failed (exit {}): {}",
                    hook_name,
                    output.status,
                    stderr.chars().take(500).collect::<String>()
                )))
            }
        }
        Ok(Err(e)) => Err(HammurabiError::Ai(format!(
            "{} hook failed to execute: {}",
            hook_name, e
        ))),
        Err(_) => Err(HammurabiError::AiTimeout(format!(
            "{} hook timed out after {}s",
            hook_name, timeout_secs
        ))),
    }
}

/// Run a hook where failure is logged but not fatal.
pub async fn run_hook_best_effort(
    hook_name: &str,
    script: Option<&str>,
    workspace_path: &Path,
    timeout_secs: u64,
) {
    if let Err(e) = run_hook(hook_name, script, workspace_path, timeout_secs).await {
        tracing::warn!(hook = hook_name, "Hook failed (non-fatal): {}", e);
    }
}

/// Get the effective timeout for hooks.
pub fn hooks_timeout(hooks: &HooksConfig) -> u64 {
    hooks.timeout_secs.unwrap_or(60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_run_hook_none() {
        let result = run_hook("test", None, &PathBuf::from("/tmp"), 10).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_hook_empty() {
        let result = run_hook("test", Some(""), &PathBuf::from("/tmp"), 10).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_hook_success() {
        let result = run_hook("test", Some("true"), &PathBuf::from("/tmp"), 10).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_hook_failure() {
        let result = run_hook("test", Some("false"), &PathBuf::from("/tmp"), 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_hook_timeout() {
        let result = run_hook("test", Some("sleep 10"), &PathBuf::from("/tmp"), 1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_run_hook_best_effort_logs_failure() {
        // Should not panic or propagate error
        run_hook_best_effort("test", Some("false"), &PathBuf::from("/tmp"), 10).await;
    }
}
