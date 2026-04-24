use std::fs;
use std::path::{Path, PathBuf};

use crate::error::HammurabiError;

pub struct LockFile {
    path: PathBuf,
}

impl LockFile {
    pub fn acquire(path: &Path) -> Result<Self, HammurabiError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(HammurabiError::Io)?;
        }

        if path.exists() {
            let content = fs::read_to_string(path).map_err(HammurabiError::Io)?;
            if let Ok(pid) = content.trim().parse::<u32>() {
                if is_process_running(pid) {
                    return Err(HammurabiError::Config(format!(
                        "another daemon instance is running (PID {}). Lock file: {}",
                        pid,
                        path.display()
                    )));
                }
            }
            // Stale lock file — overwrite
            tracing::warn!("removing stale lock file: {}", path.display());
        }

        let pid = std::process::id();
        fs::write(path, pid.to_string()).map_err(HammurabiError::Io)?;

        Ok(LockFile {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    // On Unix, sending signal 0 checks if the target process exists without
    // actually signaling it. `kill` returns 0 on success.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn is_process_running(_pid: u32) -> bool {
    // No portable liveness probe on Windows; treat every lock file as
    // stale. A daemon that crashed without cleanup gets its successor
    // started without friction; a live daemon on the same data dir would
    // be racing the new process regardless (Windows file locking would
    // make the SQLite open fail loudly).
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_acquire_and_release() {
        let tmp = env::temp_dir().join("hammurabi-lock-test.lock");
        let _ = fs::remove_file(&tmp);

        {
            let _lock = LockFile::acquire(&tmp).unwrap();
            assert!(tmp.exists());

            // Second acquire should fail
            let result = LockFile::acquire(&tmp);
            assert!(result.is_err());
        }

        // After drop, lock file should be removed
        assert!(!tmp.exists());
    }

    #[test]
    fn test_stale_lock_overwritten() {
        let tmp = env::temp_dir().join("hammurabi-lock-stale.lock");
        // Write a PID that doesn't exist
        fs::write(&tmp, "999999999").unwrap();

        let lock = LockFile::acquire(&tmp);
        assert!(lock.is_ok());

        let _ = fs::remove_file(&tmp);
    }
}
