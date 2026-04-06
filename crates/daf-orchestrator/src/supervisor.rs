//! Agent supervision — Erlang-inspired fault-tolerance strategies.
//!
//! The supervisor monitors agent health and applies restart strategies when
//! agents fail. Three classic strategies are supported:
//!
//! - **OneForOne** — only the failed agent is restarted.
//! - **AllForOne** — all supervised agents are restarted when any one fails.
//! - **RestForOne** — the failed agent and all agents registered after it
//!   are restarted.
//!
//! A restart budget (`max_restarts` within a `time_window`) prevents restart
//! storms from consuming resources indefinitely. When the budget is exhausted
//! the supervisor escalates by emitting a [`SupervisorEvent::RestartLimitExceeded`]
//! and transitions to a dead state.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use daf_core::agent::AgentId;
use daf_core::error::DafResult;

// ---------------------------------------------------------------------------
// SupervisorStrategy
// ---------------------------------------------------------------------------

/// Restart strategy determining which agents are restarted when one fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorStrategy {
    /// Restart only the failed agent. Other agents are unaffected.
    OneForOne,
    /// Restart all supervised agents when any single one fails.
    /// Use when agents are tightly coupled and share state.
    AllForOne,
    /// Restart the failed agent and all agents that were registered after it.
    /// Models a dependency chain where later agents depend on earlier ones.
    RestForOne,
}

impl fmt::Display for SupervisorStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OneForOne => write!(f, "one_for_one"),
            Self::AllForOne => write!(f, "all_for_one"),
            Self::RestForOne => write!(f, "rest_for_one"),
        }
    }
}

// ---------------------------------------------------------------------------
// RestartPolicy
// ---------------------------------------------------------------------------

/// Controls the restart budget: how many restarts are allowed within a
/// sliding time window before the supervisor gives up and escalates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartPolicy {
    /// Maximum number of restarts allowed within `time_window`.
    pub max_restarts: u32,
    /// Sliding window over which restarts are counted.
    pub time_window: Duration,
    /// Delay before restarting a failed agent (backoff).
    pub restart_delay: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            time_window: Duration::from_secs(60),
            restart_delay: Duration::from_secs(1),
        }
    }
}

impl RestartPolicy {
    /// Create a policy that allows no restarts — any failure is immediately
    /// escalated.
    pub fn no_restarts() -> Self {
        Self {
            max_restarts: 0,
            time_window: Duration::from_secs(60),
            restart_delay: Duration::ZERO,
        }
    }

    /// Create a lenient policy suitable for development.
    pub fn lenient() -> Self {
        Self {
            max_restarts: 20,
            time_window: Duration::from_secs(300),
            restart_delay: Duration::from_millis(500),
        }
    }
}

// ---------------------------------------------------------------------------
// SupervisorEvent
// ---------------------------------------------------------------------------

/// Events emitted by the supervisor during failure handling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorEvent {
    /// An agent failed and the supervisor is handling it.
    AgentFailed {
        /// The agent that failed.
        agent_id: AgentId,
        /// Human-readable failure reason.
        reason: String,
        /// When the failure was detected.
        timestamp: DateTime<Utc>,
    },
    /// An agent was successfully restarted.
    AgentRestarted {
        /// The agent that was restarted.
        agent_id: AgentId,
        /// Which restart attempt this is (1-based).
        attempt: u32,
        /// When the restart occurred.
        timestamp: DateTime<Utc>,
    },
    /// The restart budget has been exhausted — the supervisor is escalating.
    RestartLimitExceeded {
        /// The agent whose failures triggered the limit.
        agent_id: AgentId,
        /// Number of restarts consumed.
        restarts_used: u32,
        /// The policy that was exceeded.
        max_restarts: u32,
        /// When the limit was hit.
        timestamp: DateTime<Utc>,
    },
}

impl fmt::Display for SupervisorEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentFailed {
                agent_id, reason, ..
            } => write!(f, "AgentFailed({agent_id}: {reason})"),
            Self::AgentRestarted {
                agent_id, attempt, ..
            } => write!(f, "AgentRestarted({agent_id}, attempt {attempt})"),
            Self::RestartLimitExceeded {
                agent_id,
                restarts_used,
                max_restarts,
                ..
            } => write!(
                f,
                "RestartLimitExceeded({agent_id}: {restarts_used}/{max_restarts})"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// DeadLetter
// ---------------------------------------------------------------------------

/// A dead letter captures information about a permanently failed agent
/// that could not be recovered by the supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetter {
    /// The agent that permanently failed.
    pub agent_id: AgentId,
    /// Last known failure reason.
    pub reason: String,
    /// Total restart attempts made before giving up.
    pub restart_attempts: u32,
    /// When the agent was declared permanently dead.
    pub declared_dead_at: DateTime<Utc>,
    /// The strategy that was in effect.
    pub strategy: SupervisorStrategy,
}

