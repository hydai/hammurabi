use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::HammurabiError;

#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn get_token(&self) -> Result<String, HammurabiError>;
}

pub struct StaticTokenProvider {
    token: String,
}

impl StaticTokenProvider {
    pub fn new(token: String) -> Self {
        Self { token }
    }
}

#[async_trait]
impl TokenProvider for StaticTokenProvider {
    async fn get_token(&self) -> Result<String, HammurabiError> {
        Ok(self.token.clone())
    }
}

pub struct AppTokenProvider {
    app_client: octocrab::Octocrab,
    installation_id: octocrab::models::InstallationId,
}

impl AppTokenProvider {
    pub fn new(app_client: octocrab::Octocrab, installation_id: octocrab::models::InstallationId) -> Self {
        Self {
            app_client,
            installation_id,
        }
    }
}

#[async_trait]
impl TokenProvider for AppTokenProvider {
    async fn get_token(&self) -> Result<String, HammurabiError> {
        use secrecy::ExposeSecret;
        let (_crab, token) = self
            .app_client
            .installation_and_token(self.installation_id)
            .await
            .map_err(|e| {
                HammurabiError::GitHub(format!("failed to get installation token: {}", e))
            })?;
        Ok(token.expose_secret().to_string())
    }
}

#[async_trait]
pub trait WorktreeManager: Send + Sync {
    async fn ensure_bare_clone(&self, repo_url: &str) -> Result<PathBuf, HammurabiError>;
    async fn ensure_default_branch(&self, default_branch: &str) -> Result<(), HammurabiError>;
    async fn fetch_origin(&self) -> Result<(), HammurabiError>;
    async fn create_worktree(
        &self,
        issue_number: u64,
        task_name: &str,
        base_branch: &str,
    ) -> Result<PathBuf, HammurabiError>;
    async fn remove_worktree(&self, path: &Path) -> Result<(), HammurabiError>;
    async fn commit_all_changes(
        &self,
        worktree_path: &Path,
        message: &str,
    ) -> Result<bool, HammurabiError>;
    async fn push_branch(&self, branch_name: &str) -> Result<(), HammurabiError>;
    async fn delete_remote_branch(&self, branch_name: &str) -> Result<(), HammurabiError>;
    async fn seed_file(&self, worktree_path: &Path, filename: &str, content: &str) -> Result<(), HammurabiError>;
}

pub struct GitWorktreeManager {
    base_dir: PathBuf,
    bare_clone_path: PathBuf,
    worktrees_dir: PathBuf,
    token_provider: Arc<dyn TokenProvider>,
}

