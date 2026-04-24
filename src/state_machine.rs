//! Authoritative specification of the issue lifecycle: a pure function
//! `(State, Event) -> Vec<SideEffect>` with no I/O.
//!
//! The daemon at runtime dispatches directly to the per-edge modules
//! under `src/transitions/` rather than calling through this function;
//! the two are kept in lockstep by the exhaustive test suite below. Any
//! legal transition must have a matching test case here, and any new
//! state or event must be added to both representations in the same
//! commit. Adding a state-machine test that fails is the canonical way
//! to flag a missing transition implementation.
//!
//! See `docs/architecture.md` for the full state graph.
#![allow(dead_code)]

use crate::error::HammurabiError;
use crate::models::IssueState;

pub const MSG_STARTING_SPEC: &str = "Starting spec generation...";
pub const MSG_SPEC_APPROVED: &str = "Spec approved. Starting implementation...";
pub const MSG_SPEC_FEEDBACK: &str = "Feedback received. Revising spec...";
pub const MSG_PR_MERGED: &str = "Implementation PR merged. Issue complete!";
pub const MSG_PR_FEEDBACK: &str = "PR feedback received. Revising implementation...";
pub const MSG_PR_CLOSED_ERR: &str = "Implementation PR was closed without merge";
pub const MSG_PR_CLOSED: &str =
    "Implementation PR was closed without merge. Issue marked as failed. Use `/retry` to retry.";
pub const MSG_RESET: &str = "Issue reset to Discovered state.";

