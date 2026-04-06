//! Core agent registry.
//!
//! The [`Registry`] is the central phone book of the DAF cluster. It stores
//! live agent instances in a lock-free [`DashMap`], tracks their health via
//! the [`HealthMonitor`](crate::health::HealthMonitor), and provides discovery
//! APIs that match capability requirements against registered agents.
//!
//! All public methods are `&self` — the registry is designed for concurrent
//! access from the orchestrator, transport layer, and health sweeper without
//! requiring an external mutex.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use daf_core::{AgentId, AgentKind, AgentManifest, AgentStatus, DafError, DafResult};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::capability::{
    score_capabilities, CapabilityRequirement, CapabilitySet, MatchScore,
};
use crate::health::{HealthMonitor, HealthStatus};
use crate::version::SemVer;

// ---------------------------------------------------------------------------
// RegistryEntry
// ---------------------------------------------------------------------------

/// A live agent entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// The agent's manifest.
    pub manifest: AgentManifest,
    /// Registry-level capability set (parsed from manifest).
    pub capabilities: CapabilitySet,
    /// Current agent status.
    pub status: AgentStatus,
    /// When this agent was registered.
    pub registered_at: DateTime<Utc>,
    /// Last heartbeat timestamp.
    pub last_heartbeat: DateTime<Utc>,
    /// Consecutive missed heartbeats / failed health checks.
    pub consecutive_failures: u32,
}

impl RegistryEntry {
    /// Create a new entry from a manifest.
    fn new(manifest: AgentManifest) -> Self {
        let capabilities = Self::parse_capabilities(&manifest);
        let now = Utc::now();
        Self {
            manifest,
            capabilities,
            status: AgentStatus::Spawning,
            registered_at: now,
            last_heartbeat: now,
            consecutive_failures: 0,
        }
    }

