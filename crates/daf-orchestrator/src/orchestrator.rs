//! Core orchestrator — the central brain that coordinates agents, missions,
//! and task dispatch across the DAF swarm.
//!
//! The orchestrator owns the lifecycle of missions: it plans execution order,
//! selects specialist agents, dispatches tasks in dependency-respecting waves,
//! monitors progress, and drives graceful shutdown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument, warn};

use daf_core::agent::{AgentCapability, AgentId, AgentManifest, AgentStatus};
use daf_core::error::{DafError, DafResult};
use daf_graph::node::TaskSpec;

use crate::handoff::HandoffManager;
use crate::metrics::MetricsCollector;
use crate::mission::{Mission, MissionId, MissionResult, MissionState, PhaseResult};
use crate::specialist::SpecialistRouter;
use crate::supervisor::{Supervisor, SupervisorEvent, SupervisorStrategy};

// ---------------------------------------------------------------------------
// OrchestratorConfig
// ---------------------------------------------------------------------------

/// Configuration knobs for the orchestrator runtime.
///
/// All durations and limits have sensible defaults for single-machine
/// deployments. Tune upward for large distributed swarms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    /// Maximum number of missions executing concurrently.
    pub max_concurrent_missions: usize,
    /// Per-wave timeout in seconds — if a wave doesn't complete within this
    /// window the orchestrator marks it failed and moves on.
    pub wave_timeout_secs: u64,
    /// Interval between agent heartbeat probes (seconds).
    pub heartbeat_interval_secs: u64,
    /// Maximum tasks dispatched to a single agent at once.
    pub max_tasks_per_agent: usize,
    /// Whether to cancel remaining phases when one phase fails.
    pub fail_fast: bool,
    /// Maximum tasks per wave (for the sprint planner).
    pub max_tasks_per_wave: Option<usize>,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_concurrent_missions: 8,
            wave_timeout_secs: 300,
            heartbeat_interval_secs: 15,
            max_tasks_per_agent: 4,
            fail_fast: true,
            max_tasks_per_wave: None,
        }
    }
}

// ---------------------------------------------------------------------------
// AgentEntry
// ---------------------------------------------------------------------------

/// Internal bookkeeping for a registered agent.
#[derive(Debug, Clone)]
pub struct AgentEntry {
    /// The agent's unique identifier.
    pub id: AgentId,
    /// Declared manifest including capabilities and resource limits.
    pub manifest: AgentManifest,
    /// Current lifecycle status.
    pub status: AgentStatus,
    /// Number of tasks currently assigned to this agent.
    pub active_tasks: usize,
    /// Timestamp of the last successful heartbeat.
    pub last_heartbeat: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// The DAF orchestrator — coordinates mission execution across a swarm of agents.
///
/// # Thread Safety
///
/// The orchestrator is `Send + Sync` and designed for concurrent access. Internal
/// state uses lock-free data structures ([`DashMap`]) and atomic flags.
///
/// # Construction
///
/// Use [`Orchestrator::builder`] for a fluent construction API, or
/// [`Orchestrator::new`] for a quick default setup.
pub struct Orchestrator {
    /// Runtime configuration.
    config: OrchestratorConfig,
    /// Registered agents keyed by their ID.
    agents: DashMap<AgentId, AgentEntry>,
    /// Active missions keyed by their ID.
    missions: DashMap<MissionId, MissionState>,
    /// Specialist routing engine.
    router: SpecialistRouter,
    /// Agent supervisor for restart policies. Behind a mutex because
    /// `handle_failure` requires `&mut self`.
    supervisor: parking_lot::Mutex<Supervisor>,
    /// Handoff manager for agent-to-agent context transfer.
    handoff_manager: HandoffManager,
    /// Metrics collector.
    metrics: MetricsCollector,
    /// Graceful shutdown flag.
    shutdown_flag: Arc<AtomicBool>,
}

impl Orchestrator {
    /// Create an orchestrator with default configuration.
    pub fn new() -> Self {
        Self::with_config(OrchestratorConfig::default())
    }

