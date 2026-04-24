//! Cross-platform helpers for spawning an ACP agent subprocess and
//! cleaning it up on drop.
//!
//! On Unix we place the child in its own process group (via `setpgid` in
//! the child-init callback) so we can tear down the entire subtree with a
//! single `kill(-pgid, ...)` even if the agent spawns helpers. On Windows
//! we fall back to killing the direct child only — the OS will orphan any
//! grandchildren.

use std::collections::HashMap;
use std::time::Duration;
use tokio::process::{Child, Command};

use crate::error::HammurabiError;

/// Spawn a subprocess configured for ACP stdio and (on Unix) process-group
/// management. Returns the `Child` and its PGID (on Unix).
pub fn spawn_child(
    command: &str,
    args: &[String],
    working_dir: &str,
    env: &HashMap<String, String>,
) -> Result<(Child, Option<i32>), HammurabiError> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Swallow stderr — spec-compliant agents keep protocol on stdout.
        // If debugging, set HAMMURABI_ACP_STDERR=1 to inherit.
        .stderr(if std::env::var("HAMMURABI_ACP_STDERR").is_ok() {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        })
        .current_dir(working_dir);

    for (k, v) in env {
        cmd.env(k, expand_env_refs(v));
    }

    #[cfg(unix)]
    install_unix_child_init(&mut cmd);

    let child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            HammurabiError::Config(format!("ACP agent binary '{command}' not found on PATH"))
        }
        _ => HammurabiError::Io(e),
    })?;

    #[cfg(unix)]
    let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
    #[cfg(not(unix))]
    let pgid: Option<i32> = None;

    Ok((child, pgid))
}

/// Expand `${VAR}` references in an `[agents.*].env` value at spawn time.
/// Delegates to [`crate::env_expand::expand_str`] so the syntax matches
/// every other secret-bearing field in the config: `${VAR}` anywhere in
/// the string, `$$` escapes a literal `$`, unknown vars resolve to empty.
fn expand_env_refs(value: &str) -> String {
    crate::env_expand::expand_str(value)
}

/// Kill a process subtree previously set up via [`spawn_child`].
///
/// On Unix: SIGTERM to the process group, then SIGKILL ~1.5s later. We use
/// a detached `std::thread` (not `tokio::spawn`) so the follow-up SIGKILL
/// fires even if the tokio runtime is shutting down.
///
/// On Windows: best-effort no-op here; callers should `Child::start_kill`
/// on the direct child in their drop path.
pub fn kill_subtree(pgid: Option<i32>) {
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        if pgid > 0 {
            unix_kill_subtree(pgid);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

#[cfg(unix)]
fn install_unix_child_init(cmd: &mut Command) {
    // SAFETY: `setpgid(0, 0)` is POSIX async-signal-safe. We check the
    // return value; on failure we propagate the error, which aborts the
    // child's setup (spawn will fail) so we never reach a state where
    // `kill(-pgid, ...)` would target the wrong group.
    let child_init = || -> std::io::Result<()> {
        let rc = unsafe { libc::setpgid(0, 0) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    // Tokio's `tokio::process::Command` exposes an inherent `pre_exec`
    // (unsafe) that forwards to the underlying `std::process::Command`.
    // No `CommandExt` import needed.
    unsafe {
        cmd.pre_exec(child_init);
    }
}

#[cfg(unix)]
fn unix_kill_subtree(pgid: i32) {
    // SAFETY: negative pid targets the whole process group; signal codes
    // are well-defined constants.
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::expand_env_refs;

    #[test]
    fn expands_whole_value_ref() {
        std::env::set_var("HAMMURABI_TEST_VAR", "abc");
        assert_eq!(expand_env_refs("${HAMMURABI_TEST_VAR}"), "abc");
        std::env::remove_var("HAMMURABI_TEST_VAR");
    }

    #[test]
    fn missing_var_becomes_empty() {
        std::env::remove_var("HAMMURABI_DEFINITELY_UNSET_XYZ");
        assert_eq!(expand_env_refs("${HAMMURABI_DEFINITELY_UNSET_XYZ}"), "");
    }

    #[test]
    fn non_placeholder_values_pass_through() {
        assert_eq!(expand_env_refs("plain"), "plain");
    }

    #[test]
    fn expands_partial_refs() {
        std::env::set_var("HAMMURABI_TEST_PREFIX", "xyz");
        assert_eq!(
            expand_env_refs("prefix-${HAMMURABI_TEST_PREFIX}"),
            "prefix-xyz"
        );
        std::env::remove_var("HAMMURABI_TEST_PREFIX");
    }

    #[test]
    fn dollar_escapes_itself() {
        assert_eq!(expand_env_refs("$$literal"), "$literal");
    }
}
