//! Process-global registry of active ACP subprocess PGIDs.
//!
//! Each `Session` owns a process group (see `spawn::spawn_child`); the
//! per-session `Drop` impl already calls `unix_kill_subtree` on normal
//! teardown. This registry adds a second fan-out so the daemon can issue
//! a SIGTERM to every live ACP child when it receives its own SIGTERM —
//! without waiting for the owning `JoinSet` tasks to finish cooperatively.
//!
//! The registry is opt-in. Tests that exercise `Session::start` directly
//! don't need it, and production `AcpAgent` passes an `Arc` clone down to
//! each `invoke`. A `SessionGuard` RAII helper keeps registration balanced
//! across every exit path (including early returns from `invoke`).

use std::collections::HashSet;
use std::sync::Mutex;

use crate::acp::spawn;

/// Opaque bag of PGIDs. Lookups are by PGID; tracking is coarse because
/// the only operation that matters at shutdown is "kill everything".
#[derive(Default)]
pub struct AcpSessionRegistry {
    pgids: Mutex<HashSet<i32>>,
}

impl AcpSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, pgid: i32) {
        if pgid <= 0 {
            return;
        }
        if let Ok(mut guard) = self.pgids.lock() {
            guard.insert(pgid);
        }
    }

    pub fn unregister(&self, pgid: i32) {
        if let Ok(mut guard) = self.pgids.lock() {
            guard.remove(&pgid);
        }
    }

    /// Send SIGTERM (then SIGKILL 1.5s later, via the usual helper) to every
    /// PGID currently registered. Safe to call from the shutdown path; any
    /// session that completes cooperatively has already unregistered, so at
    /// most this kills the genuinely in-flight agents.
    pub fn kill_all(&self) -> usize {
        let pgids: Vec<i32> = self
            .pgids
            .lock()
            .map(|g| g.iter().copied().collect())
            .unwrap_or_default();
        for pgid in &pgids {
            spawn::kill_subtree(Some(*pgid));
        }
        pgids.len()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.pgids.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// RAII guard that registers a PGID on construction and unregisters on drop.
/// Callers should hold one for the lifetime of each live `Session` so the
/// registry never leaks an entry for a session that completed or errored.
pub struct SessionGuard {
    registry: std::sync::Arc<AcpSessionRegistry>,
    pgid: i32,
}

impl SessionGuard {
    pub fn new(registry: std::sync::Arc<AcpSessionRegistry>, pgid: i32) -> Self {
        registry.register(pgid);
        Self { registry, pgid }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.registry.unregister(self.pgid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn register_and_unregister_are_balanced() {
        let reg = Arc::new(AcpSessionRegistry::new());
        {
            let _g1 = SessionGuard::new(reg.clone(), 1001);
            let _g2 = SessionGuard::new(reg.clone(), 1002);
            assert_eq!(reg.len(), 2);
        }
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn non_positive_pgid_ignored() {
        let reg = AcpSessionRegistry::new();
        reg.register(0);
        reg.register(-1);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn kill_all_clears_nothing_by_itself() {
        // kill_all enumerates a snapshot; registered entries stay registered
        // until their owning SessionGuard drops. That keeps the shutdown
        // ordering (kill -> guard drop -> unregister) well-defined.
        let reg = Arc::new(AcpSessionRegistry::new());
        let _g = SessionGuard::new(reg.clone(), 9_999_999);
        let killed = reg.kill_all();
        assert_eq!(killed, 1);
        assert_eq!(reg.len(), 1, "kill_all itself does not unregister");
    }
}
