use std::fmt;
use std::str::FromStr;

/// Origin of a tracked issue. Persisted as a lowercase string in the DB
/// `source` column so the unique identity `(source, repo, external_id)`
/// remains readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    GitHub,
    Discord,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::GitHub => "github",
            SourceKind::Discord => "discord",
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SourceKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(SourceKind::GitHub),
            "discord" => Ok(SourceKind::Discord),
            _ => Err(format!("unknown source: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Discovered,
    SpecDrafting,
    AwaitSpecApproval,
    Implementing,
    Reviewing,
    AwaitPRApproval,
    Done,
    Failed,
}

impl IssueState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            IssueState::Discovered
                | IssueState::SpecDrafting
                | IssueState::Implementing
                | IssueState::Reviewing
        )
    }

    #[allow(dead_code)]
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            IssueState::AwaitSpecApproval | IssueState::AwaitPRApproval
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, IssueState::Done | IssueState::Failed)
    }

    pub fn sort_priority(&self) -> u8 {
        match self {
            IssueState::Failed => 0,
            IssueState::Discovered
            | IssueState::SpecDrafting
            | IssueState::Implementing
            | IssueState::Reviewing => 1,
            IssueState::AwaitSpecApproval | IssueState::AwaitPRApproval => 2,
            IssueState::Done => 3,
        }
    }
}

macro_rules! issue_state_strings {
    ($($variant:ident => $s:expr),+ $(,)?) => {
        impl fmt::Display for IssueState {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(IssueState::$variant => write!(f, $s),)+
                }
            }
        }

        impl FromStr for IssueState {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok(IssueState::$variant),)+
                    _ => Err(format!("unknown issue state: {}", s)),
                }
            }
        }
    };
}

issue_state_strings! {
    Discovered       => "Discovered",
    SpecDrafting     => "SpecDrafting",
    AwaitSpecApproval => "AwaitSpecApproval",
    Implementing     => "Implementing",
    Reviewing        => "Reviewing",
    AwaitPRApproval  => "AwaitPRApproval",
    Done             => "Done",
    Failed           => "Failed",
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TrackedIssue {
    pub id: i64,
    /// Origin of this issue (GitHub label poll, Discord thread, etc.).
    /// Canonical identity is `(source, repo, external_id)`.
    pub source: SourceKind,
    /// Source-native identifier as text. For GitHub this is the issue
    /// number stringified; for Discord it is the thread ID.
    pub external_id: String,
    pub repo: String,
    /// GitHub issue number, or `0` for Discord-sourced rows that haven't
    /// reached `/confirm` yet. Populated once the spec is approved and
    /// `ensure_github_issue` opens the issue.
    pub github_issue_number: u64,
    pub title: String,
    pub state: IssueState,
    pub spec_comment_id: Option<u64>,
    pub spec_content: Option<String>,
    pub impl_pr_number: Option<u64>,
    pub last_comment_id: Option<u64>,
    pub last_pr_comment_id: Option<u64>,
    pub previous_state: Option<IssueState>,
    pub error_message: Option<String>,
    pub worktree_path: Option<String>,
    pub retry_count: u32,
    pub review_count: u32,
    pub review_feedback: Option<String>,
    pub bypass: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[allow(dead_code)]
impl TrackedIssue {
    /// True if this is a Discord-sourced intake that hasn't yet been
    /// `/confirm`ed into a GitHub issue.
    pub fn is_discord_pending(&self) -> bool {
        self.source == SourceKind::Discord && self.github_issue_number == 0
    }

    /// Parse `external_id` as a u64. For Discord rows this is the thread
    /// snowflake; for GitHub rows it's the issue number in textual form.
    /// Returns `None` if the id isn't numeric (shouldn't happen for our
    /// supported sources but guards against DB corruption).
    pub fn external_id_u64(&self) -> Option<u64> {
        self.external_id.parse().ok()
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UsageEntry {
    pub id: i64,
    pub issue_id: i64,
    pub sub_issue_id: Option<i64>,
    pub transition: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_state_roundtrip() {
        let states = [
            IssueState::Discovered,
            IssueState::SpecDrafting,
            IssueState::AwaitSpecApproval,
            IssueState::Implementing,
            IssueState::Reviewing,
            IssueState::AwaitPRApproval,
            IssueState::Done,
            IssueState::Failed,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: IssueState = s.parse().unwrap();
            assert_eq!(*state, parsed);
        }
    }

    #[test]
    fn test_issue_state_classification() {
        assert!(IssueState::Discovered.is_active());
        assert!(IssueState::SpecDrafting.is_active());
        assert!(IssueState::Implementing.is_active());
        assert!(IssueState::Reviewing.is_active());
        assert!(!IssueState::AwaitSpecApproval.is_active());
        assert!(!IssueState::Done.is_active());

        assert!(IssueState::AwaitSpecApproval.is_blocking());
        assert!(IssueState::AwaitPRApproval.is_blocking());
        assert!(!IssueState::Discovered.is_blocking());

        assert!(IssueState::Done.is_terminal());
        assert!(IssueState::Failed.is_terminal());
        assert!(!IssueState::Discovered.is_terminal());
    }

    #[test]
    fn test_sort_priority() {
        assert!(IssueState::Failed.sort_priority() < IssueState::Discovered.sort_priority());
        assert!(
            IssueState::Discovered.sort_priority() < IssueState::AwaitSpecApproval.sort_priority()
        );
        assert!(IssueState::AwaitSpecApproval.sort_priority() < IssueState::Done.sort_priority());
    }

    #[test]
    fn test_invalid_state_parse() {
        assert!(IssueState::from_str("invalid").is_err());
    }

    #[test]
    fn test_source_kind_roundtrip() {
        for kind in [SourceKind::GitHub, SourceKind::Discord] {
            let s = kind.to_string();
            let parsed: SourceKind = s.parse().unwrap();
            assert_eq!(kind, parsed);
        }
    }

    #[test]
    fn test_source_kind_invalid_parse() {
        assert!("".parse::<SourceKind>().is_err());
        assert!("slack".parse::<SourceKind>().is_err());
    }
}
