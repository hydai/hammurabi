use rusqlite::{params, Connection, Result as SqlResult};
use std::sync::Mutex;

use crate::error::HammurabiError;
use crate::models::{IssueState, SubIssue, SubIssueState, TrackedIssue, UsageEntry};

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
        self.conn()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS issues (
                    id INTEGER PRIMARY KEY,
                    github_issue_number INTEGER UNIQUE NOT NULL,
                    github_issue_title TEXT NOT NULL,
                    state TEXT NOT NULL DEFAULT 'Discovered',
                    spec_pr_number INTEGER,
                    decomposition_comment_id INTEGER,
                    last_comment_id INTEGER,
                    previous_state TEXT,
                    error_message TEXT,
                    worktree_path TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE TABLE IF NOT EXISTS sub_issues (
                    id INTEGER PRIMARY KEY,
                    parent_issue_id INTEGER NOT NULL REFERENCES issues(id),
                    github_issue_number INTEGER,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    state TEXT NOT NULL DEFAULT 'pending',
                    pr_number INTEGER,
                    worktree_path TEXT,
                    session_id TEXT
                );

                CREATE TABLE IF NOT EXISTS usage_log (
                    id INTEGER PRIMARY KEY,
                    issue_id INTEGER NOT NULL REFERENCES issues(id),
                    sub_issue_id INTEGER REFERENCES sub_issues(id),
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

    // --- Issues ---

    pub fn insert_issue(
        &self,
        github_issue_number: u64,
        title: &str,
    ) -> Result<i64, HammurabiError> {
        let conn = self.conn();
        conn.execute(
                "INSERT OR IGNORE INTO issues (github_issue_number, github_issue_title) VALUES (?1, ?2)",
                params![github_issue_number as i64, title],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_issue(
        &self,
        github_issue_number: u64,
    ) -> Result<Option<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, github_issue_number, github_issue_title, state, spec_pr_number,
                        decomposition_comment_id, last_comment_id, previous_state,
                        error_message, worktree_path, created_at, updated_at
                 FROM issues WHERE github_issue_number = ?1",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let result = stmt
            .query_row(params![github_issue_number as i64], |row| {
                Ok(row_to_tracked_issue(row))
            })
            .optional()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        match result {
            Some(issue) => Ok(Some(issue)),
            None => Ok(None),
        }
    }

    pub fn get_all_issues(&self) -> Result<Vec<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, github_issue_number, github_issue_title, state, spec_pr_number,
                        decomposition_comment_id, last_comment_id, previous_state,
                        error_message, worktree_path, created_at, updated_at
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

    pub fn get_issues_by_state(
        &self,
        state: IssueState,
    ) -> Result<Vec<TrackedIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, github_issue_number, github_issue_title, state, spec_pr_number,
                        decomposition_comment_id, last_comment_id, previous_state,
                        error_message, worktree_path, created_at, updated_at
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

    pub fn update_issue_spec_pr(
        &self,
        id: i64,
        pr_number: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET spec_pr_number = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![pr_number as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_issue_decomp_comment(
        &self,
        id: i64,
        comment_id: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE issues SET decomposition_comment_id = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![comment_id as i64, id],
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

    // --- Sub-issues ---

    pub fn insert_sub_issue(
        &self,
        parent_issue_id: i64,
        title: &str,
        description: &str,
    ) -> Result<i64, HammurabiError> {
        let conn = self.conn();
        conn.execute(
                "INSERT INTO sub_issues (parent_issue_id, title, description) VALUES (?1, ?2, ?3)",
                params![parent_issue_id, title, description],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_sub_issues(&self, parent_issue_id: i64) -> Result<Vec<SubIssue>, HammurabiError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, parent_issue_id, github_issue_number, title, description,
                        state, pr_number, worktree_path, session_id
                 FROM sub_issues WHERE parent_issue_id = ?1",
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        let sub_issues = stmt
            .query_map(params![parent_issue_id], |row| {
                Ok(row_to_sub_issue(row))
            })
            .map_err(|e| HammurabiError::Database(e.to_string()))?
            .collect::<SqlResult<Vec<_>>>()
            .map_err(|e| HammurabiError::Database(e.to_string()))?;

        Ok(sub_issues)
    }

    pub fn update_sub_issue_state(
        &self,
        id: i64,
        state: SubIssueState,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE sub_issues SET state = ?1 WHERE id = ?2",
                params![state.to_string(), id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_sub_issue_github_number(
        &self,
        id: i64,
        github_issue_number: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE sub_issues SET github_issue_number = ?1 WHERE id = ?2",
                params![github_issue_number as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_sub_issue_pr(
        &self,
        id: i64,
        pr_number: u64,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE sub_issues SET pr_number = ?1 WHERE id = ?2",
                params![pr_number as i64, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_sub_issue_worktree(
        &self,
        id: i64,
        path: Option<&str>,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE sub_issues SET worktree_path = ?1 WHERE id = ?2",
                params![path, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_sub_issue_session(
        &self,
        id: i64,
        session_id: Option<&str>,
    ) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE sub_issues SET session_id = ?1 WHERE id = ?2",
                params![session_id, id],
            )
            .map_err(|e| HammurabiError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn reset_failed_sub_issues(&self, parent_issue_id: i64) -> Result<(), HammurabiError> {
        self.conn()
            .execute(
                "UPDATE sub_issues SET state = 'pending', worktree_path = NULL, session_id = NULL WHERE parent_issue_id = ?1 AND state = 'failed'",
                params![parent_issue_id],
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
        github_issue_number: row.get::<_, i64>(1).unwrap() as u64,
        title: row.get(2).unwrap(),
        state: row
            .get::<_, String>(3)
            .unwrap()
            .parse()
            .unwrap_or(IssueState::Failed),
        spec_pr_number: row.get::<_, Option<i64>>(4).unwrap().map(|v| v as u64),
        decomposition_comment_id: row.get::<_, Option<i64>>(5).unwrap().map(|v| v as u64),
        last_comment_id: row.get::<_, Option<i64>>(6).unwrap().map(|v| v as u64),
        previous_state: row
            .get::<_, Option<String>>(7)
            .unwrap()
            .and_then(|s| s.parse().ok()),
        error_message: row.get(8).unwrap(),
        worktree_path: row.get(9).unwrap(),
        created_at: row.get(10).unwrap(),
        updated_at: row.get(11).unwrap(),
    }
}

fn row_to_sub_issue(row: &rusqlite::Row) -> SubIssue {
    SubIssue {
        id: row.get(0).unwrap(),
        parent_issue_id: row.get(1).unwrap(),
        github_issue_number: row.get::<_, Option<i64>>(2).unwrap().map(|v| v as u64),
        title: row.get(3).unwrap(),
        description: row.get(4).unwrap(),
        state: row
            .get::<_, String>(5)
            .unwrap()
            .parse()
            .unwrap_or(SubIssueState::Failed),
        pr_number: row.get::<_, Option<i64>>(6).unwrap().map(|v| v as u64),
        worktree_path: row.get(7).unwrap(),
        session_id: row.get(8).unwrap(),
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
        let id = db.insert_issue(42, "Test issue").unwrap();
        assert!(id > 0);

        let issue = db.get_issue(42).unwrap().unwrap();
        assert_eq!(issue.github_issue_number, 42);
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.state, IssueState::Discovered);
        assert!(issue.spec_pr_number.is_none());
        assert!(issue.previous_state.is_none());
        assert!(issue.error_message.is_none());
    }

    #[test]
    fn test_get_nonexistent_issue() {
        let db = test_db();
        assert!(db.get_issue(999).unwrap().is_none());
    }

    #[test]
    fn test_update_issue_state() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        db.update_issue_state(issue.id, IssueState::SpecDrafting, Some(IssueState::Discovered))
            .unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.state, IssueState::SpecDrafting);
        assert_eq!(updated.previous_state, Some(IssueState::Discovered));
    }

    #[test]
    fn test_update_issue_error() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        db.update_issue_error(issue.id, "something went wrong")
            .unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.error_message.as_deref(), Some("something went wrong"));
    }

    #[test]
    fn test_update_issue_spec_pr() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        db.update_issue_spec_pr(issue.id, 100).unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.spec_pr_number, Some(100));
    }

    #[test]
    fn test_get_all_issues() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        db.insert_issue(2, "Issue 2").unwrap();
        db.insert_issue(3, "Issue 3").unwrap();

        let issues = db.get_all_issues().unwrap();
        assert_eq!(issues.len(), 3);
    }

    #[test]
    fn test_get_issues_by_state() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        db.insert_issue(2, "Issue 2").unwrap();
        let issue2 = db.get_issue(2).unwrap().unwrap();
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
    fn test_insert_and_get_sub_issues() {
        let db = test_db();
        db.insert_issue(1, "Parent").unwrap();
        let parent = db.get_issue(1).unwrap().unwrap();

        let sub_id1 = db
            .insert_sub_issue(parent.id, "Sub 1", "Do thing 1")
            .unwrap();
        let sub_id2 = db
            .insert_sub_issue(parent.id, "Sub 2", "Do thing 2")
            .unwrap();

        let subs = db.get_sub_issues(parent.id).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].title, "Sub 1");
        assert_eq!(subs[0].description, "Do thing 1");
        assert_eq!(subs[0].state, SubIssueState::Pending);
        assert_eq!(subs[1].title, "Sub 2");

        db.update_sub_issue_state(sub_id1, SubIssueState::Working)
            .unwrap();
        db.update_sub_issue_pr(sub_id2, 200).unwrap();

        let subs = db.get_sub_issues(parent.id).unwrap();
        assert_eq!(subs[0].state, SubIssueState::Working);
        assert_eq!(subs[1].pr_number, Some(200));
    }

    #[test]
    fn test_reset_failed_sub_issues() {
        let db = test_db();
        db.insert_issue(1, "Parent").unwrap();
        let parent = db.get_issue(1).unwrap().unwrap();

        let sub1 = db.insert_sub_issue(parent.id, "Sub 1", "").unwrap();
        let sub2 = db.insert_sub_issue(parent.id, "Sub 2", "").unwrap();
        let sub3 = db.insert_sub_issue(parent.id, "Sub 3", "").unwrap();

        db.update_sub_issue_state(sub1, SubIssueState::Done).unwrap();
        db.update_sub_issue_state(sub2, SubIssueState::Failed).unwrap();
        db.update_sub_issue_state(sub3, SubIssueState::PrOpen).unwrap();

        db.reset_failed_sub_issues(parent.id).unwrap();

        let subs = db.get_sub_issues(parent.id).unwrap();
        assert_eq!(subs[0].state, SubIssueState::Done);
        assert_eq!(subs[1].state, SubIssueState::Pending);
        assert_eq!(subs[2].state, SubIssueState::PrOpen);
    }

    #[test]
    fn test_usage_log() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        db.log_usage(issue.id, None, "spec_drafting", 1000, 2000, "claude-sonnet-4-6")
            .unwrap();
        db.log_usage(issue.id, None, "decomposing", 500, 800, "claude-sonnet-4-6")
            .unwrap();

        let usage = db.get_usage_by_issue(issue.id).unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].transition, "spec_drafting");
        assert_eq!(usage[0].input_tokens, 1000);
        assert_eq!(usage[0].output_tokens, 2000);
        assert_eq!(usage[0].model, "claude-sonnet-4-6");
        assert_eq!(usage[1].transition, "decomposing");
    }

    #[test]
    fn test_update_sub_issue_session() {
        let db = test_db();
        db.insert_issue(1, "Parent").unwrap();
        let parent = db.get_issue(1).unwrap().unwrap();
        let sub_id = db.insert_sub_issue(parent.id, "Sub 1", "").unwrap();

        db.update_sub_issue_session(sub_id, Some("session-abc-123"))
            .unwrap();

        let subs = db.get_sub_issues(parent.id).unwrap();
        assert_eq!(subs[0].session_id.as_deref(), Some("session-abc-123"));

        db.update_sub_issue_session(sub_id, None).unwrap();
        let subs = db.get_sub_issues(parent.id).unwrap();
        assert!(subs[0].session_id.is_none());
    }

    #[test]
    fn test_update_issue_worktree() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        db.update_issue_worktree(issue.id, Some("/tmp/worktree"))
            .unwrap();
        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.worktree_path.as_deref(), Some("/tmp/worktree"));

        db.update_issue_worktree(issue.id, None).unwrap();
        let updated = db.get_issue(1).unwrap().unwrap();
        assert!(updated.worktree_path.is_none());
    }

    #[test]
    fn test_insert_duplicate_issue_ignored() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        db.insert_issue(1, "Issue 1 duplicate").unwrap(); // OR IGNORE

        let issues = db.get_all_issues().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Issue 1");
    }

    #[test]
    fn test_decomp_comment_and_last_comment() {
        let db = test_db();
        db.insert_issue(1, "Issue 1").unwrap();
        let issue = db.get_issue(1).unwrap().unwrap();

        db.update_issue_decomp_comment(issue.id, 555).unwrap();
        db.update_issue_last_comment(issue.id, 600).unwrap();

        let updated = db.get_issue(1).unwrap().unwrap();
        assert_eq!(updated.decomposition_comment_id, Some(555));
        assert_eq!(updated.last_comment_id, Some(600));
    }
}
