use async_trait::async_trait;
use serde::Deserialize;

use crate::error::HammurabiError;

/// Format an octocrab error by extracting the actual source error.
///
/// octocrab's `Error::GitHub` variant has a broken `Display` impl (snafu default)
/// that just prints "GitHub". The real details are in `source()` -> `GitHubError`.
fn format_octocrab_error(err: &octocrab::Error) -> String {
    use std::error::Error;
    if let Some(source) = err.source() {
        source.to_string()
    } else {
        err.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub state: String,
    pub user_login: String,
}

#[derive(Debug, Clone)]
pub struct GitHubComment {
    pub id: u64,
    pub body: String,
    pub user_login: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrStatus {
    Open,
    Merged,
    ClosedWithoutMerge,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrFileContent {
    pub content: String,
}

#[async_trait]
pub trait GitHubClient: Send + Sync {
    async fn list_labeled_issues(&self, label: &str) -> Result<Vec<GitHubIssue>, HammurabiError>;
    async fn get_issue(&self, number: u64) -> Result<GitHubIssue, HammurabiError>;
    async fn get_issue_comments(
        &self,
        number: u64,
        since_id: Option<u64>,
    ) -> Result<Vec<GitHubComment>, HammurabiError>;
    async fn post_issue_comment(
        &self,
        number: u64,
        body: &str,
    ) -> Result<u64, HammurabiError>;
    async fn create_pull_request(
        &self,
        title: &str,
        head: &str,
        base: &str,
        body: &str,
    ) -> Result<u64, HammurabiError>;
    async fn get_pr_status(&self, pr_number: u64) -> Result<PrStatus, HammurabiError>;
    async fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<u64, HammurabiError>;
    async fn get_default_branch(&self) -> Result<String, HammurabiError>;
    async fn get_file_content(
        &self,
        branch: &str,
        path: &str,
    ) -> Result<String, HammurabiError>;
    async fn is_issue_open(&self, number: u64) -> Result<bool, HammurabiError>;
    async fn get_label_adder(
        &self,
        issue_number: u64,
        label: &str,
    ) -> Result<Option<String>, HammurabiError>;
}

pub struct OctocrabClient {
    client: octocrab::Octocrab,
    owner: String,
    repo: String,
    max_retries: u32,
}

impl OctocrabClient {
    pub fn new(
        auth: &crate::config::GitHubAuth,
        owner: &str,
        repo: &str,
        max_retries: u32,
    ) -> Result<Self, HammurabiError> {
        let client = match auth {
            crate::config::GitHubAuth::Token(token) => {
                octocrab::Octocrab::builder()
                    .personal_token(token.to_string())
                    .build()
                    .map_err(|e| HammurabiError::GitHub(format!(
                        "failed to create GitHub client: {}",
                        format_octocrab_error(&e)
                    )))?
            }
            crate::config::GitHubAuth::App {
                app_id,
                private_key_pem,
                installation_id,
            } => {
                let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem)
                    .map_err(|e| HammurabiError::Config(format!("invalid PEM key: {}", e)))?;
                let app_crab = octocrab::Octocrab::builder()
                    .app(
                        octocrab::models::AppId(*app_id),
                        key,
                    )
                    .build()
                    .map_err(|e| HammurabiError::GitHub(format!(
                        "failed to create GitHub App client: {}",
                        format_octocrab_error(&e)
                    )))?;
                app_crab
                    .installation(octocrab::models::InstallationId(*installation_id))
                    .map_err(|e| HammurabiError::GitHub(format!(
                        "failed to create installation client: {}",
                        format_octocrab_error(&e)
                    )))?
            }
        };

        Ok(Self {
            client,
            owner: owner.to_string(),
            repo: repo.to_string(),
            max_retries,
        })
    }

    async fn retry<F, Fut, T>(&self, operation: F) -> Result<T, HammurabiError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, HammurabiError>>,
    {
        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            match operation().await {
                Ok(val) => return Ok(val),
                Err(e) => {
                    if attempt < self.max_retries {
                        let delay = std::time::Duration::from_millis(500 * 2u64.pow(attempt));
                        tracing::warn!(
                            "GitHub API error (attempt {}/{}): {}. Retrying in {:?}...",
                            attempt + 1,
                            self.max_retries + 1,
                            e,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap())
    }
}

#[async_trait]
impl GitHubClient for OctocrabClient {
    async fn list_labeled_issues(&self, label: &str) -> Result<Vec<GitHubIssue>, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let label = label.to_string();
        let client = self.client.clone();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let label = label.clone();
            let client = client.clone();
            async move {
                let page = client
                    .issues(&owner, &repo)
                    .list()
                    .labels(&[label])
                    .state(octocrab::params::State::Open)
                    .per_page(100)
                    .send()
                    .await
                    .map_err(|e| HammurabiError::GitHub(format!("list issues: {}", format_octocrab_error(&e))))?;

                Ok(page
                    .items
                    .into_iter()
                    .filter(|i| i.pull_request.is_none())
                    .map(|i| GitHubIssue {
                        number: i.number,
                        title: i.title,
                        body: i.body.unwrap_or_default(),
                        labels: i.labels.iter().map(|l| l.name.clone()).collect(),
                        state: format!("{:?}", i.state),
                        user_login: i.user.login,
                    })
                    .collect())
            }
        })
        .await
    }

    async fn get_issue(&self, number: u64) -> Result<GitHubIssue, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            async move {
                let issue = client
                    .issues(&owner, &repo)
                    .get(number)
                    .await
                    .map_err(|e| HammurabiError::GitHub(format!("get issue #{}: {}", number, format_octocrab_error(&e))))?;

                Ok(GitHubIssue {
                    number: issue.number,
                    title: issue.title,
                    body: issue.body.unwrap_or_default(),
                    labels: issue.labels.iter().map(|l| l.name.clone()).collect(),
                    state: format!("{:?}", issue.state),
                    user_login: issue.user.login,
                })
            }
        })
        .await
    }

    async fn get_issue_comments(
        &self,
        number: u64,
        since_id: Option<u64>,
    ) -> Result<Vec<GitHubComment>, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            async move {
                let page = client
                    .issues(&owner, &repo)
                    .list_comments(number)
                    .per_page(100)
                    .send()
                    .await
                    .map_err(|e| {
                        HammurabiError::GitHub(format!("list comments for #{}: {}", number, format_octocrab_error(&e)))
                    })?;

                let comments: Vec<GitHubComment> = page
                    .items
                    .into_iter()
                    .filter(|c| {
                        if let Some(since) = since_id {
                            c.id.into_inner() > since
                        } else {
                            true
                        }
                    })
                    .map(|c| GitHubComment {
                        id: c.id.into_inner(),
                        body: c.body.unwrap_or_default(),
                        user_login: c.user.login,
                    })
                    .collect();

                Ok(comments)
            }
        })
        .await
    }

    async fn post_issue_comment(
        &self,
        number: u64,
        body: &str,
    ) -> Result<u64, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();
        let body = body.to_string();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            let body = body.clone();
            async move {
                let comment = client
                    .issues(&owner, &repo)
                    .create_comment(number, body)
                    .await
                    .map_err(|e| {
                        HammurabiError::GitHub(format!("post comment on #{}: {}", number, format_octocrab_error(&e)))
                    })?;

                Ok(comment.id.into_inner())
            }
        })
        .await
    }

    async fn create_pull_request(
        &self,
        title: &str,
        head: &str,
        base: &str,
        body: &str,
    ) -> Result<u64, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();
        let title = title.to_string();
        let head = head.to_string();
        let base = base.to_string();
        let body = body.to_string();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            let title = title.clone();
            let head = head.clone();
            let base = base.clone();
            let body = body.clone();
            async move {
                let pr = client
                    .pulls(&owner, &repo)
                    .create(&title, &head, &base)
                    .body(&body)
                    .send()
                    .await
                    .map_err(|e| HammurabiError::GitHub(format!("create PR: {}", format_octocrab_error(&e))))?;

                Ok(pr.number)
            }
        })
        .await
    }

    async fn get_pr_status(&self, pr_number: u64) -> Result<PrStatus, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            async move {
                let pr = client
                    .pulls(&owner, &repo)
                    .get(pr_number)
                    .await
                    .map_err(|e| {
                        HammurabiError::GitHub(format!("get PR #{}: {}", pr_number, format_octocrab_error(&e)))
                    })?;

                if pr.merged_at.is_some() {
                    Ok(PrStatus::Merged)
                } else if pr.state == Some(octocrab::models::IssueState::Closed) {
                    Ok(PrStatus::ClosedWithoutMerge)
                } else {
                    Ok(PrStatus::Open)
                }
            }
        })
        .await
    }

    async fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<u64, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();
        let title = title.to_string();
        let body = body.to_string();
        let labels: Vec<String> = labels.to_vec();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            let title = title.clone();
            let body = body.clone();
            let labels = labels.clone();
            async move {
                let issue = client
                    .issues(&owner, &repo)
                    .create(&title)
                    .body(&body)
                    .labels(labels)
                    .send()
                    .await
                    .map_err(|e| HammurabiError::GitHub(format!("create issue: {}", format_octocrab_error(&e))))?;

                Ok(issue.number)
            }
        })
        .await
    }

    async fn get_default_branch(&self) -> Result<String, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            async move {
                let repo_info = client
                    .repos(&owner, &repo)
                    .get()
                    .await
                    .map_err(|e| HammurabiError::GitHub(format!("get repo info: {}", format_octocrab_error(&e))))?;

                Ok(repo_info
                    .default_branch
                    .unwrap_or_else(|| "main".to_string()))
            }
        })
        .await
    }

    async fn get_file_content(
        &self,
        branch: &str,
        path: &str,
    ) -> Result<String, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();
        let branch = branch.to_string();
        let path = path.to_string();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            let branch = branch.clone();
            let path = path.clone();
            async move {
                let content = client
                    .repos(&owner, &repo)
                    .get_content()
                    .path(&path)
                    .r#ref(&branch)
                    .send()
                    .await
                    .map_err(|e| {
                        HammurabiError::GitHub(format!("get file {}: {}", path, format_octocrab_error(&e)))
                    })?;

                match content.items.into_iter().next() {
                    Some(item) => {
                        let decoded = item
                            .decoded_content()
                            .ok_or_else(|| {
                                HammurabiError::GitHub(format!("failed to decode {}", path))
                            })?;
                        Ok(decoded)
                    }
                    None => Err(HammurabiError::GitHub(format!("file not found: {}", path))),
                }
            }
        })
        .await
    }

    async fn is_issue_open(&self, number: u64) -> Result<bool, HammurabiError> {
        let issue = self.get_issue(number).await?;
        Ok(issue.state.contains("Open"))
    }

    async fn get_label_adder(
        &self,
        issue_number: u64,
        label: &str,
    ) -> Result<Option<String>, HammurabiError> {
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        let client = self.client.clone();
        let label = label.to_string();

        self.retry(|| {
            let owner = owner.clone();
            let repo = repo.clone();
            let client = client.clone();
            let label = label.clone();
            async move {
                let route = format!("/repos/{owner}/{repo}/issues/{issue_number}/events");
                let events: Vec<serde_json::Value> = client
                    .get(route, None::<&()>)
                    .await
                    .map_err(|e| {
                        HammurabiError::GitHub(format!(
                            "get events for #{}: {}",
                            issue_number, format_octocrab_error(&e)
                        ))
                    })?;

                // Find the most recent "labeled" event matching our label
                let mut adder: Option<String> = None;
                for event in &events {
                    if event.get("event").and_then(|v| v.as_str()) == Some("labeled") {
                        if let Some(label_obj) = event.get("label") {
                            if label_obj.get("name").and_then(|v| v.as_str()) == Some(&label) {
                                if let Some(actor) = event.get("actor") {
                                    if let Some(login) =
                                        actor.get("login").and_then(|v| v.as_str())
                                    {
                                        adder = Some(login.to_string());
                                    }
                                }
                            }
                        }
                    }
                }

                Ok(adder)
            }
        })
        .await
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub struct MockGitHubClient {
        pub issues: Mutex<Vec<GitHubIssue>>,
        pub comments: Mutex<HashMap<u64, Vec<GitHubComment>>>,
        pub pr_statuses: Mutex<HashMap<u64, PrStatus>>,
        pub created_comments: Mutex<Vec<(u64, String)>>,
        pub created_prs: Mutex<Vec<(String, String, String, String)>>,
        pub created_issues: Mutex<Vec<(String, String)>>,
        pub file_contents: Mutex<HashMap<(String, String), String>>,
        pub label_adders: Mutex<HashMap<(u64, String), String>>,
        pub next_comment_id: Mutex<u64>,
        pub next_pr_number: Mutex<u64>,
        pub next_issue_number: Mutex<u64>,
        pub default_branch: Mutex<String>,
    }

    impl MockGitHubClient {
        pub fn new() -> Self {
            Self {
                issues: Mutex::new(Vec::new()),
                comments: Mutex::new(HashMap::new()),
                pr_statuses: Mutex::new(HashMap::new()),
                created_comments: Mutex::new(Vec::new()),
                created_prs: Mutex::new(Vec::new()),
                created_issues: Mutex::new(Vec::new()),
                file_contents: Mutex::new(HashMap::new()),
                label_adders: Mutex::new(HashMap::new()),
                next_comment_id: Mutex::new(1000),
                next_pr_number: Mutex::new(100),
                next_issue_number: Mutex::new(200),
                default_branch: Mutex::new("main".to_string()),
            }
        }

        pub fn add_issue(&self, issue: GitHubIssue) {
            self.issues.lock().unwrap().push(issue);
        }

        pub fn add_comment(&self, issue_number: u64, comment: GitHubComment) {
            self.comments
                .lock()
                .unwrap()
                .entry(issue_number)
                .or_default()
                .push(comment);
        }

        pub fn set_pr_status(&self, pr_number: u64, status: PrStatus) {
            self.pr_statuses
                .lock()
                .unwrap()
                .insert(pr_number, status);
        }

        pub fn set_file_content(&self, branch: &str, path: &str, content: &str) {
            self.file_contents
                .lock()
                .unwrap()
                .insert((branch.to_string(), path.to_string()), content.to_string());
        }

        pub fn set_label_adder(&self, issue_number: u64, label: &str, user: &str) {
            self.label_adders
                .lock()
                .unwrap()
                .insert((issue_number, label.to_string()), user.to_string());
        }
    }

    #[async_trait]
    impl GitHubClient for MockGitHubClient {
        async fn list_labeled_issues(
            &self,
            label: &str,
        ) -> Result<Vec<GitHubIssue>, HammurabiError> {
            let issues = self.issues.lock().unwrap();
            Ok(issues
                .iter()
                .filter(|i| i.labels.contains(&label.to_string()) && i.state.contains("Open"))
                .cloned()
                .collect())
        }

        async fn get_issue(&self, number: u64) -> Result<GitHubIssue, HammurabiError> {
            let issues = self.issues.lock().unwrap();
            issues
                .iter()
                .find(|i| i.number == number)
                .cloned()
                .ok_or_else(|| HammurabiError::GitHub(format!("issue #{} not found", number)))
        }

        async fn get_issue_comments(
            &self,
            number: u64,
            since_id: Option<u64>,
        ) -> Result<Vec<GitHubComment>, HammurabiError> {
            let comments = self.comments.lock().unwrap();
            let issue_comments = comments.get(&number).cloned().unwrap_or_default();
            Ok(issue_comments
                .into_iter()
                .filter(|c| since_id.map_or(true, |id| c.id > id))
                .collect())
        }

        async fn post_issue_comment(
            &self,
            number: u64,
            body: &str,
        ) -> Result<u64, HammurabiError> {
            let mut next_id = self.next_comment_id.lock().unwrap();
            let id = *next_id;
            *next_id += 1;
            self.created_comments
                .lock()
                .unwrap()
                .push((number, body.to_string()));
            Ok(id)
        }

        async fn create_pull_request(
            &self,
            title: &str,
            head: &str,
            base: &str,
            body: &str,
        ) -> Result<u64, HammurabiError> {
            let mut next_pr = self.next_pr_number.lock().unwrap();
            let pr = *next_pr;
            *next_pr += 1;
            self.created_prs.lock().unwrap().push((
                title.to_string(),
                head.to_string(),
                base.to_string(),
                body.to_string(),
            ));
            self.pr_statuses
                .lock()
                .unwrap()
                .insert(pr, PrStatus::Open);
            Ok(pr)
        }

        async fn get_pr_status(&self, pr_number: u64) -> Result<PrStatus, HammurabiError> {
            let statuses = self.pr_statuses.lock().unwrap();
            statuses
                .get(&pr_number)
                .copied()
                .ok_or_else(|| HammurabiError::GitHub(format!("PR #{} not found", pr_number)))
        }

        async fn create_issue(
            &self,
            title: &str,
            body: &str,
            _labels: &[String],
        ) -> Result<u64, HammurabiError> {
            let mut next_issue = self.next_issue_number.lock().unwrap();
            let number = *next_issue;
            *next_issue += 1;
            self.created_issues
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
            Ok(number)
        }

        async fn get_default_branch(&self) -> Result<String, HammurabiError> {
            Ok(self.default_branch.lock().unwrap().clone())
        }

        async fn get_file_content(
            &self,
            branch: &str,
            path: &str,
        ) -> Result<String, HammurabiError> {
            let contents = self.file_contents.lock().unwrap();
            contents
                .get(&(branch.to_string(), path.to_string()))
                .cloned()
                .ok_or_else(|| {
                    HammurabiError::GitHub(format!("file not found: {}:{}", branch, path))
                })
        }

        async fn is_issue_open(&self, number: u64) -> Result<bool, HammurabiError> {
            let issues = self.issues.lock().unwrap();
            Ok(issues
                .iter()
                .any(|i| i.number == number && i.state.contains("Open")))
        }

        async fn get_label_adder(
            &self,
            issue_number: u64,
            label: &str,
        ) -> Result<Option<String>, HammurabiError> {
            let adders = self.label_adders.lock().unwrap();
            Ok(adders
                .get(&(issue_number, label.to_string()))
                .cloned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::*;
    use super::*;

    #[tokio::test]
    async fn test_list_labeled_issues() {
        let client = MockGitHubClient::new();
        client.add_issue(GitHubIssue {
            number: 1,
            title: "Feature A".to_string(),
            body: "Do something".to_string(),
            labels: vec!["hammurabi".to_string()],
            state: "Open".to_string(),
            user_login: "alice".to_string(),
        });
        client.add_issue(GitHubIssue {
            number: 2,
            title: "Feature B".to_string(),
            body: "Other".to_string(),
            labels: vec!["bug".to_string()],
            state: "Open".to_string(),
            user_login: "bob".to_string(),
        });

        let issues = client.list_labeled_issues("hammurabi").await.unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 1);
    }

    #[tokio::test]
    async fn test_post_and_get_comments() {
        let client = MockGitHubClient::new();
        client.add_comment(
            1,
            GitHubComment {
                id: 100,
                body: "first comment".to_string(),
                user_login: "alice".to_string(),
            },
        );
        client.add_comment(
            1,
            GitHubComment {
                id: 101,
                body: "/approve".to_string(),
                user_login: "bob".to_string(),
            },
        );

        let all = client.get_issue_comments(1, None).await.unwrap();
        assert_eq!(all.len(), 2);

        let since = client.get_issue_comments(1, Some(100)).await.unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].id, 101);
    }

    #[tokio::test]
    async fn test_pr_status() {
        let client = MockGitHubClient::new();
        let pr = client
            .create_pull_request("Test PR", "feature", "main", "body")
            .await
            .unwrap();

        assert_eq!(client.get_pr_status(pr).await.unwrap(), PrStatus::Open);

        client.set_pr_status(pr, PrStatus::Merged);
        assert_eq!(client.get_pr_status(pr).await.unwrap(), PrStatus::Merged);
    }

    #[tokio::test]
    async fn test_create_issue() {
        let client = MockGitHubClient::new();
        let num = client
            .create_issue("Sub task", "Do this", &["hammurabi".to_string()])
            .await
            .unwrap();
        assert!(num > 0);

        let created = client.created_issues.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "Sub task");
    }

    #[tokio::test]
    async fn test_file_content() {
        let client = MockGitHubClient::new();
        client.set_file_content("feature-branch", "SPEC.md", "# Spec content");

        let content = client
            .get_file_content("feature-branch", "SPEC.md")
            .await
            .unwrap();
        assert_eq!(content, "# Spec content");

        let err = client
            .get_file_content("main", "nonexistent.md")
            .await;
        assert!(err.is_err());
    }
}
