use async_trait::async_trait;
use std::path::{Path, PathBuf};

use crate::error::HammurabiError;

#[async_trait]
pub trait WorktreeManager: Send + Sync {
    async fn ensure_bare_clone(&self, repo_url: &str) -> Result<PathBuf, HammurabiError>;
    async fn fetch_origin(&self) -> Result<(), HammurabiError>;
    async fn create_worktree(
        &self,
        issue_number: u64,
        task_name: &str,
        base_branch: &str,
    ) -> Result<PathBuf, HammurabiError>;
    async fn remove_worktree(&self, path: &Path) -> Result<(), HammurabiError>;
    async fn push_branch(&self, branch_name: &str) -> Result<(), HammurabiError>;
    async fn delete_remote_branch(&self, branch_name: &str) -> Result<(), HammurabiError>;
    async fn seed_file(&self, worktree_path: &Path, filename: &str, content: &str) -> Result<(), HammurabiError>;
}

pub struct GitWorktreeManager {
    base_dir: PathBuf,
    bare_clone_path: PathBuf,
    worktrees_dir: PathBuf,
    github_token: String,
}

impl GitWorktreeManager {
    pub fn new(base_dir: PathBuf, github_token: String) -> Self {
        let bare_clone_path = base_dir.join("repo");
        let worktrees_dir = base_dir.join("worktrees");
        Self {
            base_dir,
            bare_clone_path,
            worktrees_dir,
            github_token,
        }
    }

    fn branch_name(issue_number: u64, task_name: &str) -> String {
        format!("hammurabi/{}-{}", issue_number, task_name)
    }

    fn worktree_dir_name(issue_number: u64, task_name: &str) -> String {
        format!("{}-{}", issue_number, task_name)
    }