impl fmt::Display for DeadLetter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DeadLetter({agent} after {n} restarts: {reason})",
            agent = self.agent_id,
            n = self.restart_attempts,
            reason = self.reason,
        )
    }
}

// ---------------------------------------------------------------------------
// RestartRecord (internal)
// ---------------------------------------------------------------------------

/// Internal record tracking when a restart occurred.
#[derive(Debug, Clone)]
struct RestartRecord {
    #[allow(dead_code)]
    agent_id: AgentId,
    timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

/// Monitors and restarts agents according to a configured strategy and
/// restart budget.
///
/// The supervisor maintains an ordered list of agents (registration order
/// matters for [`RestForOne`](SupervisorStrategy::RestForOne)). When a
/// failure is reported via [`handle_failure`](Self::handle_failure), the
/// supervisor determines which agents to restart and whether the restart
/// budget has been exceeded.
pub struct Supervisor {
    /// The restart strategy.
    strategy: SupervisorStrategy,
    /// Restart budget configuration.
    policy: RestartPolicy,
    /// Ordered list of supervised agents (registration order).
    agents: Vec<AgentId>,
    /// Sliding window of recent restart timestamps for budget tracking.
    restart_history: VecDeque<RestartRecord>,
    /// Accumulated event log.
    events: Vec<SupervisorEvent>,
    /// Dead letters for agents that exceeded the restart budget.
    dead_letters: Vec<DeadLetter>,
}

impl Supervisor {
    /// Create a new supervisor with the given strategy and default policy.
    pub fn new(strategy: SupervisorStrategy) -> Self {
        Self {
            strategy,
            policy: RestartPolicy::default(),
            agents: Vec::new(),
            restart_history: VecDeque::new(),
            events: Vec::new(),
            dead_letters: Vec::new(),
        }
    }

