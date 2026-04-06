//! Health monitoring for registered agents.
//!
//! Tracks heartbeat timing, consecutive failures, and derives per-agent
//! and cluster-wide health status. The [`HealthMonitor`] runs periodic
//! sweeps to detect stale agents and auto-deregister those that have
//! gone silent for too long.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// HealthStatus
// ---------------------------------------------------------------------------

/// Aggregate health classification for an agent or a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// All checks passing, latency within bounds.
    Healthy,
    /// Some checks failing or latency elevated, but still operational.
    Degraded,
    /// Multiple checks failing or agent unresponsive.
    Unhealthy,
    /// No health data available yet.
    Unknown,
}

impl HealthStatus {
    /// Derive status from consecutive failure count and thresholds.
    pub fn from_failures(consecutive: u32, degrade_at: u32, unhealthy_at: u32) -> Self {
        if consecutive >= unhealthy_at {
            Self::Unhealthy
        } else if consecutive >= degrade_at {
            Self::Degraded
        } else {
            Self::Healthy
        }
    }
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded => write!(f, "degraded"),
            Self::Unhealthy => write!(f, "unhealthy"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl Default for HealthStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

// ---------------------------------------------------------------------------
// HealthCheck
// ---------------------------------------------------------------------------

/// A single health check result for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    /// The agent that was checked.
    pub agent_id: AgentId,
    /// Derived status.
    pub status: HealthStatus,
    /// When this check was performed.
    pub checked_at: DateTime<Utc>,
    /// Round-trip latency of the health probe, if measured.
    pub latency: Option<Duration>,
    /// Free-form details (error messages, degradation reasons).
    pub details: Option<String>,
}

impl HealthCheck {
    /// Create a healthy check result.
    pub fn healthy(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            status: HealthStatus::Healthy,
            checked_at: Utc::now(),
            latency: None,
            details: None,
        }
    }

