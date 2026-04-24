//! `AgentRegistry` — maps `AgentKind` to concrete `AiAgent` implementations.
//!
//! Built once at daemon startup (typically in `poller::run_daemon`) and shared
//! across every `TransitionContext`. Transitions resolve the appropriate
//! agent via `ctx.config.agent_kind_for_task(...)` + `ctx.agents.get(kind)`.

use std::collections::HashMap;
use std::sync::Arc;

use super::{AgentKind, AiAgent};
use crate::error::HammurabiError;

pub struct AgentRegistry {
    agents: HashMap<AgentKind, Arc<dyn AiAgent>>,
}

impl AgentRegistry {
    pub fn new(agents: HashMap<AgentKind, Arc<dyn AiAgent>>) -> Self {
        Self { agents }
    }

    /// Resolve an agent by kind. Returns a `Config` error if the kind is not
    /// registered — this indicates a config/registry construction bug.
    pub fn get(&self, kind: AgentKind) -> Result<Arc<dyn AiAgent>, HammurabiError> {
        self.agents.get(&kind).cloned().ok_or_else(|| {
            HammurabiError::Config(format!("agent kind {:?} is not registered", kind))
        })
    }

    #[cfg(test)]
    pub fn for_test<A>(ai: Arc<A>) -> Self
    where
        A: AiAgent + 'static,
    {
        let mut map: HashMap<AgentKind, Arc<dyn AiAgent>> = HashMap::new();
        map.insert(AgentKind::ClaudeCli, ai);
        Self { agents: map }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::mock::MockAiAgent;

    #[test]
    fn get_returns_registered_agent() {
        let mock = Arc::new(MockAiAgent::new());
        let reg = AgentRegistry::for_test(mock);
        assert!(reg.get(AgentKind::ClaudeCli).is_ok());
    }
}