    /// Set the restart policy.
    pub fn with_policy(mut self, policy: RestartPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Add an agent to be supervised. Registration order matters for
    /// [`RestForOne`](SupervisorStrategy::RestForOne).
    pub fn add_agent(&mut self, agent_id: AgentId) {
        if !self.agents.contains(&agent_id) {
            self.agents.push(agent_id);
            debug!(%agent_id, strategy = %self.strategy, "agent added to supervisor");
        }
    }

    /// Remove an agent from supervision.
    pub fn remove_agent(&mut self, agent_id: &AgentId) {
        self.agents.retain(|id| id != agent_id);
    }

    /// Handle a reported agent failure.
    ///
    /// Returns the list of events produced during failure handling (restarts,
    /// escalations, dead letters). The caller is responsible for acting on
    /// these events (e.g., actually restarting the agent process).
    pub fn handle_failure(
        &mut self,
        failed_agent: AgentId,
        reason: &str,
    ) -> DafResult<Vec<SupervisorEvent>> {
        info!(
            %failed_agent,
            strategy = %self.strategy,
            reason = %reason,
            "handling agent failure"
        );

        let mut produced_events = Vec::new();

        // Emit the failure event.
        let failure_event = SupervisorEvent::AgentFailed {
            agent_id: failed_agent,
            reason: reason.to_string(),
            timestamp: Utc::now(),
        };
        produced_events.push(failure_event.clone());
        self.events.push(failure_event);

        // Prune expired restart records from the sliding window.
        let window_start = Utc::now()
            - chrono::Duration::from_std(self.policy.time_window)
                .unwrap_or(chrono::Duration::seconds(60));
        while self
            .restart_history
            .front()
            .map_or(false, |r| r.timestamp < window_start)
        {
            self.restart_history.pop_front();
        }

        // Check restart budget.
        let restarts_in_window = self.restart_history.len() as u32;
        if restarts_in_window >= self.policy.max_restarts {
            warn!(
                %failed_agent,
                restarts = restarts_in_window,
                max = self.policy.max_restarts,
                "restart budget exhausted"
            );

            let exceeded_event = SupervisorEvent::RestartLimitExceeded {
                agent_id: failed_agent,
                restarts_used: restarts_in_window,
                max_restarts: self.policy.max_restarts,
                timestamp: Utc::now(),
            };
            produced_events.push(exceeded_event.clone());
            self.events.push(exceeded_event);

            let dead_letter = DeadLetter {
                agent_id: failed_agent,
                reason: reason.to_string(),
                restart_attempts: restarts_in_window,
                declared_dead_at: Utc::now(),
                strategy: self.strategy,
            };
            self.dead_letters.push(dead_letter);

            // Remove the dead agent from supervision.
            self.remove_agent(&failed_agent);

            return Ok(produced_events);
        }

        // Determine which agents to restart based on strategy.
        let agents_to_restart = self.agents_to_restart(failed_agent);

        for &agent_id in &agents_to_restart {
            let attempt = restarts_in_window + 1;
            let restart_event = SupervisorEvent::AgentRestarted {
                agent_id,
                attempt,
                timestamp: Utc::now(),
            };
            produced_events.push(restart_event.clone());
            self.events.push(restart_event);

            self.restart_history.push_back(RestartRecord {
                agent_id,
                timestamp: Utc::now(),
            });

            debug!(
                %agent_id,
                attempt,
                delay = ?self.policy.restart_delay,
                "scheduled agent restart"
            );
        }

        Ok(produced_events)
    }

    /// Determine which agents should be restarted based on the strategy.
    fn agents_to_restart(&self, failed_agent: AgentId) -> Vec<AgentId> {
        match self.strategy {
            SupervisorStrategy::OneForOne => {
                if self.agents.contains(&failed_agent) {
                    vec![failed_agent]
                } else {
                    vec![]
                }
            }
            SupervisorStrategy::AllForOne => self.agents.clone(),
            SupervisorStrategy::RestForOne => {
                if let Some(pos) = self.agents.iter().position(|id| *id == failed_agent) {
                    self.agents[pos..].to_vec()
                } else {
                    vec![]
                }
            }
        }
    }

    /// Return the current strategy.
    pub fn strategy(&self) -> SupervisorStrategy {
        self.strategy
    }

    /// Return the current restart policy.
    pub fn policy(&self) -> &RestartPolicy {
        &self.policy
    }

    /// Return the list of supervised agents (in registration order).
    pub fn agents(&self) -> &[AgentId] {
        &self.agents
    }

    /// Return the accumulated event log.
    pub fn event_log(&self) -> &[SupervisorEvent] {
        &self.events
    }

    /// Return all dead letters (permanently failed agents).
    pub fn dead_letters(&self) -> &[DeadLetter] {
        &self.dead_letters
    }

    /// Return the number of restarts within the current sliding window.
    pub fn restarts_in_window(&self) -> u32 {
        let window_start = Utc::now()
            - chrono::Duration::from_std(self.policy.time_window)
                .unwrap_or(chrono::Duration::seconds(60));
        self.restart_history
            .iter()
            .filter(|r| r.timestamp >= window_start)
            .count() as u32
    }

    /// Check whether the supervisor has any remaining restart budget.
    pub fn has_restart_budget(&self) -> bool {
        self.restarts_in_window() < self.policy.max_restarts
    }

    /// Clear the event log (useful after events have been processed).
    pub fn clear_events(&mut self) {
        self.events.clear();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_for_one_restarts_only_failed() {
        let a = AgentId::new();
        let b = AgentId::new();
        let c = AgentId::new();

        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne)
            .with_policy(RestartPolicy::default());
        sup.add_agent(a);
        sup.add_agent(b);
        sup.add_agent(c);

        let events = sup.handle_failure(b, "OOM").unwrap();

        // Should have: 1 AgentFailed + 1 AgentRestarted
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            SupervisorEvent::AgentFailed { agent_id, .. } if *agent_id == b
        ));
        assert!(matches!(
            &events[1],
            SupervisorEvent::AgentRestarted { agent_id, .. } if *agent_id == b
        ));
    }

    #[test]
    fn all_for_one_restarts_everyone() {
        let a = AgentId::new();
        let b = AgentId::new();
        let c = AgentId::new();

        let mut sup = Supervisor::new(SupervisorStrategy::AllForOne)
            .with_policy(RestartPolicy::default());
        sup.add_agent(a);
        sup.add_agent(b);
        sup.add_agent(c);

        let events = sup.handle_failure(b, "crash").unwrap();

        // 1 failure + 3 restarts (a, b, c)
        assert_eq!(events.len(), 4);
        let restart_ids: Vec<AgentId> = events
            .iter()
            .filter_map(|e| match e {
                SupervisorEvent::AgentRestarted { agent_id, .. } => Some(*agent_id),
                _ => None,
            })
            .collect();
        assert_eq!(restart_ids.len(), 3);
        assert!(restart_ids.contains(&a));
        assert!(restart_ids.contains(&b));
        assert!(restart_ids.contains(&c));
    }