    /// Create an unhealthy check result with details.
    pub fn unhealthy(agent_id: AgentId, details: impl Into<String>) -> Self {
        Self {
            agent_id,
            status: HealthStatus::Unhealthy,
            checked_at: Utc::now(),
            latency: None,
            details: Some(details.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentHealth (per-agent tracking)
// ---------------------------------------------------------------------------

/// Internal per-agent health state maintained by the monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHealth {
    /// The agent being tracked.
    pub agent_id: AgentId,
    /// Current derived status.
    pub status: HealthStatus,
    /// When we last received a heartbeat from this agent.
    pub last_heartbeat: DateTime<Utc>,
    /// Running count of consecutive failed health checks.
    pub consecutive_failures: u32,
    /// Last health check result.
    pub last_check: Option<HealthCheck>,
    /// Total successful checks since registration.
    pub total_checks: u64,
    /// Total failed checks since registration.
    pub total_failures: u64,
}

impl AgentHealth {
    /// Create initial health state for a newly registered agent.
    pub fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            status: HealthStatus::Unknown,
            last_heartbeat: Utc::now(),
            consecutive_failures: 0,
            last_check: None,
            total_checks: 0,
            total_failures: 0,
        }
    }

    /// Record a successful heartbeat.
    pub fn record_heartbeat(&mut self) {
        self.last_heartbeat = Utc::now();
        self.consecutive_failures = 0;
        self.status = HealthStatus::Healthy;
    }

    /// Record a health check result.
    pub fn record_check(&mut self, check: HealthCheck) {
        self.total_checks += 1;
        match check.status {
            HealthStatus::Healthy => {
                self.consecutive_failures = 0;
            }
            HealthStatus::Degraded | HealthStatus::Unhealthy | HealthStatus::Unknown => {
                self.consecutive_failures += 1;
                self.total_failures += 1;
            }
        }
        self.status = check.status;
        self.last_check = Some(check);
    }

    /// Record a missed heartbeat (no response within the expected interval).
    pub fn record_missed_heartbeat(&mut self, config: &HealthConfig) {
        self.consecutive_failures += 1;
        self.total_failures += 1;
        self.status = HealthStatus::from_failures(
            self.consecutive_failures,
            config.degrade_after_failures,
            config.unhealthy_after_failures,
        );
    }

    /// Duration since the last heartbeat.
    pub fn time_since_heartbeat(&self) -> Duration {
        let elapsed = Utc::now() - self.last_heartbeat;
        elapsed.to_std().unwrap_or(Duration::ZERO)
    }

    /// Returns `true` if this agent should be auto-deregistered.
    pub fn should_deregister(&self, config: &HealthConfig) -> bool {
        self.consecutive_failures >= config.deregister_after_failures
    }

    /// Failure rate as a fraction (0.0..=1.0). Returns 0.0 if no checks.
    pub fn failure_rate(&self) -> f64 {
        if self.total_checks == 0 {
            return 0.0;
        }
        self.total_failures as f64 / self.total_checks as f64
    }
}

// ---------------------------------------------------------------------------
// HealthConfig
// ---------------------------------------------------------------------------

/// Configuration for the health monitoring subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// How often to check for stale heartbeats.
    pub check_interval: Duration,
    /// Maximum time between heartbeats before marking as missed.
    pub heartbeat_timeout: Duration,
    /// Consecutive failures before transitioning to `Degraded`.
    pub degrade_after_failures: u32,
    /// Consecutive failures before transitioning to `Unhealthy`.
    pub unhealthy_after_failures: u32,
    /// Consecutive failures before auto-deregistering the agent.
    pub deregister_after_failures: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(30),
            degrade_after_failures: 3,
            unhealthy_after_failures: 5,
            deregister_after_failures: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// HealthMonitor
// ---------------------------------------------------------------------------

/// Monitors agent health via heartbeat tracking and periodic sweeps.
///
/// The monitor does not own the registry — it operates on a snapshot of
/// agent health states. The registry calls into the monitor to record
/// heartbeats and run sweeps, then acts on the results (deregistration, etc.).
#[derive(Debug)]
pub struct HealthMonitor {
    /// Per-agent health tracking.
    agents: parking_lot::RwLock<HashMap<AgentId, AgentHealth>>,
    /// Configuration.
    config: HealthConfig,
}

impl HealthMonitor {
    /// Create a new health monitor with the given configuration.
    pub fn new(config: HealthConfig) -> Self {
        Self {
            agents: parking_lot::RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Create with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(HealthConfig::default())
    }

    /// Start tracking an agent.
    pub fn track(&self, agent_id: AgentId) {
        let mut agents = self.agents.write();
        agents.entry(agent_id).or_insert_with(|| {
            debug!(%agent_id, "health monitor: now tracking agent");
            AgentHealth::new(agent_id)
        });
    }

    /// Stop tracking an agent.
    pub fn untrack(&self, agent_id: &AgentId) {
        let mut agents = self.agents.write();
        agents.remove(agent_id);
        debug!(%agent_id, "health monitor: stopped tracking agent");
    }

    /// Record a heartbeat from an agent.
    pub fn heartbeat(&self, agent_id: &AgentId) {
        let mut agents = self.agents.write();
        if let Some(health) = agents.get_mut(agent_id) {
            health.record_heartbeat();
        }
    }

    /// Record a health check result.
    pub fn record_check(&self, check: HealthCheck) {
        let mut agents = self.agents.write();
        if let Some(health) = agents.get_mut(&check.agent_id) {
            health.record_check(check);
        }
    }

    /// Get the current health status of an agent.
    pub fn status(&self, agent_id: &AgentId) -> HealthStatus {
        let agents = self.agents.read();
        agents
            .get(agent_id)
            .map(|h| h.status)
            .unwrap_or(HealthStatus::Unknown)
    }

    /// Get the full health state for an agent.
    pub fn get_health(&self, agent_id: &AgentId) -> Option<AgentHealth> {
        let agents = self.agents.read();
        agents.get(agent_id).cloned()
    }

    /// Sweep all tracked agents: detect missed heartbeats and return the list
    /// of agent IDs that should be deregistered.
    pub fn sweep(&self) -> Vec<AgentId> {
        let mut agents = self.agents.write();
        let mut to_deregister = Vec::new();

        for (id, health) in agents.iter_mut() {
            let elapsed = health.time_since_heartbeat();
            if elapsed > self.config.heartbeat_timeout {
                health.record_missed_heartbeat(&self.config);
                warn!(
                    agent_id = %id,
                    consecutive_failures = health.consecutive_failures,
                    elapsed_secs = elapsed.as_secs(),
                    "agent missed heartbeat"
                );

                if health.should_deregister(&self.config) {
                    info!(agent_id = %id, "agent exceeded failure threshold, marking for deregistration");
                    to_deregister.push(*id);
                }
            }
        }

        to_deregister
    }

    /// Compute cluster-level health from all tracked agents.
    pub fn cluster_health(&self) -> ClusterHealth {
        let agents = self.agents.read();
        let total = agents.len();
        let mut healthy = 0u32;
        let mut degraded = 0u32;
        let mut unhealthy = 0u32;
        let mut unknown = 0u32;

        for health in agents.values() {
            match health.status {
                HealthStatus::Healthy => healthy += 1,
                HealthStatus::Degraded => degraded += 1,
                HealthStatus::Unhealthy => unhealthy += 1,
                HealthStatus::Unknown => unknown += 1,
            }
        }

        let overall = if total == 0 {
            HealthStatus::Unknown
        } else if unhealthy > 0 {
            HealthStatus::Unhealthy
        } else if degraded > 0 {
            HealthStatus::Degraded
        } else if unknown == total as u32 {
            HealthStatus::Unknown
        } else {
            HealthStatus::Healthy
        };

        ClusterHealth {
            overall,
            total_agents: total as u32,
            healthy,
            degraded,
            unhealthy,
            unknown,
        }
    }

    /// Get the health configuration.
    pub fn config(&self) -> &HealthConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// ClusterHealth
// ---------------------------------------------------------------------------

/// Aggregate health snapshot of the entire agent cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterHealth {
    /// Overall cluster status (worst-of aggregation).
    pub overall: HealthStatus,
    /// Total number of tracked agents.
    pub total_agents: u32,
    /// Agents in `Healthy` state.
    pub healthy: u32,
    /// Agents in `Degraded` state.
    pub degraded: u32,
    /// Agents in `Unhealthy` state.
    pub unhealthy: u32,
    /// Agents in `Unknown` state.
    pub unknown: u32,
}

impl ClusterHealth {
    /// Fraction of agents that are healthy (0.0..=1.0).
    pub fn health_ratio(&self) -> f64 {
        if self.total_agents == 0 {
            return 0.0;
        }
        self.healthy as f64 / self.total_agents as f64
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_from_failures() {
        assert_eq!(HealthStatus::from_failures(0, 3, 5), HealthStatus::Healthy);
        assert_eq!(HealthStatus::from_failures(3, 3, 5), HealthStatus::Degraded);
        assert_eq!(HealthStatus::from_failures(5, 3, 5), HealthStatus::Unhealthy);
        assert_eq!(HealthStatus::from_failures(10, 3, 5), HealthStatus::Unhealthy);
    }

    #[test]
    fn agent_health_heartbeat_resets_failures() {
        let id = AgentId::new();
        let mut health = AgentHealth::new(id);
        let config = HealthConfig::default();

        health.record_missed_heartbeat(&config);
        health.record_missed_heartbeat(&config);
        assert_eq!(health.consecutive_failures, 2);

        health.record_heartbeat();
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(health.status, HealthStatus::Healthy);
    }

    #[test]
    fn agent_health_deregister_threshold() {
        let id = AgentId::new();
        let mut health = AgentHealth::new(id);
        let config = HealthConfig {
            deregister_after_failures: 3,
            ..Default::default()
        };

        for _ in 0..3 {
            health.record_missed_heartbeat(&config);
        }
        assert!(health.should_deregister(&config));
    }

    #[test]
    fn agent_health_failure_rate() {
        let id = AgentId::new();
        let mut health = AgentHealth::new(id);

        // 2 successes, 1 failure = 1/3 failure rate
        health.record_check(HealthCheck::healthy(id));
        health.record_check(HealthCheck::healthy(id));
        health.record_check(HealthCheck::unhealthy(id, "timeout"));

        assert_eq!(health.total_checks, 3);
        assert!((health.failure_rate() - 1.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn monitor_track_and_heartbeat() {
        let monitor = HealthMonitor::with_defaults();
        let id = AgentId::new();

        monitor.track(id);
        assert_eq!(monitor.status(&id), HealthStatus::Unknown);

        monitor.heartbeat(&id);
        assert_eq!(monitor.status(&id), HealthStatus::Healthy);
    }

    #[test]
    fn monitor_untrack() {
        let monitor = HealthMonitor::with_defaults();
        let id = AgentId::new();

        monitor.track(id);
        monitor.untrack(&id);
        assert_eq!(monitor.status(&id), HealthStatus::Unknown);
    }

    #[test]
    fn cluster_health_empty() {
        let monitor = HealthMonitor::with_defaults();
        let ch = monitor.cluster_health();
        assert_eq!(ch.overall, HealthStatus::Unknown);
        assert_eq!(ch.total_agents, 0);
    }

    #[test]
    fn cluster_health_all_healthy() {
        let monitor = HealthMonitor::with_defaults();
        let ids: Vec<AgentId> = (0..3).map(|_| AgentId::new()).collect();
        for &id in &ids {
            monitor.track(id);
            monitor.heartbeat(&id);
        }
        let ch = monitor.cluster_health();
        assert_eq!(ch.overall, HealthStatus::Healthy);
        assert_eq!(ch.healthy, 3);
        assert!((ch.health_ratio() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cluster_health_degraded_if_any_degraded() {
        let monitor = HealthMonitor::with_defaults();
        let healthy_id = AgentId::new();
        let degraded_id = AgentId::new();

        monitor.track(healthy_id);
        monitor.heartbeat(&healthy_id);

        monitor.track(degraded_id);
        monitor.record_check(HealthCheck {
            agent_id: degraded_id,
            status: HealthStatus::Degraded,
            checked_at: Utc::now(),
            latency: None,
            details: None,
        });

        let ch = monitor.cluster_health();
        assert_eq!(ch.overall, HealthStatus::Degraded);
    }
}
