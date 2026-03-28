use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Discovered,
    SpecDrafting,
    AwaitSpecApproval,
    Decomposing,
    AwaitDecompApproval,
    AgentsWorking,
    AwaitSubPRApprovals,
    Done,
    Failed,
}

impl IssueState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            IssueState::Discovered
                | IssueState::SpecDrafting
                | IssueState::Decomposing
                | IssueState::AgentsWorking
        )
    }

    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            IssueState::AwaitSpecApproval
                | IssueState::AwaitDecompApproval
                | IssueState::AwaitSubPRApprovals
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
            | IssueState::Decomposing
            | IssueState::AgentsWorking => 1,
            IssueState::AwaitSpecApproval
            | IssueState::AwaitDecompApproval
            | IssueState::AwaitSubPRApprovals => 2,
            IssueState::Done => 3,
        }
    }
}

impl fmt::Display for IssueState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IssueState::Discovered => write!(f, "Discovered"),
            IssueState::SpecDrafting => write!(f, "SpecDrafting"),
            IssueState::AwaitSpecApproval => write!(f, "AwaitSpecApproval"),
            IssueState::Decomposing => write!(f, "Decomposing"),
            IssueState::AwaitDecompApproval => write!(f, "AwaitDecompApproval"),
            IssueState::AgentsWorking => write!(f, "AgentsWorking"),
            IssueState::AwaitSubPRApprovals => write!(f, "AwaitSubPRApprovals"),
            IssueState::Done => write!(f, "Done"),
            IssueState::Failed => write!(f, "Failed"),
        }
    }
}

impl FromStr for IssueState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Discovered" => Ok(IssueState::Discovered),
            "SpecDrafting" => Ok(IssueState::SpecDrafting),
            "AwaitSpecApproval" => Ok(IssueState::AwaitSpecApproval),
            "Decomposing" => Ok(IssueState::Decomposing),
            "AwaitDecompApproval" => Ok(IssueState::AwaitDecompApproval),
            "AgentsWorking" => Ok(IssueState::AgentsWorking),
            "AwaitSubPRApprovals" => Ok(IssueState::AwaitSubPRApprovals),
            "Done" => Ok(IssueState::Done),
            "Failed" => Ok(IssueState::Failed),
            _ => Err(format!("unknown issue state: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubIssueState {
    Pending,
    Working,
    PrOpen,
    Done,
    Failed,
}

impl fmt::Display for SubIssueState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubIssueState::Pending => write!(f, "pending"),
            SubIssueState::Working => write!(f, "working"),
            SubIssueState::PrOpen => write!(f, "pr_open"),
            SubIssueState::Done => write!(f, "done"),
            SubIssueState::Failed => write!(f, "failed"),
        }
    }
}

impl FromStr for SubIssueState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(SubIssueState::Pending),
            "working" => Ok(SubIssueState::Working),
            "pr_open" => Ok(SubIssueState::PrOpen),
            "done" => Ok(SubIssueState::Done),
            "failed" => Ok(SubIssueState::Failed),
            _ => Err(format!("unknown sub-issue state: {}", s)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrackedIssue {
    pub id: i64,
    pub github_issue_number: u64,
    pub title: String,
    pub state: IssueState,
    pub spec_pr_number: Option<u64>,
    pub decomposition_comment_id: Option<u64>,
    pub last_comment_id: Option<u64>,
    pub previous_state: Option<IssueState>,
    pub error_message: Option<String>,
    pub worktree_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SubIssue {
    pub id: i64,
    pub parent_issue_id: i64,
    pub github_issue_number: Option<u64>,
    pub title: String,
    pub description: String,
    pub state: SubIssueState,
    pub pr_number: Option<u64>,
    pub worktree_path: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone)]
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
            IssueState::Decomposing,
            IssueState::AwaitDecompApproval,
            IssueState::AgentsWorking,
            IssueState::AwaitSubPRApprovals,
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
    fn test_sub_issue_state_roundtrip() {
        let states = [
            SubIssueState::Pending,
            SubIssueState::Working,
            SubIssueState::PrOpen,
            SubIssueState::Done,
            SubIssueState::Failed,
        ];
        for state in &states {
            let s = state.to_string();
            let parsed: SubIssueState = s.parse().unwrap();
            assert_eq!(*state, parsed);
        }
    }

    #[test]
    fn test_issue_state_classification() {
        assert!(IssueState::Discovered.is_active());
        assert!(IssueState::SpecDrafting.is_active());
        assert!(IssueState::Decomposing.is_active());
        assert!(IssueState::AgentsWorking.is_active());
        assert!(!IssueState::AwaitSpecApproval.is_active());
        assert!(!IssueState::Done.is_active());

        assert!(IssueState::AwaitSpecApproval.is_blocking());
        assert!(IssueState::AwaitDecompApproval.is_blocking());
        assert!(IssueState::AwaitSubPRApprovals.is_blocking());
        assert!(!IssueState::Discovered.is_blocking());

        assert!(IssueState::Done.is_terminal());
        assert!(IssueState::Failed.is_terminal());
        assert!(!IssueState::Discovered.is_terminal());
    }

    #[test]
    fn test_sort_priority() {
        assert!(IssueState::Failed.sort_priority() < IssueState::Discovered.sort_priority());
        assert!(IssueState::Discovered.sort_priority() < IssueState::AwaitSpecApproval.sort_priority());
        assert!(IssueState::AwaitSpecApproval.sort_priority() < IssueState::Done.sort_priority());
    }

    #[test]
    fn test_invalid_state_parse() {
        assert!(IssueState::from_str("invalid").is_err());
        assert!(SubIssueState::from_str("unknown").is_err());
    }
}