    async fn run_git(
        &self,
        args: &[&str],
        cwd: &Path,
    ) -> Result<String, HammurabiError> {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .await
            .map_err(|e| HammurabiError::Worktree(format!("failed to run git: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HammurabiError::Worktree(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run a git command with GIT_ASKPASS-based authentication.
    /// This avoids embedding the token in command-line arguments or URLs.
    async fn run_git_authenticated(
        &self,
        args: &[&str],
        cwd: &Path,
    ) -> Result<String, HammurabiError> {
        let askpass_path = self.base_dir.join(".git-askpass.sh");
        let script_content = format!("#!/bin/sh\necho '{}'", self.github_token);
        tokio::fs::write(&askpass_path, &script_content)
            .await
            .map_err(|e| HammurabiError::Worktree(format!("write askpass script: {}", e)))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            tokio::fs::set_permissions(&askpass_path, perms)
                .await
                .map_err(|e| HammurabiError::Worktree(format!("chmod askpass: {}", e)))?;
        }

        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_ASKPASS", &askpass_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .await
            .map_err(|e| HammurabiError::Worktree(format!("failed to run git: {}", e)))?;

        // Clean up askpass script
        let _ = tokio::fs::remove_file(&askpass_path).await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HammurabiError::Worktree(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[async_trait]
impl WorktreeManager for GitWorktreeManager {
    async fn ensure_bare_clone(&self, repo_url: &str) -> Result<PathBuf, HammurabiError> {
        tokio::fs::create_dir_all(&self.base_dir)
            .await
            .map_err(|e| HammurabiError::Worktree(format!("create base dir: {}", e)))?;

        if self.bare_clone_path.exists() {
            return Ok(self.bare_clone_path.clone());
        }

        self.run_git_authenticated(
            &["clone", "--bare", repo_url, "repo"],
            &self.base_dir,
        )
        .await?;

        Ok(self.bare_clone_path.clone())
    }

    async fn fetch_origin(&self) -> Result<(), HammurabiError> {
        self.run_git_authenticated(
            &["fetch", "origin", "--prune"],
            &self.bare_clone_path,
        )
        .await?;
        Ok(())
    }

    async fn create_worktree(
        &self,
        issue_number: u64,
        task_name: &str,
        base_branch: &str,
    ) -> Result<PathBuf, HammurabiError> {
        tokio::fs::create_dir_all(&self.worktrees_dir)
            .await
            .map_err(|e| HammurabiError::Worktree(format!("create worktrees dir: {}", e)))?;

        let dir_name = Self::worktree_dir_name(issue_number, task_name);
        let worktree_path = self.worktrees_dir.join(&dir_name);
        let branch = Self::branch_name(issue_number, task_name);

        // Remove stale worktree if exists
        if worktree_path.exists() {
            let _ = self
                .run_git(
                    &["worktree", "remove", "--force", worktree_path.to_str().unwrap()],
                    &self.bare_clone_path,
                )
                .await;
            if worktree_path.exists() {
                tokio::fs::remove_dir_all(&worktree_path)
                    .await
                    .map_err(|e| {
                        HammurabiError::Worktree(format!("remove stale worktree: {}", e))
                    })?;
            }
        }

        // Delete local branch if exists
        let _ = self
            .run_git(&["branch", "-D", &branch], &self.bare_clone_path)
            .await;

        // Create worktree with new branch based on the remote base branch
        let base_ref = format!("origin/{}", base_branch);
        self.run_git(
            &[
                "worktree",
                "add",
                worktree_path.to_str().unwrap(),
                "-b",
                &branch,
                &base_ref,
            ],
            &self.bare_clone_path,
        )
        .await?;

        Ok(worktree_path)
    }

    async fn remove_worktree(&self, path: &Path) -> Result<(), HammurabiError> {
        if !path.exists() {
            return Ok(());
        }

        let _ = self
            .run_git(
                &["worktree", "remove", "--force", path.to_str().unwrap()],
                &self.bare_clone_path,
            )
            .await;

        // Force remove if git worktree remove didn't work
        if path.exists() {
            tokio::fs::remove_dir_all(path)
                .await
                .map_err(|e| HammurabiError::Worktree(format!("remove dir: {}", e)))?;
        }

        // Prune stale worktree entries
        let _ = self
            .run_git(&["worktree", "prune"], &self.bare_clone_path)
            .await;

        Ok(())
    }

    async fn push_branch(&self, branch_name: &str) -> Result<(), HammurabiError> {
        // Delete remote branch first if it exists (daemon-managed branches)
        let _ = self.delete_remote_branch(branch_name).await;

        self.run_git_authenticated(
            &["push", "origin", branch_name],
            &self.bare_clone_path,
        )
        .await?;
        Ok(())
    }

    async fn delete_remote_branch(&self, branch_name: &str) -> Result<(), HammurabiError> {
        let delete_ref = format!(":{}", branch_name);
        let _ = self
            .run_git_authenticated(
                &["push", "origin", &delete_ref],
                &self.bare_clone_path,
            )
            .await;
        Ok(())
    }

    async fn seed_file(
        &self,
        worktree_path: &Path,
        filename: &str,
        content: &str,
    ) -> Result<(), HammurabiError> {
        let file_path = worktree_path.join(filename);
        tokio::fs::write(&file_path, content)
            .await
            .map_err(|e| HammurabiError::Worktree(format!("seed {}: {}", filename, e)))?;
        Ok(())
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    pub struct MockWorktreeManager {
        pub created_worktrees: Mutex<Vec<(u64, String, String)>>,
        pub removed_worktrees: Mutex<Vec<PathBuf>>,
        pub pushed_branches: Mutex<Vec<String>>,
        pub seeded_files: Mutex<Vec<(PathBuf, String, String)>>,
        pub worktree_base: PathBuf,
    }

    impl MockWorktreeManager {
        pub fn new(base: PathBuf) -> Self {
            Self {
                created_worktrees: Mutex::new(Vec::new()),
                removed_worktrees: Mutex::new(Vec::new()),
                pushed_branches: Mutex::new(Vec::new()),
                seeded_files: Mutex::new(Vec::new()),
                worktree_base: base,
            }
        }
    }

    #[async_trait]
    impl WorktreeManager for MockWorktreeManager {
        async fn ensure_bare_clone(&self, _repo_url: &str) -> Result<PathBuf, HammurabiError> {
            Ok(self.worktree_base.join("repo"))
        }

        async fn fetch_origin(&self) -> Result<(), HammurabiError> {
            Ok(())
        }

        async fn create_worktree(
            &self,
            issue_number: u64,
            task_name: &str,
            base_branch: &str,
        ) -> Result<PathBuf, HammurabiError> {
            let path = self
                .worktree_base
                .join("worktrees")
                .join(format!("{}-{}", issue_number, task_name));

            tokio::fs::create_dir_all(&path)
                .await
                .map_err(|e| HammurabiError::Worktree(e.to_string()))?;

            self.created_worktrees.lock().unwrap().push((
                issue_number,
                task_name.to_string(),
                base_branch.to_string(),
            ));

            Ok(path)
        }

        async fn remove_worktree(&self, path: &Path) -> Result<(), HammurabiError> {
            self.removed_worktrees
                .lock()
                .unwrap()
                .push(path.to_path_buf());
            Ok(())
        }

        async fn push_branch(&self, branch_name: &str) -> Result<(), HammurabiError> {
            self.pushed_branches
                .lock()
                .unwrap()
                .push(branch_name.to_string());
            Ok(())
        }

        async fn delete_remote_branch(&self, _branch_name: &str) -> Result<(), HammurabiError> {
            Ok(())
        }

        async fn seed_file(
            &self,
            worktree_path: &Path,
            filename: &str,
            content: &str,
        ) -> Result<(), HammurabiError> {
            self.seeded_files.lock().unwrap().push((
                worktree_path.to_path_buf(),
                filename.to_string(),
                content.to_string(),
            ));
            let file_path = worktree_path.join(filename);
            tokio::fs::write(&file_path, content)
                .await
                .map_err(|e| HammurabiError::Worktree(e.to_string()))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_naming() {
        assert_eq!(
            GitWorktreeManager::branch_name(42, "spec"),
            "hammurabi/42-spec"
        );
        assert_eq!(
            GitWorktreeManager::branch_name(42, "sub1"),
            "hammurabi/42-sub1"
        );
    }

    #[test]
    fn test_worktree_dir_naming() {
        assert_eq!(
            GitWorktreeManager::worktree_dir_name(42, "spec"),
            "42-spec"
        );
    }

    #[tokio::test]
    async fn test_mock_worktree_create() {
        let tmp = std::env::temp_dir().join("hammurabi-test-wt");
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        let mgr = mock::MockWorktreeManager::new(tmp.clone());

        let path = mgr.create_worktree(42, "spec", "main").await.unwrap();
        assert!(path.exists());
        assert!(path.to_str().unwrap().contains("42-spec"));

        let created = mgr.created_worktrees.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0], (42, "spec".to_string(), "main".to_string()));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_mock_seed_file() {
        let tmp = std::env::temp_dir().join("hammurabi-test-seed");
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        let mgr = mock::MockWorktreeManager::new(tmp.clone());

        let path = mgr.create_worktree(1, "spec", "main").await.unwrap();
        mgr.seed_file(&path, "CLAUDE.md", "# Context\nTest").await.unwrap();

        let content = tokio::fs::read_to_string(path.join("CLAUDE.md"))
            .await
            .unwrap();
        assert_eq!(content, "# Context\nTest");

        let seeded = mgr.seeded_files.lock().unwrap();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].1, "CLAUDE.md");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