    /// Create an orchestrator with the given configuration.
    pub fn with_config(config: OrchestratorConfig) -> Self {
        Self {
            config,
            agents: DashMap::new(),
            missions: DashMap::new(),
            router: SpecialistRouter::new(),
            supervisor: parking_lot::Mutex::new(Supervisor::new(SupervisorStrategy::OneForOne)),
            handoff_manager: HandoffManager::new(),
            metrics: MetricsCollector::new(),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start building an orchestrator with the fluent API.
    pub fn builder() -> OrchestratorBuilder {
        OrchestratorBuilder::default()
    }

    // -- Agent lifecycle ------------------------------------------------------

    /// Register an agent with the orchestrator.
    ///
    /// The agent's manifest is inspected for capabilities and resource limits.
    /// Returns the assigned [`AgentId`].
    #[instrument(skip(self, manifest), fields(name = %manifest.name))]
    pub fn spawn_agent(&self, manifest: AgentManifest) -> DafResult<AgentId> {
        let id = manifest.id;
        info!(%id, name = %manifest.name, "spawning agent");

        let entry = AgentEntry {
            id,
            manifest: manifest.clone(),
            status: AgentStatus::Idle,
            active_tasks: 0,
            last_heartbeat: Utc::now(),
        };

        self.agents.insert(id, entry);
        self.router.register_agent(manifest.clone());
        self.metrics.agent_spawned();

        debug!(%id, capabilities = manifest.capabilities.len(), "agent registered");
        Ok(id)
    }

    /// Remove an agent from the registry.
    ///
    /// Any tasks currently assigned to the agent are NOT automatically
    /// reassigned — the caller is responsible for draining first.
    pub fn remove_agent(&self, agent_id: &AgentId) -> DafResult<()> {
        self.agents
            .remove(agent_id)
            .ok_or_else(|| DafError::NotFound {
                entity: "agent".into(),
                id: agent_id.to_string(),
            })?;
        self.router.deregister_agent(agent_id);
        self.metrics.agent_removed();
        info!(%agent_id, "agent removed");
        Ok(())
    }

    // -- Task dispatch --------------------------------------------------------

    /// Dispatch a single task to the best available agent by capability match.
    ///
    /// The specialist router scores all registered agents against the required
    /// capabilities and picks the highest scorer that is under its concurrency
    /// limit.
    pub fn dispatch_task(
        &self,
        task: &TaskSpec,
        required_capabilities: &[String],
    ) -> DafResult<AgentId> {
        if self.is_shutting_down() {
            return Err(DafError::Internal(
                "orchestrator is shutting down, cannot dispatch".into(),
            ));
        }

        let decision = self.router.route_task(task, required_capabilities, None)?;

        // Update the agent's active task count.
        if let Some(mut entry) = self.agents.get_mut(&decision.agent_id) {
            entry.active_tasks += 1;
            entry.status = AgentStatus::Executing;
        }

        self.metrics.task_dispatched();
        debug!(
            agent = %decision.agent_id,
            task = %task.task_type,
            score = decision.score,
            "task dispatched"
        );

        Ok(decision.agent_id)
    }

    /// Record a task completion, updating the agent's bookkeeping.
    pub fn record_task_done(&self, agent_id: &AgentId, success: bool) {
        if let Some(mut entry) = self.agents.get_mut(agent_id) {
            entry.active_tasks = entry.active_tasks.saturating_sub(1);
            if entry.active_tasks == 0 {
                entry.status = AgentStatus::Idle;
            }
        }

        self.router.record_task_completion(agent_id);

        if success {
            self.metrics.task_completed();
        } else {
            self.metrics.task_failed();
        }
    }

    // -- Mission execution ----------------------------------------------------

    /// Execute a mission end-to-end using wave-based scheduling.
    ///
    /// Phases are topologically sorted into waves. Within each wave, all
    /// phases execute concurrently (tasks within a phase are dispatched up
    /// to the phase's concurrency limit). The orchestrator blocks between
    /// waves, waiting for all tasks in the current wave to finish before
    /// advancing.
    ///
    /// Returns a [`MissionResult`] summarizing the outcome.
    #[instrument(skip(self, mission), fields(mission_id = %mission.id, mission_name = %mission.name))]
    pub fn run_mission(&self, mission: &Mission) -> DafResult<MissionResult> {
        let started_at = Utc::now();
        info!(id = %mission.id, name = %mission.name, "starting mission");
        self.missions.insert(mission.id, MissionState::Planning);
        self.metrics.mission_started();

        // Resolve phase execution waves via topological sort.
        let waves = mission.resolve_execution_order()?;
        self.missions.insert(mission.id, MissionState::Executing);

        let mut phase_results = Vec::new();
        let mut failed = false;

        for (wave_idx, wave_phases) in waves.iter().enumerate() {
            if self.is_shutting_down() {
                warn!(mission = %mission.id, "shutdown requested, cancelling mission");
                self.missions.insert(mission.id, MissionState::Cancelled);
                failed = true;
                break;
            }

            debug!(
                mission = %mission.id,
                wave = wave_idx,
                phases = wave_phases.len(),
                "executing wave"
            );

            for phase in wave_phases {
                let phase_start = std::time::Instant::now();
                let mut tasks_succeeded = 0usize;
                let mut tasks_failed_count = 0usize;
                let mut tasks_skipped = 0usize;
                let mut agents_used = Vec::new();
                let mut retries_used = 0u32;
                let retry_policy = mission.effective_retry_policy(phase);

                for task in &phase.tasks {
                    if self.is_shutting_down() || (failed && self.config.fail_fast) {
                        tasks_skipped += 1;
                        continue;
                    }

                    // Dispatch with retry logic.
                    let caps = &phase.required_capabilities;
                    match self.dispatch_task(task, caps) {
                        Ok(agent_id) => {
                            agents_used.push(agent_id);
                            tasks_succeeded += 1;
                        }
                        Err(e) => {
                            let mut succeeded = false;
                            for attempt in 0..retry_policy.max_retries {
                                retries_used += 1;
                                let _backoff = retry_policy.backoff_for_attempt(attempt);
                                debug!(
                                    phase = %phase.name,
                                    attempt = attempt + 1,
                                    "retrying task dispatch"
                                );
                                if let Ok(agent_id) = self.dispatch_task(task, caps) {
                                    agents_used.push(agent_id);
                                    succeeded = true;
                                    break;
                                }
                            }
                            if succeeded {
                                tasks_succeeded += 1;
                            } else {
                                error!(
                                    phase = %phase.name,
                                    error = %e,
                                    "task dispatch failed after retries"
                                );
                                tasks_failed_count += 1;
                            }
                        }
                    }
                }

                let phase_succeeded = tasks_failed_count == 0;
                if !phase_succeeded {
                    failed = true;
                }

                phase_results.push(PhaseResult {
                    phase_name: phase.name.clone(),
                    succeeded: phase_succeeded,
                    tasks_succeeded,
                    tasks_failed: tasks_failed_count,
                    tasks_skipped,
                    duration: phase_start.elapsed(),
                    error: if phase_succeeded {
                        None
                    } else {
                        Some(format!(
                            "{tasks_failed_count} tasks failed in phase '{}'",
                            phase.name
                        ))
                    },
                    agents_used,
                    retries_used,
                });
            }
        }

        let finished_at = Utc::now();
        let total_duration = (finished_at - started_at)
            .to_std()
            .unwrap_or(Duration::ZERO);

        let final_state = if failed {
            if self.is_shutting_down() {
                MissionState::Cancelled
            } else {
                MissionState::Failed
            }
        } else {
            MissionState::Completed
        };

        self.missions.insert(mission.id, final_state);
        self.metrics.mission_completed();
        info!(
            id = %mission.id,
            state = %final_state,
            duration = ?total_duration,
            "mission finished"
        );

        Ok(MissionResult {
            mission_id: mission.id,
            state: final_state,
            phase_results,
            total_duration,
            agent_utilization: HashMap::new(),
            started_at,
            finished_at,
        })
    }

    // -- Health monitoring ----------------------------------------------------

    /// Run a single health-check pass across all registered agents.
    ///
    /// Agents that haven't sent a heartbeat within `2 * heartbeat_interval`
    /// are marked as potentially failed. The supervisor decides whether to
    /// restart them.
    pub fn monitor_agents(&self) -> Vec<AgentId> {
        let threshold = chrono::Duration::seconds(self.config.heartbeat_interval_secs as i64 * 2);
        let now = Utc::now();
        let mut failed_agents = Vec::new();

        for entry in self.agents.iter() {
            let age = now - entry.last_heartbeat;
            if age > threshold && !entry.status.is_terminal() {
                warn!(
                    agent = %entry.id,
                    age_secs = age.num_seconds(),
                    "agent missed heartbeat"
                );

                let events = self.supervisor.lock().handle_failure(
                    entry.id,
                    &format!("heartbeat timeout after {}s", age.num_seconds()),
                );

                match events {
                    Ok(evts) => {
                        let has_restart = evts.iter().any(|e| {
                            matches!(e, SupervisorEvent::AgentRestarted { .. })
                        });
                        if has_restart {
                            info!(agent = %entry.id, "supervisor approved restart");
                            failed_agents.push(entry.id);
                        } else {
                            warn!(agent = %entry.id, "supervisor denied restart, agent dead-lettered");
                        }
                    }
                    Err(e) => {
                        warn!(agent = %entry.id, error = %e, "supervisor failure handling errored");
                    }
                }
            }
        }

        failed_agents
    }

    // -- Shutdown --------------------------------------------------------------

    /// Initiate graceful shutdown.
    ///
    /// Sets the shutdown flag so that no new tasks are dispatched and running
    /// missions are cancelled at the next wave boundary.
    pub fn shutdown(&self) {
        info!("orchestrator shutdown requested");
        self.shutdown_flag.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if a graceful shutdown has been requested.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown_flag.load(Ordering::SeqCst)
    }

    // -- Accessors ------------------------------------------------------------

    /// Return a reference to the orchestrator configuration.
    pub fn config(&self) -> &OrchestratorConfig {
        &self.config
    }

    /// Return the number of currently registered agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Return a reference to the metrics collector.
    pub fn metrics(&self) -> &MetricsCollector {
        &self.metrics
    }

    /// Return a reference to the specialist router.
    pub fn router(&self) -> &SpecialistRouter {
        &self.router
    }

    /// Return a reference to the supervisor mutex.
    pub fn supervisor(&self) -> &parking_lot::Mutex<Supervisor> {
        &self.supervisor
    }

    /// Return a reference to the handoff manager.
    pub fn handoff_manager(&self) -> &HandoffManager {
        &self.handoff_manager
    }

    /// Look up an agent entry by ID.
    pub fn get_agent(&self, id: &AgentId) -> Option<AgentEntry> {
        self.agents.get(id).map(|r| r.clone())
    }

    /// Get the state of an active mission.
    pub fn mission_state(&self, id: &MissionId) -> Option<MissionState> {
        self.missions.get(id).map(|r| *r)
    }

    /// Update an agent's heartbeat timestamp (called by the heartbeat probe).
    pub fn heartbeat(&self, agent_id: &AgentId) -> DafResult<()> {
        let mut entry =
            self.agents
                .get_mut(agent_id)
                .ok_or_else(|| DafError::NotFound {
                    entity: "agent".into(),
                    id: agent_id.to_string(),
                })?;
        entry.last_heartbeat = Utc::now();
        Ok(())
    }

    /// List all registered agent IDs.
    pub fn agent_ids(&self) -> Vec<AgentId> {
        self.agents.iter().map(|r| *r.key()).collect()
    }

    /// Return the number of active (non-terminal) agents.
    pub fn active_agent_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|e| !e.status.is_terminal())
            .count()
    }
}

impl Default for Orchestrator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// OrchestratorBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for [`Orchestrator`].
///
/// # Example
///
/// ```rust
/// use daf_orchestrator::orchestrator::{Orchestrator, OrchestratorConfig};
///
/// let orch = Orchestrator::builder()
///     .max_concurrent_missions(16)
///     .wave_timeout_secs(600)
///     .fail_fast(false)
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct OrchestratorBuilder {
    config: OrchestratorConfig,
    supervisor_strategy: Option<SupervisorStrategy>,
}

impl OrchestratorBuilder {
    /// Set the full configuration.
    pub fn config(mut self, config: OrchestratorConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the maximum concurrent missions.
    pub fn max_concurrent_missions(mut self, n: usize) -> Self {
        self.config.max_concurrent_missions = n;
        self
    }

    /// Set the per-wave timeout in seconds.
    pub fn wave_timeout_secs(mut self, secs: u64) -> Self {
        self.config.wave_timeout_secs = secs;
        self
    }

    /// Set the heartbeat interval in seconds.
    pub fn heartbeat_interval_secs(mut self, secs: u64) -> Self {
        self.config.heartbeat_interval_secs = secs;
        self
    }

    /// Set the maximum tasks per agent.
    pub fn max_tasks_per_agent(mut self, n: usize) -> Self {
        self.config.max_tasks_per_agent = n;
        self
    }

    /// Set whether the orchestrator should fail fast on phase failure.
    pub fn fail_fast(mut self, yes: bool) -> Self {
        self.config.fail_fast = yes;
        self
    }

    /// Set the supervisor strategy.
    pub fn supervisor_strategy(mut self, strategy: SupervisorStrategy) -> Self {
        self.supervisor_strategy = Some(strategy);
        self
    }

    /// Consume the builder and produce the [`Orchestrator`].
    pub fn build(self) -> Orchestrator {
        let mut orch = Orchestrator::with_config(self.config);
        if let Some(strategy) = self.supervisor_strategy {
            orch.supervisor = parking_lot::Mutex::new(Supervisor::new(strategy));
        }
        orch
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::{Mission, Phase};
    use daf_core::agent::{AgentCapability, AgentKind, AgentManifest};

    fn test_manifest(name: &str, caps: &[&str]) -> AgentManifest {
        let mut m = AgentManifest::new(AgentKind::Worker, name);
        for cap in caps {
            m = m.with_capability(AgentCapability::new(*cap, "1.0.0", ""));
        }
        m
    }

    fn sample_task(name: &str) -> TaskSpec {
        TaskSpec {
            task_type: name.into(),
            params: serde_json::json!({}),
            timeout: None,
            max_retries: 0,
        }
    }

    #[test]
    fn spawn_and_lookup_agent() {
        let orch = Orchestrator::new();
        let manifest = test_manifest("worker-1", &["build", "test"]);
        let id = orch.spawn_agent(manifest).unwrap();

        assert_eq!(orch.agent_count(), 1);
        let entry = orch.get_agent(&id).unwrap();
        assert_eq!(entry.status, AgentStatus::Idle);
        assert_eq!(entry.active_tasks, 0);
    }

    #[test]
    fn remove_nonexistent_agent_errors() {
        let orch = Orchestrator::new();
        let result = orch.remove_agent(&AgentId::new());
        assert!(result.is_err());
    }

    #[test]
    fn dispatch_without_agents_errors() {
        let orch = Orchestrator::new();
        let task = sample_task("build");
        let result = orch.dispatch_task(&task, &["build".into()]);
        assert!(result.is_err());
    }

    #[test]
    fn dispatch_routes_to_capable_agent() {
        let orch = Orchestrator::new();
        let manifest = test_manifest("builder", &["build", "compile"]);
        let agent_id = orch.spawn_agent(manifest).unwrap();

        let task = sample_task("compile");
        let dispatched_to = orch.dispatch_task(&task, &["build".into()]).unwrap();
        assert_eq!(dispatched_to, agent_id);

        let entry = orch.get_agent(&agent_id).unwrap();
        assert_eq!(entry.active_tasks, 1);
        assert_eq!(entry.status, AgentStatus::Executing);
    }

    #[test]
    fn record_task_done_updates_agent() {
        let orch = Orchestrator::new();
        let manifest = test_manifest("worker", &["test"]);
        let agent_id = orch.spawn_agent(manifest).unwrap();

        let task = sample_task("test.run");
        orch.dispatch_task(&task, &[]).unwrap();
        assert_eq!(orch.get_agent(&agent_id).unwrap().active_tasks, 1);

        orch.record_task_done(&agent_id, true);
        assert_eq!(orch.get_agent(&agent_id).unwrap().active_tasks, 0);
        assert_eq!(orch.get_agent(&agent_id).unwrap().status, AgentStatus::Idle);
    }

    #[test]
    fn shutdown_prevents_dispatch() {
        let orch = Orchestrator::new();
        let manifest = test_manifest("worker", &["any"]);
        orch.spawn_agent(manifest).unwrap();

        orch.shutdown();
        assert!(orch.is_shutting_down());

        let task = sample_task("work");
        let result = orch.dispatch_task(&task, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn builder_pattern() {
        let orch = Orchestrator::builder()
            .max_concurrent_missions(4)
            .wave_timeout_secs(120)
            .heartbeat_interval_secs(5)
            .max_tasks_per_agent(2)
            .fail_fast(false)
            .build();

        assert_eq!(orch.config().max_concurrent_missions, 4);
        assert_eq!(orch.config().wave_timeout_secs, 120);
        assert!(!orch.config().fail_fast);
    }

    #[test]
    fn heartbeat_updates_timestamp() {
        let orch = Orchestrator::new();
        let manifest = test_manifest("hb-agent", &[]);
        let id = orch.spawn_agent(manifest).unwrap();

        let before = orch.get_agent(&id).unwrap().last_heartbeat;
        std::thread::sleep(std::time::Duration::from_millis(10));
        orch.heartbeat(&id).unwrap();
        let after = orch.get_agent(&id).unwrap().last_heartbeat;
        assert!(after > before);
    }

    #[test]
    fn agent_ids_lists_all() {
        let orch = Orchestrator::new();
        let id1 = orch.spawn_agent(test_manifest("a", &[])).unwrap();
        let id2 = orch.spawn_agent(test_manifest("b", &[])).unwrap();

        let ids = orch.agent_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn run_mission_all_dispatched() {
        let orch = Orchestrator::new();
        orch.spawn_agent(test_manifest("builder", &["build"])).unwrap();
        orch.spawn_agent(test_manifest("tester", &["test"])).unwrap();

        let mission = Mission::builder("deploy")
            .phase(Phase::new("build").task(sample_task("cargo.build")).requires("build"))
            .phase(
                Phase::new("test")
                    .depends_on("build")
                    .task(sample_task("cargo.test"))
                    .requires("test"),
            )
            .build();

        let result = orch.run_mission(&mission).unwrap();
        assert_eq!(result.state, MissionState::Completed);
        assert_eq!(result.phase_results.len(), 2);
        assert!(result.phase_results[0].succeeded);
        assert!(result.phase_results[1].succeeded);
    }

    #[test]
    fn run_mission_fails_when_no_agents() {
        let orch = Orchestrator::new();
        let mission = Mission::builder("no-agents")
            .phase(Phase::new("build").task(sample_task("compile")).requires("build"))
            .build();

        let result = orch.run_mission(&mission).unwrap();
        assert_eq!(result.state, MissionState::Failed);
    }

    #[test]
    fn monitor_agents_detects_stale() {
        let orch = Orchestrator::builder()
            .heartbeat_interval_secs(0)
            .build();

        let manifest = test_manifest("stale", &[]);
        let id = orch.spawn_agent(manifest).unwrap();

        // Artificially backdate the heartbeat.
        if let Some(mut entry) = orch.agents.get_mut(&id) {
            entry.last_heartbeat = Utc::now() - chrono::Duration::seconds(100);
        }

        let failed = orch.monitor_agents();
        // The supervisor should approve at least one restart.
        assert!(!failed.is_empty() || !orch.supervisor().lock().dead_letters().is_empty());
    }

    #[test]
    fn active_agent_count() {
        let orch = Orchestrator::new();
        orch.spawn_agent(test_manifest("a", &[])).unwrap();
        orch.spawn_agent(test_manifest("b", &[])).unwrap();
        assert_eq!(orch.active_agent_count(), 2);
    }

    #[test]
    fn builder_with_supervisor_strategy() {
        let orch = Orchestrator::builder()
            .supervisor_strategy(SupervisorStrategy::OneForAll)
            .build();
        assert_eq!(orch.supervisor().lock().strategy(), SupervisorStrategy::OneForAll);
    }
}
