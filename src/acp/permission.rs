//! Policy for responding to the agent's `session/request_permission` calls.
//!
//! The agent presents a list of options with `kind` markers; we pick the most
//! permissive selectable one. Hammurabi runs in a controlled worktree with
//! the same trust posture as `claude --dangerously-skip-permissions`, so
//! auto-approving is consistent with current behavior.

use serde::Deserialize;
use serde_json::{json, Value};

/// One permission option from a `session/request_permission` params payload.
#[derive(Debug, Clone, Deserialize)]
pub struct PermissionOption {
    #[serde(rename = "optionId")]
    pub option_id: String,
    pub kind: String,
}

/// What we decide to do with a permission prompt.
#[derive(Debug, PartialEq, Eq)]
pub enum PermissionOutcome {
    Selected(String),
    Cancelled,
}

/// Priority ranking — higher wins.
fn kind_rank(kind: &str) -> i32 {
    match kind {
        "allow_always" => 3,
        "allow_once" => 2,
        "reject_once" => -1,
        "reject_always" => -1,
        // Any unknown kind sits between allow_once and reject — treat as
        // selectable (the agent wouldn't have offered it if it were dangerous,
        // and some agents use custom kinds like `workspace_write`).
        _ => 1,
    }
}

/// Pick the best option from a list of candidates. Returns `Cancelled` if
/// every option is explicitly a reject.
pub fn decide(options: &[PermissionOption]) -> PermissionOutcome {
    let mut best: Option<(i32, &PermissionOption)> = None;
    for opt in options {
        let rank = kind_rank(&opt.kind);
        if rank < 0 {
            continue;
        }
        match best {
            Some((r, _)) if r >= rank => {}
            _ => best = Some((rank, opt)),
        }
    }
    match best {
        Some((_, opt)) => PermissionOutcome::Selected(opt.option_id.clone()),
        None => PermissionOutcome::Cancelled,
    }
}

/// Produce the `result` payload for a `session/request_permission` response.
///
/// For legacy agents that predate the `options` array, default to
/// `{"outcome": "selected", "optionId": "allow_always"}` — this mirrors the
/// behavior Claude-side tooling expected before the spec codified options.
pub fn build_response(params: Option<&Value>) -> Value {
    let options: Option<Vec<PermissionOption>> = params
        .and_then(|p| p.get("options"))
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    match options {
        None => json!({
            "outcome": {"outcome": "selected", "optionId": "allow_always"}
        }),
        Some(opts) => match decide(&opts) {
            PermissionOutcome::Selected(id) => json!({
                "outcome": {"outcome": "selected", "optionId": id}
            }),
            PermissionOutcome::Cancelled => json!({
                "outcome": {"outcome": "cancelled"}
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(id: &str, kind: &str) -> PermissionOption {
        PermissionOption {
            option_id: id.to_string(),
            kind: kind.to_string(),
        }
    }

    #[test]
    fn allow_always_wins_over_allow_once() {
        let opts = vec![
            opt("once", "allow_once"),
            opt("always", "allow_always"),
            opt("no", "reject_once"),
        ];
        assert_eq!(decide(&opts), PermissionOutcome::Selected("always".into()));
    }

    #[test]
    fn allow_once_wins_over_unknown_kind() {
        let opts = vec![opt("custom", "workspace_write"), opt("once", "allow_once")];
        assert_eq!(decide(&opts), PermissionOutcome::Selected("once".into()));
    }

    #[test]
    fn unknown_kind_selected_when_no_allow_present() {
        let opts = vec![
            opt("reject", "reject_once"),
            opt("custom", "workspace_write"),
        ];
        assert_eq!(decide(&opts), PermissionOutcome::Selected("custom".into()));
    }

    #[test]
    fn all_reject_returns_cancelled() {
        let opts = vec![opt("one", "reject_once"), opt("always", "reject_always")];
        assert_eq!(decide(&opts), PermissionOutcome::Cancelled);
    }

    #[test]
    fn empty_options_returns_cancelled() {
        assert_eq!(decide(&[]), PermissionOutcome::Cancelled);
    }

    #[test]
    fn claude_option_fixtures_pick_bypass_permissions() {
        // The real set Claude's ACP adapter emits for ExitPlanMode etc.
        let opts = vec![
            opt("bypassPermissions", "allow_always"),
            opt("acceptEdits", "allow_always"),
            opt("default", "allow_once"),
            opt("plan", "reject_once"),
        ];
        // Two allow_always — we return the first encountered.
        let outcome = decide(&opts);
        match outcome {
            PermissionOutcome::Selected(id) => {
                assert!(id == "bypassPermissions" || id == "acceptEdits");
            }
            other => panic!("expected Selected, got {:?}", other),
        }
    }

    #[test]
    fn build_response_missing_options_uses_legacy_fallback() {
        let resp = build_response(Some(&json!({"toolCall": {"title": "x"}})));
        assert_eq!(
            resp,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}})
        );
    }

    #[test]
    fn build_response_with_options_picks_best() {
        let resp = build_response(Some(&json!({
            "options": [
                {"optionId": "a", "kind": "allow_always"},
                {"optionId": "b", "kind": "reject_once"}
            ]
        })));
        assert_eq!(
            resp,
            json!({"outcome": {"outcome": "selected", "optionId": "a"}})
        );
    }

    #[test]
    fn build_response_all_reject_returns_cancelled() {
        let resp = build_response(Some(&json!({
            "options": [
                {"optionId": "a", "kind": "reject_once"},
                {"optionId": "b", "kind": "reject_always"}
            ]
        })));
        assert_eq!(resp, json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn build_response_empty_options_returns_cancelled() {
        let resp = build_response(Some(&json!({"options": []})));
        assert_eq!(resp, json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn build_response_none_params_uses_legacy_fallback() {
        let resp = build_response(None);
        assert_eq!(
            resp,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}})
        );
    }
}