    #[test]
    fn rest_for_one_restarts_from_failed_onward() {
        let a = AgentId::new();
        let b = AgentId::new();
        let c = AgentId::new();

        let mut sup = Supervisor::new(SupervisorStrategy::RestForOne)
            .with_policy(RestartPolicy::default());
        sup.add_agent(a);
        sup.add_agent(b);
        sup.add_agent(c);

        let events = sup.handle_failure(b, "error").unwrap();

        // 1 failure + 2 restarts (b, c)
        assert_eq!(events.len(), 3);
        let restart_ids: Vec<AgentId> = events
            .iter()
            .filter_map(|e| match e {
                SupervisorEvent::AgentRestarted { agent_id, .. } => Some(*agent_id),
                _ => None,
            })
            .collect();
        assert_eq!(restart_ids.len(), 2);
        assert!(restart_ids.contains(&b));
        assert!(restart_ids.contains(&c));
        assert!(!restart_ids.contains(&a));
    }

    #[test]
    fn restart_budget_exhaustion() {
        let a = AgentId::new();

        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne).with_policy(RestartPolicy {
            max_restarts: 2,
            time_window: Duration::from_secs(60),
            restart_delay: Duration::ZERO,
        });
        sup.add_agent(a);

        // First failure -> restart (1/2 budget used).
        let events1 = sup.handle_failure(a, "fail-1").unwrap();
        assert_eq!(events1.len(), 2); // failed + restarted

        // Second failure -> restart (2/2 budget used).
        let events2 = sup.handle_failure(a, "fail-2").unwrap();
        assert_eq!(events2.len(), 2);

        // Third failure -> budget exceeded, dead letter.
        let events3 = sup.handle_failure(a, "fail-3").unwrap();
        assert!(events3
            .iter()
            .any(|e| matches!(e, SupervisorEvent::RestartLimitExceeded { .. })));
        assert_eq!(sup.dead_letters().len(), 1);
        assert!(!sup.agents().contains(&a));
    }

    #[test]
    fn no_restart_policy() {
        let a = AgentId::new();
        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne)
            .with_policy(RestartPolicy::no_restarts());
        sup.add_agent(a);

        let events = sup.handle_failure(a, "instant-death").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, SupervisorEvent::RestartLimitExceeded { .. })));
    }

    #[test]
    fn add_and_remove_agent() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne);
        sup.add_agent(a);
        sup.add_agent(b);
        assert_eq!(sup.agents().len(), 2);

        sup.remove_agent(&a);
        assert_eq!(sup.agents().len(), 1);
        assert!(!sup.agents().contains(&a));
    }

    #[test]
    fn duplicate_add_is_idempotent() {
        let a = AgentId::new();
        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne);
        sup.add_agent(a);
        sup.add_agent(a);
        assert_eq!(sup.agents().len(), 1);
    }

    #[test]
    fn strategy_display() {
        assert_eq!(SupervisorStrategy::OneForOne.to_string(), "one_for_one");
        assert_eq!(SupervisorStrategy::AllForOne.to_string(), "all_for_one");
        assert_eq!(SupervisorStrategy::RestForOne.to_string(), "rest_for_one");
    }

    #[test]
    fn dead_letter_display() {
        let dl = DeadLetter {
            agent_id: AgentId::new(),
            reason: "OOM".into(),
            restart_attempts: 5,
            declared_dead_at: Utc::now(),
            strategy: SupervisorStrategy::OneForOne,
        };
        let s = dl.to_string();
        assert!(s.contains("5 restarts"));
        assert!(s.contains("OOM"));
    }

    #[test]
    fn event_log_and_clear() {
        let a = AgentId::new();
        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne)
            .with_policy(RestartPolicy::default());
        sup.add_agent(a);

        sup.handle_failure(a, "test").unwrap();
        assert!(!sup.event_log().is_empty());

        sup.clear_events();
        assert!(sup.event_log().is_empty());
    }

    #[test]
    fn has_restart_budget() {
        let mut sup = Supervisor::new(SupervisorStrategy::OneForOne)
            .with_policy(RestartPolicy {
                max_restarts: 1,
                time_window: Duration::from_secs(60),
                restart_delay: Duration::ZERO,
            });
        let a = AgentId::new();
        sup.add_agent(a);

        assert!(sup.has_restart_budget());
        sup.handle_failure(a, "once").unwrap();
        assert!(!sup.has_restart_budget());
    }
}
