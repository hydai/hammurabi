use crate::error::HammurabiError;
use crate::models::IssueState;

#[derive(Debug, Clone)]
pub enum Event {
    PollCycleActive,
    SpecPrMerged,
    SpecPrClosedWithoutMerge,
    DecompApproved,
    DecompFeedback { body: String },
    AllAgentsDone { any_failed: bool },
    AllSubPrsMerged,
    SubPrClosedWithoutMerge,
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
    ExecuteSpecDrafting,
    ExecuteDecomposing {
        feedback: Option<String>,
    },
    ExecuteAgentWork,
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
                body: "Starting spec generation...".to_string(),
            },
            SideEffect::ExecuteSpecDrafting,
        ]),

        (IssueState::SpecDrafting, Event::PollCycleActive) => {
            Ok(vec![SideEffect::ExecuteSpecDrafting])
        }

        (IssueState::Decomposing, Event::PollCycleActive) => Ok(vec![
            SideEffect::ExecuteDecomposing { feedback: None },
        ]),

        (IssueState::AgentsWorking, Event::PollCycleActive) => {
            Ok(vec![SideEffect::ExecuteAgentWork])
        }

        // --- Blocking state approvals ---
        (IssueState::AwaitSpecApproval, Event::SpecPrMerged) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Decomposing,
                previous_state: Some(IssueState::AwaitSpecApproval),
            },
            SideEffect::PostComment {
                body: "Spec PR merged. Starting decomposition...".to_string(),
            },
            SideEffect::ExecuteDecomposing { feedback: None },
        ]),

        (IssueState::AwaitSpecApproval, Event::SpecPrClosedWithoutMerge) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Failed,
                previous_state: Some(IssueState::AwaitSpecApproval),
            },
            SideEffect::SetError {
                message: "Spec PR was closed without merge".to_string(),
            },
            SideEffect::PostComment {
                body: "Spec PR was closed without merge. Issue marked as failed. Use `/retry` to regenerate."
                    .to_string(),
            },
        ]),

        (IssueState::AwaitDecompApproval, Event::DecompApproved) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::AgentsWorking,
                previous_state: Some(IssueState::AwaitDecompApproval),
            },
            SideEffect::PostComment {
                body: "Decomposition approved. Spawning agents...".to_string(),
            },
            SideEffect::ExecuteAgentWork,
        ]),

        (IssueState::AwaitDecompApproval, Event::DecompFeedback { body }) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Decomposing,
                previous_state: Some(IssueState::AwaitDecompApproval),
            },
            SideEffect::PostComment {
                body: "Feedback received. Re-running decomposition...".to_string(),
            },
            SideEffect::ExecuteDecomposing {
                feedback: Some(body.clone()),
            },
        ]),

        // --- Agent completion ---
        (IssueState::AgentsWorking, Event::AllAgentsDone { any_failed: false }) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::AwaitSubPRApprovals,
                previous_state: Some(IssueState::AgentsWorking),
            },
            SideEffect::PostComment {
                body: "All agents completed. PRs open for review.".to_string(),
            },
        ]),

        (IssueState::AgentsWorking, Event::AllAgentsDone { any_failed: true }) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Failed,
                previous_state: Some(IssueState::AgentsWorking),
            },
            SideEffect::SetError {
                message: "One or more agents failed".to_string(),
            },
            SideEffect::PostComment {
                body: "One or more agents failed. Use `/retry` to re-run failed sub-issues."
                    .to_string(),
            },
        ]),

        // --- Sub-PR completion ---
        (IssueState::AwaitSubPRApprovals, Event::AllSubPrsMerged) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Done,
                previous_state: Some(IssueState::AwaitSubPRApprovals),
            },
            SideEffect::PostComment {
                body: "All sub-issue PRs merged. Issue complete!".to_string(),
            },
        ]),

        (IssueState::AwaitSubPRApprovals, Event::SubPrClosedWithoutMerge) => Ok(vec![
            SideEffect::UpdateState {
                new_state: IssueState::Failed,
                previous_state: Some(IssueState::AwaitSubPRApprovals),
            },
            SideEffect::SetError {
                message: "A sub-issue PR was closed without merge".to_string(),
            },
            SideEffect::PostComment {
                body: "A sub-issue PR was closed without merge. Issue marked as failed. Use `/retry` to retry."
                    .to_string(),
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
                body: "Issue reset to Discovered state.".to_string(),
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

        // --- Transition error from blocking states (e.g., PR closed) ---
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
        assert!(effects.contains(&SideEffect::ExecuteSpecDrafting));
    }

    #[test]
    fn test_spec_drafting_poll() {
        let effects = transition(IssueState::SpecDrafting, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteSpecDrafting));
    }

    #[test]
    fn test_await_spec_approval_merged() {
        let effects =
            transition(IssueState::AwaitSpecApproval, Event::SpecPrMerged, None).unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Decomposing,
            previous_state: Some(IssueState::AwaitSpecApproval),
        }));
        assert!(effects.contains(&SideEffect::ExecuteDecomposing { feedback: None }));
    }

    #[test]
    fn test_await_spec_approval_closed() {
        let effects = transition(
            IssueState::AwaitSpecApproval,
            Event::SpecPrClosedWithoutMerge,
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::AwaitSpecApproval),
        }));
    }

    #[test]
    fn test_decomposing_poll() {
        let effects = transition(IssueState::Decomposing, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteDecomposing { feedback: None }));
    }

    #[test]
    fn test_await_decomp_approved() {
        let effects =
            transition(IssueState::AwaitDecompApproval, Event::DecompApproved, None).unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::AgentsWorking,
            previous_state: Some(IssueState::AwaitDecompApproval),
        }));
        assert!(effects.contains(&SideEffect::ExecuteAgentWork));
    }

    #[test]
    fn test_await_decomp_feedback() {
        let effects = transition(
            IssueState::AwaitDecompApproval,
            Event::DecompFeedback {
                body: "add more detail".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Decomposing,
            previous_state: Some(IssueState::AwaitDecompApproval),
        }));
        assert!(effects.contains(&SideEffect::ExecuteDecomposing {
            feedback: Some("add more detail".to_string()),
        }));
    }

    #[test]
    fn test_agents_working_poll() {
        let effects = transition(IssueState::AgentsWorking, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteAgentWork));
    }

    #[test]
    fn test_agents_done_success() {
        let effects = transition(
            IssueState::AgentsWorking,
            Event::AllAgentsDone { any_failed: false },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::AwaitSubPRApprovals,
            previous_state: Some(IssueState::AgentsWorking),
        }));
    }

    #[test]
    fn test_agents_done_failure() {
        let effects = transition(
            IssueState::AgentsWorking,
            Event::AllAgentsDone { any_failed: true },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::AgentsWorking),
        }));
    }

    #[test]
    fn test_all_sub_prs_merged() {
        let effects =
            transition(IssueState::AwaitSubPRApprovals, Event::AllSubPrsMerged, None).unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Done,
            previous_state: Some(IssueState::AwaitSubPRApprovals),
        }));
    }

    #[test]
    fn test_sub_pr_closed() {
        let effects = transition(
            IssueState::AwaitSubPRApprovals,
            Event::SubPrClosedWithoutMerge,
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Failed,
            previous_state: Some(IssueState::AwaitSubPRApprovals),
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
    fn test_reset_from_any_state() {
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
            IssueState::Decomposing,
            IssueState::AgentsWorking,
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
    fn test_invalid_transition() {
        let result = transition(IssueState::Done, Event::PollCycleActive, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_feedback_loop_cycle() {
        // AwaitDecompApproval → Decomposing (feedback) → would produce AwaitDecompApproval after execution
        let effects = transition(
            IssueState::AwaitDecompApproval,
            Event::DecompFeedback {
                body: "needs more detail".to_string(),
            },
            None,
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::Decomposing,
            previous_state: Some(IssueState::AwaitDecompApproval),
        }));

        // Then decomposing can execute on poll
        let effects = transition(IssueState::Decomposing, Event::PollCycleActive, None).unwrap();
        assert!(effects.contains(&SideEffect::ExecuteDecomposing { feedback: None }));
    }

    #[test]
    fn test_retry_from_failed_agents_working() {
        let effects = transition(
            IssueState::Failed,
            Event::RetryRequested,
            Some(IssueState::AgentsWorking),
        )
        .unwrap();
        assert!(effects.contains(&SideEffect::UpdateState {
            new_state: IssueState::AgentsWorking,
            previous_state: None,
        }));
    }
}
