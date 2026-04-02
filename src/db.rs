use rusqlite::{params, Connection, Result as SqlResult};
use std::sync::Mutex;

use crate::error::HammurabiError;
use crate::models::{IssueState, TrackedIssue, UsageEntry};

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &str) -> Result<Self, HammurabiError> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open(path)
        }
        .map_err(|e| HammurabiError::Database(e.to_string()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let db = Database {
            conn: Mutex::new(conn),
        };
        db.run_migrations()?;
        Ok(db)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    fn run_migrations(&self) -> Result<(), HammurabiError> {
        let conn = self.conn();

        // Check if we need to migrate from old schema
        let has_old_schema = conn
            .prepare("SELECT spec_pr_number FROM issues LIMIT 0")
            .is_ok();
        let has_new_schema = conn
            .prepare("SELECT spec_comment_id FROM issues LIMIT 0")
            .is_ok();

        if has_old_schema && !has_new_schema {
            // Migrate from old schema: rename old table, create new, copy data
            conn.execute_batch(
                "ALTER TABLE issues RENAME TO issues_old;

                CREATE TABLE issues (
                    id INTEGER PRIMARY KEY,
                    repo TEXT NOT NULL DEFAULT '',
                    github_issue_number INTEGER NOT NULL,
                    github_issue_title TEXT NOT NULL,
                    state TEXT NOT NULL DEFAULT 'Discovered',
                    spec_comment_id INTEGER,
                    spec_content TEXT,
                    impl_pr_number INTEGER,
                    last_comment_id INTEGER,
                    last_pr_comment_id INTEGER,
                    previous_state TEXT,
                    error_message TEXT,
                    worktree_path TEXT,
                    retry_count INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    UNIQUE(repo, github_issue_number)
                );

                INSERT INTO issues (id, repo, github_issue_number, github_issue_title, state,
                    last_comment_id, previous_state, error_message, worktree_path,
                    created_at, updated_at)
                SELECT id, '', github_issue_number, github_issue_title,
                    CASE
                        WHEN state IN ('Decomposing', 'AwaitDecompApproval', 'AgentsWorking', 'AwaitSubPRApprovals')
                        THEN 'Discovered'
                        ELSE state
                    END,
                    last_comment_id, previous_state, error_message, worktree_path,
                    created_at, updated_at
                FROM issues_old;

                DROP TABLE IF EXISTS issues_old;
                DROP TABLE IF EXISTS sub_issues;",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        }

        // Check if the issues table exists (it won't on fresh installs before
        // CREATE TABLE IF NOT EXISTS runs below). Incremental migrations must
        // only run when the table is present.
        let table_exists = conn
            .prepare("SELECT id FROM issues LIMIT 0")
            .is_ok();

        // Add last_pr_comment_id column if missing (incremental migration)
        let has_pr_comment_col = conn
            .prepare("SELECT last_pr_comment_id FROM issues LIMIT 0")
            .is_ok();
        if table_exists && !has_pr_comment_col {
            conn.execute_batch(
                "ALTER TABLE issues ADD COLUMN last_pr_comment_id INTEGER;",
            )
            .map_err(|e| HammurabiError::Database(format!("last_pr_comment_id migration failed: {}", e)))?;
        }

        // Add retry_count column if missing (incremental migration)
        let has_retry_count = conn
            .prepare("SELECT retry_count FROM issues LIMIT 0")
            .is_ok();
        if table_exists && !has_retry_count {
            conn.execute_batch(
                "ALTER TABLE issues ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|e| HammurabiError::Database(format!("retry_count migration failed: {}", e)))?;
        }

        // Add repo column if missing (multi-repo migration)
        // SQLite autoindexes backing UNIQUE constraints cannot be dropped,
        // so we must rebuild the table to change UNIQUE(github_issue_number)
        // to UNIQUE(repo, github_issue_number).
        let has_repo_col = conn
            .prepare("SELECT repo FROM issues LIMIT 0")
            .is_ok();
        if table_exists && !has_repo_col {
            conn.execute_batch(
                "ALTER TABLE issues RENAME TO issues_old;

                CREATE TABLE issues (
                    id INTEGER PRIMARY KEY,
                    repo TEXT NOT NULL DEFAULT '',
                    github_issue_number INTEGER NOT NULL,
                    github_issue_title TEXT NOT NULL,
                    state TEXT NOT NULL DEFAULT 'Discovered',
                    spec_comment_id INTEGER,
                    spec_content TEXT,
                    impl_pr_number INTEGER,
                    last_comment_id INTEGER,
                    last_pr_comment_id INTEGER,
                    previous_state TEXT,
                    error_message TEXT,
                    worktree_path TEXT,
                    retry_count INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    UNIQUE(repo, github_issue_number)
                );

                INSERT INTO issues (
                    id, repo, github_issue_number, github_issue_title, state,
                    spec_comment_id, spec_content, impl_pr_number,
                    last_comment_id, last_pr_comment_id,
                    previous_state, error_message, worktree_path,
                    retry_count, created_at, updated_at
                )
                SELECT
                    id, '' as repo, github_issue_number, github_issue_title, state,
                    spec_comment_id, spec_content, impl_pr_number,
                    last_comment_id, last_pr_comment_id,
                    previous_state, error_message, worktree_path,
                    retry_count, created_at, updated_at
                FROM issues_old;

                DROP TABLE issues_old;",
            )
            .map_err(|e| HammurabiError::Database(format!("repo column migration failed: {}", e)))?;
        }

        // Add bypass column if missing (incremental migration)
        let has_bypass = conn
            .prepare("SELECT bypass FROM issues LIMIT 0")
            .is_ok();
        if table_exists && !has_bypass {
            conn.execute_batch(
                "ALTER TABLE issues ADD COLUMN bypass INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|e| HammurabiError::Database(format!("bypass column migration failed: {}", e)))?;
        }

        // Add review_count column if missing (incremental migration)
        let has_review_count = conn
            .prepare("SELECT review_count FROM issues LIMIT 0")
            .is_ok();
        if table_exists && !has_review_count {
            conn.execute_batch(
                "ALTER TABLE issues ADD COLUMN review_count INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|e| HammurabiError::Database(format!("review_count column migration failed: {}", e)))?;
        }

        // Create tables if they don't exist (fresh install or post-migration)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS issues (
                id INTEGER PRIMARY KEY,
                repo TEXT NOT NULL DEFAULT '',
                github_issue_number INTEGER NOT NULL,
                github_issue_title TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'Discovered',
                spec_comment_id INTEGER,
                spec_content TEXT,
                impl_pr_number INTEGER,
                last_comment_id INTEGER,
                last_pr_comment_id INTEGER,
                previous_state TEXT,
                error_message TEXT,
                worktree_path TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0,
                review_count INTEGER NOT NULL DEFAULT 0,
                bypass INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(repo, github_issue_number)
            );

            CREATE TABLE IF NOT EXISTS usage_log (
                id INTEGER PRIMARY KEY,
                issue_id INTEGER NOT NULL REFERENCES issues(id),
                sub_issue_id INTEGER,
                transition TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                model TEXT NOT NULL,
                timestamp TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(())
    }

    /// Backfill empty repo values with the given repo name.
    /// Used during startup when migrating from single-repo to multi-repo.
    pub fn backfill_repo(&self, repo: &str) -> Result<u64, HammurabiError> {
        let conn = self.conn();
        let count = conn
            .execute(
                "UPDATE issues SET repo = ?1 WHERE repo = ''",
                params![repo],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(count as u64)
    }

    // --- Issues ---

    pub fn insert_issue(
        &self,
        repo: &str,
        github_issue_number: u64,
        title: &str,
    ) -> Result<i64, HammurabiError> {
        let conn = self.conn();
        conn.execute(
                "INSERT OR IGNORE INTO issues (repo, github_issue_number, github_issue_title) VALUES (?1, ?2, ?3)",
                params![repo, github_issue_number as i64, title],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_issue(
        &self,
        repo: &str,
        github_issue_number: u64,
    ) -> Result<Option<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, repo, github_issue_number, github_issue_title, state, spec_comment_id,
                        spec_content, impl_pr_number, last_comment_id, last_pr_comment_id,
                        previous_state, error_message, worktree_path, retry_count, review_count,
                        bypass, created_at, updated_at
                 FROM issues WHERE repo = ?1 AND github_issue_number = ?2",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let result = stmt
            .query_row(params![repo, github_issue_number as i64], |row| {
                Ok(row_to_tracked_issue(row))
            })
            .optional()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        match result {
            Some(issue) => Ok(Some(issue)),
            None => Ok(None),
        }
    }

    /// Get an issue by number across all repos. Returns all matches.
    pub fn get_issue_any_repo(
        &self,
        github_issue_number: u64,
    ) -> Result<Vec<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, repo, github_issue_number, github_issue_title, state, spec_comment_id,
                        spec_content, impl_pr_number, last_comment_id, last_pr_comment_id,
                        previous_state, error_message, worktree_path, retry_count, review_count,
                        bypass, created_at, updated_at
                 FROM issues WHERE github_issue_number = ?1",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let issues = stmt
            .query_map(params![github_issue_number as i64], |row| {
                Ok(row_to_tracked_issue(row))
            })
            .map_err(|e| HammurabiError::Database(e.to_string()))?
            .collect::<SqlResult<Vec<_>>>()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(issues)
    }

    pub fn get_all_issues(&self) -> Result<Vec<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, repo, github_issue_number, github_issue_title, state, spec_comment_id,
                        spec_content, impl_pr_number, last_comment_id, last_pr_comment_id,
                        previous_state, error_message, worktree_path, retry_count, review_count,
                        bypass, created_at, updated_at
                 FROM issues",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let issues = stmt
            .query_map([], |row| Ok(row_to_tracked_issue(row)))
            .map_err(|e| HammurabiError::Database(e.to_string()))?
            .collect::<SqlResult<Vec<_>>>()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(issues)
    }

    pub fn get_all_issues_for_repo(
        &self,
        repo: &str,
    ) -> Result<Vec<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, repo, github_issue_number, github_issue_title, state, spec_comment_id,
                        spec_content, impl_pr_number, last_comment_id, last_pr_comment_id,
                        previous_state, error_message, worktree_path, retry_count, review_count,
                        bypass, created_at, updated_at
                 FROM issues WHERE repo = ?1",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let issues = stmt
            .query_map(params![repo], |row| Ok(row_to_tracked_issue(row)))
            .map_err(|e| HammurabiError::Database(e.to_string()))?
            .collect::<SqlResult<Vec<_>>>()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(issues)
    }

    pub fn get_issues_by_state(
        &self,
        state: IssueState,
    ) -> Result<Vec<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, repo, github_issue_number, github_issue_title, state, spec_comment_id,
                        spec_content, impl_pr_number, last_comment_id, last_pr_comment_id,
                        previous_state, error_message, worktree_path, retry_count, review_count,
                        bypass, created_at, updated_at
                 FROM issues WHERE state = ?1",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let issues = stmt
            .query_map(params![state.to_string()], |row| {
                Ok(row_to_tracked_issue(row))
            })
            .map_err(|e| HammurabiError::Database(e.to_string()))?
            .collect::<SqlResult<Vec<_>>>()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(issues)
    }

    pub fn update_issue_state(
        &self,
        id: i64,
        new_state: IssueState,
        previous_state: Option<IssueState>,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET state = ?1, previous_state = ?2, updated_at = datetime('now') WHERE id = ?3",
                params![
                    new_state.to_string(),
                    previous_state.map(|s| s.to_string()),
                    id
                ],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_error(
        &self,
        id: i64,
        error_message: &str,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET error_message = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![error_message, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_spec_comment(
        &self,
        id: i64,
        comment_id: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET spec_comment_id = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![comment_id as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_spec_content(
        &self,
        id: i64,
        spec_content: &str,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET spec_content = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![spec_content, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_impl_pr(
        &self,
        id: i64,
        pr_number: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET impl_pr_number = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![pr_number as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_last_comment(
        &self,
        id: i64,
        comment_id: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET last_comment_id = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![comment_id as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_last_pr_comment(
        &self,
        id: i64,
        comment_id: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET last_pr_comment_id = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![comment_id as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn increment_retry_count(&self, id: i64) -> Result<u32, HammurabiError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE issues SET retry_count = retry_count + 1, updated_at = datetime('now') WHERE id = ?1",
            params![id],
        )
        .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let count: i64 = conn
            .query_row(
                "SELECT retry_count FROM issues WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(count as u32)
    }

    pub fn reset_retry_count(&self, id: i64) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET retry_count = 0, updated_at = datetime('now') WHERE id = ?1",
                params![id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn increment_review_count(&self, id: i64) -> Result<u32, HammurabiError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE issues SET review_count = review_count + 1, updated_at = datetime('now') WHERE id = ?1",
            params![id],
        )
        .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let count: i64 = conn
            .query_row(
                "SELECT review_count FROM issues WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(count as u32)
    }

    pub fn reset_review_count(&self, id: i64) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET review_count = 0, updated_at = datetime('now') WHERE id = ?1",
                params![id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_issue_bypass(&self, id: i64, bypass: bool) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET bypass = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![bypass as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_worktree(
        &self,
        id: i64,
        path: Option<&str>,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET worktree_path = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![path, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    // --- Usage log ---

    pub fn log_usage(
        &self,
        issue_id: i64,
        sub_issue_id: Option<i64>,
        transition: &str,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "INSERT INTO usage_log (issue_id, sub_issue_id, transition, input_tokens, output_tokens, model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    issue_id,
                    sub_issue_id,
                    transition,
                    input_tokens as i64,
                    output_tokens as i64,
                    model
                ],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_usage_by_issue(&self, issue_id: i64) -> Result<Vec<UsageEntry>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, issue_id, sub_issue_id, transition, input_tokens, output_tokens, model, timestamp
                 FROM usage_log WHERE issue_id = ?1 ORDER BY timestamp",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let entries = stmt
            .query_map(params![issue_id], |row| {
                Ok(UsageEntry {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    sub_issue_id: row.get(2)?,
                    transition: row.get(3)?,
                    input_tokens: row.get::<_, i64>(4)? as u64,
                    output_tokens: row.get::<_, i64>(5)? as u64,
                    model: row.get(6)?,
                    timestamp: row.get(7)?,
                })
            })
            .map_err(|e| HammurabiError::Database(e.to_string()))?
            .collect::<SqlResult<Vec<_>>>()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(entries)
    }
}

fn row_to_tracked_issue(row: &rusqlite::Row) -> TrackedIssue {
    TrackedIssue {
        id: row.get(0).unwrap(),
        repo: row.get(1).unwrap(),
        github_issue_number: row.get::<_, i64>(2).unwrap() as u64,
        title: row.get(3).unwrap(),
        state: row
            .get::<_, String>(4)
            .unwrap()
            .parse()
            .unwrap_or(IssueState::Failed),
        spec_comment_id: row.get::<_, Option<i64>>(5).unwrap().map(|v| v as u64),
        spec_content: row.get(6).unwrap(),
        impl_pr_number: row.get::<_, Option<i64>>(7).unwrap().map(|v| v as u64),
        last_comment_id: row.get::<_, Option<i64>>(8).unwrap().map(|v| v as u64),
        last_pr_comment_id: row.get::<_, Option<i64>>(9).unwrap().map(|v| v as u64),
        previous_state: row
            .get::<_, Option<String>>(10)
            .unwrap()
            .and_then(|s| s.parse().ok()),
        error_message: row.get(11).unwrap(),
        worktree_path: row.get(12).unwrap(),
        retry_count: row.get::<_, i64>(13).unwrap_or(0) as u32,
        review_count: row.get::<_, i64>(14).unwrap_or(0) as u32,
        bypass: row.get::<_, i64>(15).unwrap_or(0) != 0,
        created_at: row.get(16).unwrap(),
        updated_at: row.get(17).unwrap(),
    }
}

trait OptionalRow {
    fn optional(self) -> SqlResult<Option<TrackedIssue>>;
}

impl OptionalRow for SqlResult<TrackedIssue> {
    fn optional(self) -> SqlResult<Option<TrackedIssue>> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::open(":memory:").unwrap()
    }

    #[test]
    fn test_insert_and_get_issue() {
        let db = test_db();
        let id = db.insert_issue("owner/repo", 42, "Test issue").unwrap();
        assert!(id > 0);

        let issue = db.get_issue("owner/repo", 42).unwrap().unwrap();
        assert_eq!(issue.github_issue_number, 42);
        assert_eq!(issue.repo, "owner/repo");
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.state, IssueState::Discovered);
        assert!(issue.spec_comment_id.is_none());
        assert!(issue.impl_pr_number.is_none());
        assert!(issue.spec_content.is_none());
        assert!(issue.previous_state.is_none());
        assert!(issue.error_message.is_none());
    }

    #[test]
    fn test_get_nonexistent_issue() {
        let db = test_db();
        assert!(db.get_issue("owner/repo", 999).unwrap().is_none());
    }

    #[test]
    fn test_update_issue_state() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_state(issue.id, IssueState::SpecDrafting, Some(IssueState::Discovered))
            .unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::SpecDrafting);
        assert_eq!(updated.previous_state, Some(IssueState::Discovered));
    }

    #[test]
    fn test_update_issue_error() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_error(issue.id, "something went wrong")
            .unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.error_message.as_deref(), Some("something went wrong"));
    }

    #[test]
    fn test_update_issue_spec_comment() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_spec_comment(issue.id, 100).unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.spec_comment_id, Some(100));
    }

    #[test]
    fn test_update_issue_spec_content() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_spec_content(issue.id, "# Spec\nDo the thing")
            .unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.spec_content.as_deref(), Some("# Spec\nDo the thing"));
    }

    #[test]
    fn test_update_issue_impl_pr() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_impl_pr(issue.id, 200).unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.impl_pr_number, Some(200));
    }

    #[test]
    fn test_get_all_issues() {
        let db = test_db();
        db.insert_issue("owner/repo-a", 1, "Issue 1").unwrap();
        db.insert_issue("owner/repo-a", 2, "Issue 2").unwrap();
        db.insert_issue("owner/repo-b", 3, "Issue 3").unwrap();

        let issues = db.get_all_issues().unwrap();
        assert_eq!(issues.len(), 3);
    }

    #[test]
    fn test_get_all_issues_for_repo() {
        let db = test_db();
        db.insert_issue("owner/repo-a", 1, "Issue 1").unwrap();
        db.insert_issue("owner/repo-a", 2, "Issue 2").unwrap();
        db.insert_issue("owner/repo-b", 3, "Issue 3").unwrap();

        let issues_a = db.get_all_issues_for_repo("owner/repo-a").unwrap();
        assert_eq!(issues_a.len(), 2);

        let issues_b = db.get_all_issues_for_repo("owner/repo-b").unwrap();
        assert_eq!(issues_b.len(), 1);
        assert_eq!(issues_b[0].github_issue_number, 3);
    }

    #[test]
    fn test_get_issues_by_state() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        db.insert_issue("owner/repo", 2, "Issue 2").unwrap();
        let issue2 = db.get_issue("owner/repo", 2).unwrap().unwrap();
        db.update_issue_state(issue2.id, IssueState::SpecDrafting, None)
            .unwrap();

        let discovered = db.get_issues_by_state(IssueState::Discovered).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].github_issue_number, 1);

        let drafting = db.get_issues_by_state(IssueState::SpecDrafting).unwrap();
        assert_eq!(drafting.len(), 1);
        assert_eq!(drafting[0].github_issue_number, 2);
    }

    #[test]
    fn test_usage_log() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.log_usage(issue.id, None, "spec_drafting", 1000, 2000, "claude-sonnet-4-6")
            .unwrap();
        db.log_usage(issue.id, None, "implementing", 500, 800, "claude-sonnet-4-6")
            .unwrap();

        let usage = db.get_usage_by_issue(issue.id).unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].transition, "spec_drafting");
        assert_eq!(usage[0].input_tokens, 1000);
        assert_eq!(usage[0].output_tokens, 2000);
        assert_eq!(usage[0].model, "claude-sonnet-4-6");
        assert_eq!(usage[1].transition, "implementing");
    }

    #[test]
    fn test_update_issue_worktree() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_worktree(issue.id, Some("/tmp/worktree"))
            .unwrap();
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.worktree_path.as_deref(), Some("/tmp/worktree"));

        db.update_issue_worktree(issue.id, None).unwrap();
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert!(updated.worktree_path.is_none());
    }

    #[test]
    fn test_insert_duplicate_issue_ignored() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        db.insert_issue("owner/repo", 1, "Issue 1 duplicate").unwrap(); // OR IGNORE

        let issues = db.get_all_issues().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Issue 1");
    }

    #[test]
    fn test_same_issue_number_different_repos() {
        let db = test_db();
        db.insert_issue("owner/repo-a", 1, "Issue in A").unwrap();
        db.insert_issue("owner/repo-b", 1, "Issue in B").unwrap();

        let issues = db.get_all_issues().unwrap();
        assert_eq!(issues.len(), 2);

        let a = db.get_issue("owner/repo-a", 1).unwrap().unwrap();
        assert_eq!(a.title, "Issue in A");

        let b = db.get_issue("owner/repo-b", 1).unwrap().unwrap();
        assert_eq!(b.title, "Issue in B");
    }

    #[test]
    fn test_get_issue_any_repo() {
        let db = test_db();
        db.insert_issue("owner/repo-a", 1, "Issue in A").unwrap();
        db.insert_issue("owner/repo-b", 1, "Issue in B").unwrap();

        let issues = db.get_issue_any_repo(1).unwrap();
        assert_eq!(issues.len(), 2);

        let issues = db.get_issue_any_repo(999).unwrap();
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn test_backfill_repo() {
        let db = test_db();
        // Insert with empty repo (simulating pre-migration data)
        db.insert_issue("", 1, "Issue 1").unwrap();
        db.insert_issue("", 2, "Issue 2").unwrap();
        db.insert_issue("owner/existing", 3, "Issue 3").unwrap();

        let count = db.backfill_repo("owner/repo").unwrap();
        assert_eq!(count, 2);

        let issue1 = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue1.repo, "owner/repo");

        // Issue 3 should be unchanged
        let issue3 = db.get_issue("owner/existing", 3).unwrap().unwrap();
        assert_eq!(issue3.repo, "owner/existing");
    }

    #[test]
    fn test_spec_comment_and_last_comment() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.update_issue_spec_comment(issue.id, 555).unwrap();
        db.update_issue_last_comment(issue.id, 600).unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.spec_comment_id, Some(555));
        assert_eq!(updated.last_comment_id, Some(600));
    }

    #[test]
    fn test_retry_count_default_zero() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(issue.retry_count, 0);
    }

    #[test]
    fn test_increment_retry_count() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        let count = db.increment_retry_count(issue.id).unwrap();
        assert_eq!(count, 1);

        let count = db.increment_retry_count(issue.id).unwrap();
        assert_eq!(count, 2);

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.retry_count, 2);
    }

    #[test]
    fn test_reset_retry_count() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.increment_retry_count(issue.id).unwrap();
        db.increment_retry_count(issue.id).unwrap();
        db.reset_retry_count(issue.id).unwrap();

        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert_eq!(updated.retry_count, 0);
    }

    #[test]
    fn test_bypass_default_false() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert!(!issue.bypass);
    }

    #[test]
    fn test_set_issue_bypass() {
        let db = test_db();
        db.insert_issue("owner/repo", 1, "Issue 1").unwrap();
        let issue = db.get_issue("owner/repo", 1).unwrap().unwrap();

        db.set_issue_bypass(issue.id, true).unwrap();
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert!(updated.bypass);

        db.set_issue_bypass(issue.id, false).unwrap();
        let updated = db.get_issue("owner/repo", 1).unwrap().unwrap();
        assert!(!updated.bypass);
    }
}
