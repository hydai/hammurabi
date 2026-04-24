//! Access control policy for intake sources.
//!
//! Intake sources (Discord channels, future Slack workspaces, ...) all face
//! the same question: "is this user allowed to submit work?". The
//! [`AllowUsers`] enum is the shared shape — either an explicit allowlist
//! or a wildcard. Using an enum rather than `Vec<String>` + a flag avoids
//! the classic bug of an empty allowlist accidentally meaning "anyone".

use serde::Deserialize;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AllowUsers {
    /// Only these usernames/IDs are allowed. Empty list means nobody.
    List(Vec<String>),
    /// Any user is allowed.
    All,
}

#[allow(dead_code)]
impl AllowUsers {
    /// Check whether `user` is permitted. Matching is case-sensitive.
    pub fn is_allowed(&self, user: &str) -> bool {
        match self {
            AllowUsers::All => true,
            AllowUsers::List(users) => users.iter().any(|u| u == user),
        }
    }

    /// Resolve from the raw TOML shape: an `allow_all_users = true` flag
    /// short-circuits to `All`; otherwise the `allowed_users` list becomes
    /// `List`. Passing both is a config error, flagged by the caller.
    pub fn from_raw(allow_all: bool, users: Vec<String>) -> Self {
        if allow_all {
            AllowUsers::All
        } else {
            AllowUsers::List(users)
        }
    }
}

/// Raw TOML shape merged into a host struct via `#[serde(flatten)]`.
/// Kept as a separate helper so each intake source can reuse it.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RawAccess {
    #[serde(default)]
    pub allow_all_users: bool,
    #[serde(default)]
    pub allow_users: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_matches_listed_user() {
        let a = AllowUsers::List(vec!["alice".into(), "bob".into()]);
        assert!(a.is_allowed("alice"));
        assert!(a.is_allowed("bob"));
        assert!(!a.is_allowed("eve"));
    }

    #[test]
    fn empty_list_allows_nobody() {
        let a = AllowUsers::List(Vec::new());
        assert!(!a.is_allowed("alice"));
    }

    #[test]
    fn all_allows_anyone() {
        let a = AllowUsers::All;
        assert!(a.is_allowed("alice"));
        assert!(a.is_allowed("eve"));
    }

    #[test]
    fn from_raw_all_short_circuits() {
        let a = AllowUsers::from_raw(true, vec!["alice".into()]);
        assert!(matches!(a, AllowUsers::All));
        assert!(a.is_allowed("anyone"));
    }

    #[test]
    fn from_raw_list_preserves_users() {
        let a = AllowUsers::from_raw(false, vec!["alice".into()]);
        match a {
            AllowUsers::List(users) => assert_eq!(users, vec!["alice".to_string()]),
            _ => panic!("expected List"),
        }
    }
}