#[derive(Debug, Clone)]
pub enum Event {
    PollCycleActive,
    SpecApproved,
    SpecFeedback { body: String },
    PrMerged,
    PrClosedWithoutMerge,
    PrFeedback { body: String },
    TransitionError { message: String },
    RetryRequested,
    ResetRequested,
    IssueClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideEffect {
    UpdateState {
        new_state: IssueState,
        previous_state: Option<IssueState>,
    },
    ExecuteSpecDrafting {
        feedback: Option<String>,
    },
    ExecuteImplementation,
    ExecuteReview,
    PostComment {
        body: String,
    },
    SetError {
        message: String,
    },
}

pub fn transition(
    current_state: IssueState,
    event: Event,
    previous_state: Option<IssueState>,
) -> Result<Vec<SideEffect>, HammurabiError> {
    match (current_state, &event) {
        // --- Active states on poll cycle ---
        (IssueState::Discovered, Event::PollCycleActive) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::SpecDrafting,
                previous_state: Some(IssueState::Discovered),
            },
            SideEffect::PostComment {
                body: MSG_STARTING_SPEC.to_string(),
            },
            SideEffect::ExecuteSpecDrafting { feedback: None },
        ]),

        (IssueState::SpecDrafting, Event::PollCycleActive) => {
            Ok(vec![SideEffect::ExecuteSpecDrafting { feedback: None }])
        }

        (IssueState::Implementing, Event::PollCycleActive) => {
            Ok(vec![SideEffect::ExecuteImplementation])
        }

        (IssueState::Reviewing, Event::PollCycleActive) => Ok(vec![SideEffect::ExecuteReview]),

        // --- Spec approval (comment-based) ---
        (IssueState::AwaitSpecApproval, Event::SpecApproved) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Implementing,
                previous_state: Some(IssueState::AwaitSpecApproval),
            },
            SideEffect::PostComment {
                body: MSG_SPEC_APPROVED.to_string(),
            },
            SideEffect::ExecuteImplementation,
        ]),

        (IssueState::AwaitSpecApproval, Event::SpecFeedback { body }) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::SpecDrafting,
                previous_state: Some(IssueState::AwaitSpecApproval),
            },
            SideEffect::PostComment {
                body: MSG_SPEC_FEEDBACK.to_string(),
            },
            SideEffect::ExecuteSpecDrafting {
                feedback: Some(body.clone()),
            },
        ]),

        // --- Implementation PR approval ---
        (IssueState::AwaitPRApproval, Event::PrMerged) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Done,
                previous_state: Some(IssueState::AwaitPRApproval),
            },
            SideEffect::PostComment {
                body: MSG_PR_MERGED.to_string(),
            },
        ]),

        (IssueState::AwaitPRApproval, Event::PrFeedback { body: _ }) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Implementing,
                previous_state: Some(IssueState::AwaitPRApproval),
            },
            SideEffect::PostComment {
                body: MSG_PR_FEEDBACK.to_string(),
            },
            SideEffect::ExecuteImplementation,
        ]),

        (IssueState::AwaitPRApproval, Event::PrClosedWithoutMerge) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Failed,
                previous_state: Some(IssueState::AwaitPRApproval),
            },
            SideEffect::SetError {
                message: MSG_PR_CLOSED_ERR.to_string(),
            },
            SideEffect::PostComment {
                body: MSG_PR_CLOSED.to_string(),
            },
        ]),

        // --- Retry from Failed ---
        (IssueState::Failed, Event::RetryRequested) => {
            let prev = previous_state.ok_or_else(|| {
                HammurabiError::StateMachine("no previous state to retry from".to_string())
            })?;
            Ok(vec![
                SideEffect::UpdateState {
                    new_state: prev,
                    previous_state: None,
                },
                SideEffect::PostComment {
                    body: format!("Retrying from {} state...", prev),
                },
            ])
        }

        // --- Reset from any state ---
        (_, Event::ResetRequested) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Discovered,
                previous_state: None,
            },
            SideEffect::PostComment {
                body: MSG_RESET.to_string(),
            },
        ]),

        // --- Issue closed externally ---
        (_, Event::IssueClosed) => Ok(vec![SideEffect::UpdateState {
            new_state: IssueState::Done,
            previous_state: Some(current_state),
        }]),

        // --- Transition error from any active state ---
        (state, Event::TransitionError { message }) if state.is_active() => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Failed,
                previous_state: Some(state),
            },
            SideEffect::SetError {
                message: message.clone(),
            },
            SideEffect::PostComment {
                body: format!("Error during {}: {}", state, message),
            },
        ]),

        // --- Transition error from blocking states ---
        (state, Event::TransitionError { message }) if state.is_blocking() => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Failed,
                previous_state: Some(state),
            },
            SideEffect::SetError {
                message: message.clone(),
            },
            SideEffect::PostComment {
                body: format!("Error: {}", message),
            },
        ]),

        // --- Invalid transitions ---
        (state, event) => Err(HammurabiError::StateMachine(format!(
            "invalid transition: {:?} + {:?}",
            state, event
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovered_poll() {
        let effects = transition(IssueState::Discovered, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::SpecDrafting,
            previous_state: Some(IssueState::Discovered),
        }));
        assert!(effects.contains(&SideEffect::ExecuteSpecDrafting { feedback: None }));
    }

    #[test]
    fn test_spec_drafting_poll() {
        let effects = transition(IssueState::SpecDrafting, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteSpecDrafting { feedback: None }));
    }

    #[test]
    fn test_implementing_poll() {
        let effects = transition(IssueState::Implementing, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteImplementation));
    }

    #[test]
    fn test_await_spec_approved() {
        let effects = transition(IssueState::AwaitSpecApproval, Event::SpecApproved, None).unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Implementing,
            previous_state: Some(IssueState::AwaitSpecApproval),
        }));
        assert!(effects.contains(&SideEffect::ExecuteImplementation));
    }

    #[test]
    fn test_await_spec_feedback() {
        let effects = transition(
            IssueState::AwaitSpecApproval,
            Event::SpecFeedback {
                body: "add more detail".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::SpecDrafting,
            previous_state: Some(IssueState::AwaitSpecApproval),
        }));
        assert!(effects.contains(&SideEffect::ExecuteSpecDrafting {
            feedback: Some("add more detail".to_string()),
        }));
    }

    #[test]
    fn test_pr_merged() {
        let effects = transition(IssueState::AwaitPRApproval, Event::PrMerged, None).unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Done,
            previous_state: Some(IssueState::AwaitPRApproval),
        }));
    }

    #[test]
    fn test_pr_closed_without_merge() {
        let effects = transition(
            IssueState::AwaitPRApproval,
            Event::PrClosedWithoutMerge,
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::AwaitPRApproval),
        }));
    }

    #[test]
    fn test_retry_from_failed() {
        let effects = transition(
            IssueState::Failed,
            Event::RetryRequested,
            Some(IssueState::SpecDrafting),
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::SpecDrafting,
            previous_state: None,
        }));
    }

    #[test]
    fn test_retry_no_previous_state() {
        let result = transition(IssueState::Failed, Event::RetryRequested, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_reviewing_poll() {
        let effects = transition(IssueState::Reviewing, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteReview));
    }

    #[test]
    fn test_transition_error_reviewing() {
        let effects = transition(
            IssueState::Reviewing,
            Event::TransitionError {
                message: "review failed".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::Reviewing),
        }));
        assert!(effects.contains(&SideEffect::SetError {
            message: "review failed".to_string(),
        }));
    }

    #[test]
    fn test_reset_from_any_state() {
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
            let effects = transition(*state, Event::ResetRequested, None).unwrap();
            assert!(effects.contains(&SideEffect::UpdateState {
                new_state: IssueState::Discovered,
                previous_state: None,
            }));
        }
    }

    #[test]
    fn test_issue_closed_from_any_state() {
        let states = [
            IssueState::Discovered,
            IssueState::SpecDrafting,
            IssueState::AwaitSpecApproval,
            IssueState::Implementing,
            IssueState::Reviewing,
            IssueState::AwaitPRApproval,
            IssueState::Failed,
        ];
        for state in &states {
            let effects = transition(*state, Event::IssueClosed, None).unwrap();
            assert!(effects.contains(&SideEffect::UpdateState {
                new_state: IssueState::Done,
                previous_state: Some(*state),
            }));
        }
    }

    #[test]
    fn test_transition_error_active() {
        let effects = transition(
            IssueState::SpecDrafting,
            Event::TransitionError {
                message: "agent crashed".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::SpecDrafting),
        }));
        assert!(effects.contains(&SideEffect::SetError {
            message: "agent crashed".to_string(),
        }));
    }

    #[test]
    fn test_transition_error_blocking() {
        let effects = transition(
            IssueState::AwaitSpecApproval,
            Event::TransitionError {
                message: "api error".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::AwaitSpecApproval),
        }));
    }

    #[test]
    fn test_invalid_transition() {
        let result = transition(IssueState::Done, Event::PollCycleActive, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_feedback_loop_cycle() {
        // AwaitSpecApproval → SpecDrafting (feedback)
        let effects = transition(
            IssueState::AwaitSpecApproval,
            Event::SpecFeedback {
                body: "needs more detail".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::SpecDrafting,
            previous_state: Some(IssueState::AwaitSpecApproval),
        }));

        // Then spec drafting can execute on poll
        let effects = transition(IssueState::SpecDrafting, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteSpecDrafting { feedback: None }));
    }

    #[test]
    fn test_pr_feedback() {
        let effects = transition(
            IssueState::AwaitPRApproval,
            Event::PrFeedback {
                body: "fix the error handling".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Implementing,
            previous_state: Some(IssueState::AwaitPRApproval),
        }));
        assert!(effects.contains(&SideEffect::ExecuteImplementation));
    }

    #[test]
    fn test_retry_from_failed_implementing() {
        let effects = transition(
            IssueState::Failed,
            Event::RetryRequested,
            Some(IssueState::Implementing),
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Implementing,
            previous_state: None,
        }));
    }
}
