//! Live progress-comment aggregator for ACP agent runs.
//!
//! The transition pipeline feeds [`AgentEvent`]s from the active agent into
//! this task, which turns them into a single rolling GitHub comment on the
//! underlying issue. Updates are throttled to a minimum interval so we don't
//! hammer the GitHub API on chatty agents.
//!
//! ClaudeCliAgent never emits events; for that agent kind this task silently
//! exits when the channel closes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::agents::{AgentEvent, ToolInvocation, ToolStatus};
use crate::github::GitHubClient;

/// Drain an `AgentEvent` stream, rendering progress updates to a single
/// GitHub comment on `issue_number`. Returns when the sender is dropped.
pub async fn run_aggregator(
    github: Arc<dyn GitHubClient>,
    issue_number: u64,
    mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    throttle: Duration,
) {
    let mut state = AggregatorState::new();
    let mut comment_id: Option<u64> = None;
    let mut last_render: Option<Instant> = None;

    while let Some(event) = rx.recv().await {
        state.apply(event);

        // First update fires immediately; subsequent ones are throttled.
        let due = match last_render {
            None => true,
            Some(t) => t.elapsed() >= throttle,
        };
        if due {
            let body = state.render(false);
            comment_id = post_or_update(&*github, issue_number, comment_id, &body).await;
            last_render = Some(Instant::now());
        }
    }

    // Final update on stream close — "collapsed" flag hides the block under
    // a <details> so completed runs stay tidy in the issue thread.
    if state.has_visible_events() {
        let body = state.render(true);
        let _ = post_or_update(&*github, issue_number, comment_id, &body).await;
    }
}

async fn post_or_update(
    github: &dyn GitHubClient,
    issue_number: u64,
    existing: Option<u64>,
    body: &str,
) -> Option<u64> {
    match existing {
        Some(id) => {
            if let Err(e) = github.update_issue_comment(id, body).await {
                tracing::warn!(
                    issue = issue_number,
                    comment = id,
                    error = %e,
                    "progress comment update failed"
                );
            }
            Some(id)
        }
        None => match github.post_issue_comment(issue_number, body).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(
                    issue = issue_number,
                    error = %e,
                    "progress comment post failed; giving up"
                );
                None
            }
        },
    }
}

struct AggregatorState {
    tools: Vec<ToolInvocation>,
    thinking: bool,
}

impl AggregatorState {
    fn new() -> Self {
        Self {
            tools: Vec::new(),
            thinking: false,
        }
    }

    fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(_) => {}
            AgentEvent::Thinking => self.thinking = true,
            AgentEvent::ToolStarted { id: _, title } => {
                if title.is_empty() {
                    return;
                }
                if let Some(existing) = self.tools.iter_mut().find(|t| t.title == title) {
                    existing.status = ToolStatus::Running;
                } else {
                    self.tools.push(ToolInvocation {
                        title,
                        status: ToolStatus::Running,
                    });
                }
            }
            AgentEvent::ToolFinished { id: _, title, ok } => {
                let status = if ok {
                    ToolStatus::Completed
                } else {
                    ToolStatus::Failed
                };
                if title.is_empty() {
                    return;
                }
                if let Some(existing) = self.tools.iter_mut().find(|t| t.title == title) {
                    existing.status = status;
                } else {
                    self.tools.push(ToolInvocation { title, status });
                }
            }
            AgentEvent::ConfigChanged { .. } => {}
        }
    }

    fn has_visible_events(&self) -> bool {
        !self.tools.is_empty() || self.thinking
    }

    fn render(&self, collapsed: bool) -> String {
        let mut body = String::new();
        let summary = format!(
            "🛠️ Agent progress ({} tool{})",
            self.tools.len(),
            if self.tools.len() == 1 { "" } else { "s" }
        );
        if collapsed {
            body.push_str(&format!("<details>\n<summary>{summary}</summary>\n\n"));
        } else {
            body.push_str(&format!("**{summary}**\n\n"));
        }
        if self.tools.is_empty() {
            body.push_str("_no tools invoked yet_\n");
        } else {
            for tool in &self.tools {
                let marker = match tool.status {
                    ToolStatus::Running => "⏳",
                    ToolStatus::Completed => "✅",
                    ToolStatus::Failed => "❌",
                };
                body.push_str(&format!("- {marker} {}\n", tool.title));
            }
        }
        if collapsed {
            body.push_str("\n</details>\n");
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentEvent;

    #[test]
    fn render_collapses_under_details_when_final() {
        let mut st = AggregatorState::new();
        st.apply(AgentEvent::ToolStarted {
            id: "a".into(),
            title: "Read foo.rs".into(),
        });
        st.apply(AgentEvent::ToolFinished {
            id: "a".into(),
            title: "Read foo.rs".into(),
            ok: true,
        });
        let body = st.render(true);
        assert!(body.contains("<details>"));
        assert!(body.contains("✅ Read foo.rs"));
    }

    #[test]
    fn render_open_form_omits_details_tag() {
        let mut st = AggregatorState::new();
        st.apply(AgentEvent::ToolStarted {
            id: "b".into(),
            title: "Edit bar.rs".into(),
        });
        let body = st.render(false);
        assert!(!body.contains("<details>"));
        assert!(body.contains("⏳ Edit bar.rs"));
    }

    #[test]
    fn thinking_only_is_visible() {
        let mut st = AggregatorState::new();
        st.apply(AgentEvent::Thinking);
        assert!(st.has_visible_events());
    }

    #[test]
    fn apply_upgrades_running_to_completed_by_title() {
        let mut st = AggregatorState::new();
        st.apply(AgentEvent::ToolStarted {
            id: "tc-1".into(),
            title: "Run tests".into(),
        });
        st.apply(AgentEvent::ToolFinished {
            id: "tc-1".into(),
            title: "Run tests".into(),
            ok: false,
        });
        assert_eq!(st.tools.len(), 1);
        assert_eq!(st.tools[0].status, ToolStatus::Failed);
    }

    #[tokio::test]
    async fn aggregator_posts_initial_and_final_updates() {
        use crate::github::mock::MockGitHubClient;

        let gh = Arc::new(MockGitHubClient::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let throttle = Duration::from_millis(10);
        let handle = tokio::spawn(run_aggregator(gh.clone(), 42, rx, throttle));

        tx.send(AgentEvent::ToolStarted {
            id: "t".into(),
            title: "Read file".into(),
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(AgentEvent::ToolFinished {
            id: "t".into(),
            title: "Read file".into(),
            ok: true,
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        let posted = gh.created_comments.lock().unwrap();
        let updated = gh.updated_comments.lock().unwrap();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].0, 42);
        // Final <details> update runs via update_issue_comment against the
        // comment id returned from the initial post.
        assert!(!updated.is_empty(), "final update not applied");
        assert!(updated.last().unwrap().1.contains("<details>"));
    }

    #[tokio::test]
    async fn aggregator_is_silent_when_no_events_arrive() {
        use crate::github::mock::MockGitHubClient;

        let gh = Arc::new(MockGitHubClient::new());
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let throttle = Duration::from_millis(10);
        let handle = tokio::spawn(run_aggregator(gh.clone(), 99, rx, throttle));
        drop(tx);
        handle.await.unwrap();

        assert!(gh.created_comments.lock().unwrap().is_empty());
        assert!(gh.updated_comments.lock().unwrap().is_empty());
    }
}