    /// Parse manifest capabilities into the registry's richer format.
    fn parse_capabilities(manifest: &AgentManifest) -> CapabilitySet {
        let caps = manifest.capabilities.iter().map(|c| {
            let version = c.version.parse::<SemVer>().unwrap_or(SemVer::new(0, 0, 0));
            crate::capability::Capability::new(&c.name, version, &c.description)
        });
        CapabilitySet::from_capabilities(caps)
    }
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

/// Filters for listing agents.
#[derive(Debug, Clone, Default)]
pub struct AgentFilter {
    /// Filter by agent kind.
    pub kind: Option<AgentKind>,
    /// Filter by status.
    pub status: Option<AgentStatus>,
    /// Filter by capability name (agent must have at least this capability).
    pub capability: Option<String>,
    /// Filter by health status.
    pub health: Option<HealthStatus>,
}

impl AgentFilter {
    /// Create an empty (match-all) filter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by agent kind.
    pub fn with_kind(mut self, kind: AgentKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Filter by status.
    pub fn with_status(mut self, status: AgentStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Filter by capability name.
    pub fn with_capability(mut self, cap: impl Into<String>) -> Self {
        self.capability = Some(cap.into());
        self
    }

    /// Filter by health status.
    pub fn with_health(mut self, health: HealthStatus) -> Self {
        self.health = Some(health);
        self
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The live agent registry — concurrent, lock-free, and health-aware.
///
/// This is the central coordination point for agent discovery. The orchestrator
/// registers agents as they join, queries the registry when routing tasks, and
/// the health monitor deregisters agents that go silent.
pub struct Registry {
    /// All registered agents, keyed by AgentId.
    agents: DashMap<AgentId, RegistryEntry>,
    /// Health monitoring subsystem.
    health_monitor: Arc<HealthMonitor>,
}

impl Registry {
    /// Create a new registry with the given health monitor.
    pub fn new(health_monitor: Arc<HealthMonitor>) -> Self {
        Self {
            agents: DashMap::new(),
            health_monitor,
        }
    }

    /// Create a registry with default health configuration.
    pub fn with_defaults() -> Self {
        Self::new(Arc::new(HealthMonitor::with_defaults()))
    }

    /// Register an agent manifest. Returns an error if the agent ID is
    /// already registered.
    pub fn register_agent(&self, manifest: AgentManifest) -> DafResult<()> {
        let id = manifest.id;

        if self.agents.contains_key(&id) {
            return Err(DafError::AgentError {
                agent_id: Some(id.into()),
                message: format!("agent {id} is already registered"),
            });
        }

        let entry = RegistryEntry::new(manifest);
        info!(
            agent_id = %id,
            name = %entry.manifest.name,
            kind = %entry.manifest.kind,
            capabilities = entry.capabilities.len(),
            "registering agent"
        );

        self.health_monitor.track(id);
        self.agents.insert(id, entry);
        Ok(())
    }

    /// Unregister an agent, removing it from the registry and health monitor.
    pub fn unregister_agent(&self, id: &AgentId) -> DafResult<RegistryEntry> {
        let (_, entry) = self.agents.remove(id).ok_or_else(|| DafError::NotFound {
            entity: "agent".into(),
            id: id.to_string(),
        })?;

        self.health_monitor.untrack(id);
        info!(agent_id = %id, name = %entry.manifest.name, "unregistered agent");
        Ok(entry)
    }

    /// Look up an agent by ID.
    pub fn get_agent(&self, id: &AgentId) -> Option<RegistryEntry> {
        self.agents.get(id).map(|r| r.value().clone())
    }

    /// Check if an agent is registered.
    pub fn contains(&self, id: &AgentId) -> bool {
        self.agents.contains_key(id)
    }

    /// Total number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Returns `true` if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Update the status of a registered agent.
    pub fn update_status(&self, id: &AgentId, status: AgentStatus) -> DafResult<()> {
        let mut entry = self.agents.get_mut(id).ok_or_else(|| DafError::NotFound {
            entity: "agent".into(),
            id: id.to_string(),
        })?;
        entry.status = status;
        debug!(agent_id = %id, status = %status, "updated agent status");
        Ok(())
    }

    /// Record a heartbeat from an agent.
    pub fn heartbeat(&self, id: &AgentId) -> DafResult<()> {
        let mut entry = self.agents.get_mut(id).ok_or_else(|| DafError::NotFound {
            entity: "agent".into(),
            id: id.to_string(),
        })?;
        entry.last_heartbeat = Utc::now();
        entry.consecutive_failures = 0;
        self.health_monitor.heartbeat(id);
        Ok(())
    }

    /// List all agents, optionally filtered.
    pub fn list_agents(&self, filter: Option<&AgentFilter>) -> Vec<RegistryEntry> {
        self.agents
            .iter()
            .filter(|r| {
                let entry = r.value();
                let f = match filter {
                    Some(f) => f,
                    None => return true,
                };

                if let Some(kind) = f.kind {
                    if entry.manifest.kind != kind {
                        return false;
                    }
                }

                if let Some(status) = f.status {
                    if entry.status != status {
                        return false;
                    }
                }

                if let Some(ref cap_name) = f.capability {
                    if !entry.capabilities.contains(cap_name) {
                        return false;
                    }
                }

                if let Some(health) = f.health {
                    let agent_health = self.health_monitor.status(&entry.manifest.id);
                    if agent_health != health {
                        return false;
                    }
                }

                true
            })
            .map(|r| r.value().clone())
            .collect()
    }

    /// Discover agents that match a set of capability requirements.
    ///
    /// Returns entries sorted by match score (best first). Only agents that
    /// satisfy all *required* capabilities are returned.
    pub fn discover(&self, requirements: &[CapabilityRequirement]) -> Vec<(RegistryEntry, MatchScore)> {
        // Collect capability sets and entries for scoring.
        let entries: Vec<(AgentId, RegistryEntry)> = self
            .agents
            .iter()
            .map(|r| (*r.key(), r.value().clone()))
            .collect();

        let mut results: Vec<(RegistryEntry, MatchScore)> = Vec::new();

        for (idx, (_, entry)) in entries.iter().enumerate() {
            let mut score = score_capabilities(&entry.capabilities, requirements);
            score.agent_index = idx;

            if score.all_required_met() {
                results.push((entry.clone(), score));
            }
        }

        // Sort by total score descending.
        results.sort_by(|a, b| {
            b.1.total_score
                .partial_cmp(&a.1.total_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
    }

    /// Find the single best agent for a set of requirements.
    pub fn best_agent(&self, requirements: &[CapabilityRequirement]) -> Option<(RegistryEntry, MatchScore)> {
        self.discover(requirements).into_iter().next()
    }

    /// Run a health sweep: detect stale agents and auto-deregister them.
    /// Returns the list of deregistered agent IDs.
    pub fn sweep_unhealthy(&self) -> Vec<AgentId> {
        let to_remove = self.health_monitor.sweep();
        let mut removed = Vec::new();

        for id in to_remove {
            if let Ok(entry) = self.unregister_agent(&id) {
                warn!(
                    agent_id = %id,
                    name = %entry.manifest.name,
                    "auto-deregistered unhealthy agent"
                );
                removed.push(id);
            }
        }

        removed
    }

    /// Get a reference to the health monitor.
    pub fn health_monitor(&self) -> &Arc<HealthMonitor> {
        &self.health_monitor
    }

    /// Get health status for a specific agent.
    pub fn agent_health(&self, id: &AgentId) -> HealthStatus {
        self.health_monitor.status(id)
    }

    /// Get a snapshot of all registered agent IDs.
    pub fn agent_ids(&self) -> Vec<AgentId> {
        self.agents.iter().map(|r| *r.key()).collect()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("agent_count", &self.agents.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use daf_core::{AgentCapability, AgentKind};

    fn make_manifest(name: &str, kind: AgentKind, caps: Vec<(&str, &str)>) -> AgentManifest {
        let mut m = AgentManifest::new(kind, name);
        for (cap_name, ver) in caps {
            m.capabilities
                .push(AgentCapability::new(cap_name, ver, ""));
        }
        m
    }

    #[test]
    fn register_and_get() {
        let reg = Registry::with_defaults();
        let manifest = make_manifest("worker-1", AgentKind::Worker, vec![("lint", "1.0.0")]);
        let id = manifest.id;

        reg.register_agent(manifest).unwrap();
        assert!(reg.contains(&id));
        assert_eq!(reg.len(), 1);

        let entry = reg.get_agent(&id).unwrap();
        assert_eq!(entry.manifest.name, "worker-1");
    }

    #[test]
    fn register_duplicate_fails() {
        let reg = Registry::with_defaults();
        let manifest = make_manifest("worker-1", AgentKind::Worker, vec![]);

        // Clone the manifest so we can use the same ID.
        let dupe = manifest.clone();
        reg.register_agent(manifest).unwrap();
        assert!(reg.register_agent(dupe).is_err());
    }

    #[test]
    fn unregister() {
        let reg = Registry::with_defaults();
        let manifest = make_manifest("worker-1", AgentKind::Worker, vec![]);
        let id = manifest.id;

        reg.register_agent(manifest).unwrap();
        let entry = reg.unregister_agent(&id).unwrap();
        assert_eq!(entry.manifest.name, "worker-1");
        assert!(!reg.contains(&id));
        assert!(reg.is_empty());
    }

    #[test]
    fn unregister_nonexistent_fails() {
        let reg = Registry::with_defaults();
        let id = AgentId::new();
        assert!(reg.unregister_agent(&id).is_err());
    }

    #[test]
    fn heartbeat_updates_timestamp() {
        let reg = Registry::with_defaults();
        let manifest = make_manifest("worker-1", AgentKind::Worker, vec![]);
        let id = manifest.id;
        reg.register_agent(manifest).unwrap();

        let before = reg.get_agent(&id).unwrap().last_heartbeat;
        std::thread::sleep(std::time::Duration::from_millis(10));
        reg.heartbeat(&id).unwrap();
        let after = reg.get_agent(&id).unwrap().last_heartbeat;

        assert!(after > before);
    }

    #[test]
    fn update_status() {
        let reg = Registry::with_defaults();
        let manifest = make_manifest("worker-1", AgentKind::Worker, vec![]);
        let id = manifest.id;
        reg.register_agent(manifest).unwrap();

        reg.update_status(&id, AgentStatus::Executing).unwrap();
        assert_eq!(
            reg.get_agent(&id).unwrap().status,
            AgentStatus::Executing
        );
    }

    #[test]
    fn list_agents_no_filter() {
        let reg = Registry::with_defaults();
        reg.register_agent(make_manifest("w1", AgentKind::Worker, vec![])).unwrap();
        reg.register_agent(make_manifest("s1", AgentKind::Specialist, vec![])).unwrap();

        let all = reg.list_agents(None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_agents_filter_by_kind() {
        let reg = Registry::with_defaults();
        reg.register_agent(make_manifest("w1", AgentKind::Worker, vec![])).unwrap();
        reg.register_agent(make_manifest("w2", AgentKind::Worker, vec![])).unwrap();
        reg.register_agent(make_manifest("s1", AgentKind::Specialist, vec![])).unwrap();

        let filter = AgentFilter::new().with_kind(AgentKind::Worker);
        let workers = reg.list_agents(Some(&filter));
        assert_eq!(workers.len(), 2);
    }

    #[test]
    fn list_agents_filter_by_capability() {
        let reg = Registry::with_defaults();
        reg.register_agent(make_manifest("w1", AgentKind::Worker, vec![("lint", "1.0.0")])).unwrap();
        reg.register_agent(make_manifest("w2", AgentKind::Worker, vec![("deploy", "1.0.0")])).unwrap();

        let filter = AgentFilter::new().with_capability("lint");
        let result = reg.list_agents(Some(&filter));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].manifest.name, "w1");
    }

    #[test]
    fn discover_by_requirements() {
        let reg = Registry::with_defaults();
        reg.register_agent(make_manifest(
            "full-stack",
            AgentKind::Specialist,
            vec![("lint", "1.0.0"), ("test", "1.0.0"), ("deploy", "2.0.0")],
        )).unwrap();
        reg.register_agent(make_manifest(
            "lint-only",
            AgentKind::Worker,
            vec![("lint", "1.0.0")],
        )).unwrap();

        let reqs = vec![
            CapabilityRequirement::required(
                "lint",
                crate::version::VersionConstraint::parse("^1.0.0").unwrap(),
            ),
            CapabilityRequirement::required(
                "deploy",
                crate::version::VersionConstraint::parse(">=1.0.0").unwrap(),
            ),
        ];

        let results = reg.discover(&reqs);
        // Only "full-stack" meets both required capabilities.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.manifest.name, "full-stack");
    }

    #[test]
    fn discover_ranks_by_optional() {
        let reg = Registry::with_defaults();
        reg.register_agent(make_manifest(
            "basic",
            AgentKind::Worker,
            vec![("lint", "1.0.0")],
        )).unwrap();
        reg.register_agent(make_manifest(
            "premium",
            AgentKind::Worker,
            vec![("lint", "1.0.0"), ("format", "1.0.0")],
        )).unwrap();

        let reqs = vec![
            CapabilityRequirement::required(
                "lint",
                crate::version::VersionConstraint::parse("^1.0.0").unwrap(),
            ),
            CapabilityRequirement::optional(
                "format",
                crate::version::VersionConstraint::parse("*").unwrap(),
            ),
        ];

        let results = reg.discover(&reqs);
        assert_eq!(results.len(), 2);
        // "premium" should rank higher because it has the optional capability.
        assert_eq!(results[0].0.manifest.name, "premium");
    }

    #[test]
    fn best_agent() {
        let reg = Registry::with_defaults();
        reg.register_agent(make_manifest(
            "agent-a",
            AgentKind::Worker,
            vec![("lint", "1.0.0"), ("test", "1.0.0")],
        )).unwrap();

        let reqs = vec![CapabilityRequirement::required(
            "lint",
            crate::version::VersionConstraint::parse("^1.0.0").unwrap(),
        )];

        let (entry, score) = reg.best_agent(&reqs).unwrap();
        assert_eq!(entry.manifest.name, "agent-a");
        assert!(score.all_required_met());
    }

    #[test]
    fn agent_ids_snapshot() {
        let reg = Registry::with_defaults();
        let m1 = make_manifest("a", AgentKind::Worker, vec![]);
        let m2 = make_manifest("b", AgentKind::Worker, vec![]);
        let id1 = m1.id;
        let id2 = m2.id;

        reg.register_agent(m1).unwrap();
        reg.register_agent(m2).unwrap();

        let ids = reg.agent_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }
}