impl GitWorktreeManager {
    pub fn new(base_dir: PathBuf, token_provider: Arc<dyn TokenProvider>) -> Self {
        // Ensure absolute paths so git commands with different cwd values
        // always resolve paths correctly.
        let base_dir = if base_dir.is_relative() {
            std::env::current_dir()
                .expect("failed to get current directory")
                .join(&base_dir)
        } else {
            base_dir
        };
        let bare_clone_path = base_dir.join("repo");
        let worktrees_dir = base_dir.join("worktrees");
        Self {
            base_dir,
            bare_clone_path,
            worktrees_dir,
            token_provider,
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
        let token = self.token_provider.get_token().await?;
        let askpass_path = self.base_dir.join(".git-askpass.sh");
        let script_content = format!("#!/bin/sh\necho '{}'", token);
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

        // Bare clones default to refspec +refs/heads/*:refs/heads/*, so
        // "origin/main" doesn't resolve.  Reconfigure to the standard
        // remote-tracking layout and fetch so create_worktree can use
        // origin/<branch> as the start point.
        self.run_git(
            &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"],
            &self.bare_clone_path,
        )
        .await?;

        self.run_git_authenticated(
            &["fetch", "origin"],
            &self.bare_clone_path,
        )
        .await?;

        Ok(self.bare_clone_path.clone())
    }

    async fn ensure_default_branch(&self, default_branch: &str) -> Result<(), HammurabiError> {
        let remote_ref = format!("origin/{}", default_branch);
        let exists = self
            .run_git(&["rev-parse", "--verify", &remote_ref], &self.bare_clone_path)
            .await;

        if exists.is_ok() {
            return Ok(());
        }

        tracing::info!(
            branch = default_branch,
            "Remote default branch has no commits, creating initial commit"
        );

        // Create a temporary worktree for the initial commit
        let init_path = self.worktrees_dir.join("_init");
        tokio::fs::create_dir_all(&self.worktrees_dir)
            .await
            .map_err(|e| HammurabiError::Worktree(format!("create worktrees dir: {}", e)))?;

        // Clean up any stale init worktree
        if init_path.exists() {
            let _ = self
                .run_git(
                    &["worktree", "remove", "--force", init_path.to_str().unwrap()],
                    &self.bare_clone_path,
                )
                .await;
            if init_path.exists() {
                let _ = tokio::fs::remove_dir_all(&init_path).await;
            }
            let _ = self
                .run_git(&["worktree", "prune"], &self.bare_clone_path)
                .await;
        }

        // Create orphan worktree
        self.run_git(
            &[
                "worktree", "add", "--detach",
                init_path.to_str().unwrap(),
            ],
            &self.bare_clone_path,
        )
        .await?;

        // Create orphan branch with initial commit
        self.run_git(
            &["checkout", "--orphan", default_branch],
            &init_path,
        )
        .await?;

        self.run_git(
            &[
                "-c", "user.name=Hammurabi",
                "-c", "user.email=hammurabi@noreply",
                "commit", "--allow-empty", "-m", "chore: initial commit",
            ],
            &init_path,
        )
        .await?;

        self.run_git_authenticated(
            &["push", "origin", default_branch],
            &init_path,
        )
        .await?;

        // Clean up
        let _ = self
            .run_git(
                &["worktree", "remove", "--force", init_path.to_str().unwrap()],
                &self.bare_clone_path,
            )
            .await;
        let _ = self
            .run_git(&["worktree", "prune"], &self.bare_clone_path)
            .await;

        // Fetch so origin/<default_branch> is available
        self.run_git_authenticated(
            &["fetch", "origin"],
            &self.bare_clone_path,
        )
        .await?;

        Ok(())
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

    async fn commit_all_changes(
        &self,
        worktree_path: &Path,
        message: &str,
    ) -> Result<bool, HammurabiError> {
        // Stage all changes
        self.run_git(&["add", "-A"], worktree_path).await?;

        // Try to commit; git exits non-zero if nothing to commit
        let result = self
            .run_git(
                &[
                    "-c", "user.name=Hammurabi",
                    "-c", "user.email=hammurabi@noreply",
                    "commit", "-m", message,
                ],
                worktree_path,
            )
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(e) => {
                let msg = format!("{}", e);
                if msg.contains("nothing to commit") {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
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

        async fn ensure_default_branch(&self, _default_branch: &str) -> Result<(), HammurabiError> {
            Ok(())
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

        async fn commit_all_changes(
            &self,
            _worktree_path: &Path,
            _message: &str,
        ) -> Result<bool, HammurabiError> {
            Ok(true)
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

    #[test]
    fn test_relative_base_dir_becomes_absolute() {
        let mgr = GitWorktreeManager::new(
            PathBuf::from(".hammurabi"),
            Arc::new(StaticTokenProvider::new("token".to_string())),
        );
        assert!(mgr.base_dir.is_absolute(), "base_dir should be absolute");
        assert!(mgr.bare_clone_path.is_absolute(), "bare_clone_path should be absolute");
        assert!(mgr.worktrees_dir.is_absolute(), "worktrees_dir should be absolute");
        assert!(mgr.base_dir.ends_with(".hammurabi"));
        assert!(mgr.bare_clone_path.ends_with(".hammurabi/repo"));
        assert!(mgr.worktrees_dir.ends_with(".hammurabi/worktrees"));
    }

    #[test]
    fn test_absolute_base_dir_stays_absolute() {
        let mgr = GitWorktreeManager::new(
            PathBuf::from("/tmp/hammurabi-test"),
            Arc::new(StaticTokenProvider::new("token".to_string())),
        );
        assert_eq!(mgr.base_dir, PathBuf::from("/tmp/hammurabi-test"));
        assert_eq!(mgr.bare_clone_path, PathBuf::from("/tmp/hammurabi-test/repo"));
        assert_eq!(mgr.worktrees_dir, PathBuf::from("/tmp/hammurabi-test/worktrees"));
    }

    /// Creates a temp git repo with one commit, returning its path.
    async fn create_temp_repo(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(name);
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        tokio::fs::create_dir_all(&tmp).await.unwrap();

        // git init + first commit so there's a main branch
        tokio::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&tmp)
            .output().await.unwrap();
        tokio::fs::write(tmp.join("README.md"), "# test").await.unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp)
            .output().await.unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&tmp)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output().await.unwrap();

        tmp
    }

    #[tokio::test]
    async fn test_bare_clone_lands_in_correct_directory() {
        let origin = create_temp_repo("hammurabi-test-origin-clone").await;
        let base = std::env::temp_dir().join("hammurabi-test-base-clone");
        let _ = tokio::fs::remove_dir_all(&base).await;

        let mgr = GitWorktreeManager::new(base.clone(), Arc::new(StaticTokenProvider::new("unused".to_string())));
        let clone_path = mgr
            .ensure_bare_clone(origin.to_str().unwrap())
            .await
            .unwrap();

        // Clone should be at <base>/repo, not nested
        assert_eq!(clone_path, base.join("repo"));
        assert!(base.join("repo").exists(), "repo dir should exist at base/repo");
        assert!(base.join("repo/HEAD").exists(), "should be a valid bare git repo");
        assert!(
            !base.join("repo/.hammurabi").exists(),
            "should NOT have nested .hammurabi inside repo"
        );

        let _ = tokio::fs::remove_dir_all(&base).await;
        let _ = tokio::fs::remove_dir_all(&origin).await;
    }

    #[tokio::test]
    async fn test_bare_clone_has_origin_refs_after_setup() {
        let origin = create_temp_repo("hammurabi-test-origin-refs").await;
        let base = std::env::temp_dir().join("hammurabi-test-base-refs");
        let _ = tokio::fs::remove_dir_all(&base).await;

        let mgr = GitWorktreeManager::new(base.clone(), Arc::new(StaticTokenProvider::new("unused".to_string())));
        mgr.ensure_bare_clone(origin.to_str().unwrap()).await.unwrap();

        // origin/main should resolve after the refspec reconfiguration
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", "--verify", "origin/main"])
            .current_dir(base.join("repo"))
            .output().await.unwrap();
        assert!(
            output.status.success(),
            "origin/main should resolve in bare clone; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = tokio::fs::remove_dir_all(&base).await;
        let _ = tokio::fs::remove_dir_all(&origin).await;
    }

    #[tokio::test]
    async fn test_worktree_created_at_correct_path_and_seedable() {
        let origin = create_temp_repo("hammurabi-test-origin-wt").await;
        let base = std::env::temp_dir().join("hammurabi-test-base-wt");
        let _ = tokio::fs::remove_dir_all(&base).await;

        let mgr = GitWorktreeManager::new(base.clone(), Arc::new(StaticTokenProvider::new("unused".to_string())));
        mgr.ensure_bare_clone(origin.to_str().unwrap()).await.unwrap();

        let wt_path = mgr.create_worktree(7, "spec", "main").await.unwrap();

        // Worktree should be at <base>/worktrees/7-spec
        assert_eq!(wt_path, base.join("worktrees/7-spec"));
        assert!(wt_path.exists(), "worktree directory should exist");
        assert!(
            wt_path.join("README.md").exists(),
            "worktree should contain files from main branch"
        );

        // Seeding a file should succeed
        mgr.seed_file(&wt_path, "CLAUDE.md", "# Test context").await.unwrap();
        let content = tokio::fs::read_to_string(wt_path.join("CLAUDE.md")).await.unwrap();
        assert_eq!(content, "# Test context");

        let _ = tokio::fs::remove_dir_all(&base).await;
        let _ = tokio::fs::remove_dir_all(&origin).await;
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
