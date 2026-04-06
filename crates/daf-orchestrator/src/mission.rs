//! Mission definition — the declarative unit of orchestration.
//!
//! A mission is analogous to an Ansible playbook merged with a Terraform plan:
//! it declares *what* needs to happen, in *what order*, with *what constraints*,
//! and the orchestrator figures out *how* to execute it.
//!
//! Missions are composed of [`Phase`]s, each containing a set of [`TaskSpec`]s.
//! Phases execute sequentially (respecting dependency order), while tasks
//! within a phase execute concurrently up to the phase's concurrency limit.
//!
//! # Serialization
//!
//! Missions can be defined programmatically or loaded from YAML/JSON files,
//! making them suitable for both runtime construction and version-controlled
//! infrastructure-as-code workflows.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use daf_core::error::{DafError, DafResult};
use daf_core::AgentId;
use daf_graph::node::TaskSpec;

// ---------------------------------------------------------------------------
// MissionId
// ---------------------------------------------------------------------------

/// Time-ordered mission identifier backed by UUID v7.
///
/// Every mission gets a globally unique, chronologically sortable ID so that
/// mission logs, metrics, and audit trails can be correlated across the
/// distributed agent swarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MissionId(pub Uuid);

impl MissionId {
    /// Generate a new time-ordered mission identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for MissionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MissionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mission:{}", &self.0.to_string()[..8])
    }
}

// ---------------------------------------------------------------------------
// RetryPolicy
// ---------------------------------------------------------------------------

/// Controls how failures are retried within a mission or phase.
///
/// The orchestrator applies exponential backoff between retries, capped at
/// `max_backoff`. The total retry budget is bounded by both `max_retries`
/// and an implicit deadline from the parent mission's timeout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retries, fail immediately).
    pub max_retries: u32,
    /// Initial delay before the first retry.
    pub initial_backoff: Duration,
    /// Maximum delay between retries (caps exponential growth).
    pub max_backoff: Duration,
    /// Multiplier applied to the backoff after each retry (typically 2.0).
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// No retries — fail immediately on first error.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    /// Compute the backoff duration for the given attempt number (0-indexed).
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return self.initial_backoff;
        }
        let multiplier = self.backoff_multiplier.powi(attempt as i32);
        let backoff_ms = self.initial_backoff.as_millis() as f64 * multiplier;
        let capped = backoff_ms.min(self.max_backoff.as_millis() as f64);
        Duration::from_millis(capped as u64)
    }
}

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

/// A single phase within a mission — a logically grouped set of tasks that
/// execute together once the phase's dependencies are satisfied.
///
/// Phases are the primary unit of sequential ordering in a mission. Within a
/// phase, tasks run concurrently (up to `concurrency_limit`). Between phases,
/// execution is gated by the dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    /// Human-readable name (e.g., `"build"`, `"deploy"`, `"smoke-test"`).
    pub name: String,
    /// Description of what this phase accomplishes.
    pub description: String,
    /// Tasks to execute in this phase.
    pub tasks: Vec<TaskSpec>,
    /// Names of phases that must complete successfully before this one starts.
    /// Empty means this phase has no dependencies and can start immediately.
    pub dependencies: Vec<String>,
    /// Maximum number of tasks to run concurrently within this phase.
    /// `None` means unlimited (all tasks run in parallel).
    pub concurrency_limit: Option<usize>,
    /// Phase-level retry policy. Overrides the mission-level policy if set.
    pub retry_policy: Option<RetryPolicy>,
    /// Phase-level timeout. If the phase doesn't complete within this
    /// duration, it is considered failed.
    pub timeout: Option<Duration>,
    /// Arbitrary key-value variables scoped to this phase. Tasks within
    /// the phase can reference these via template expansion.
    pub vars: HashMap<String, serde_json::Value>,
    /// Required capabilities — the specialist router uses these to select
    /// which agents can handle tasks in this phase.
    pub required_capabilities: Vec<String>,
}

impl Phase {
    /// Create a new phase with the given name. All other fields default to
    /// empty/none and can be set via builder methods.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            tasks: Vec::new(),
            dependencies: Vec::new(),
            concurrency_limit: None,
            retry_policy: None,
            timeout: None,
            vars: HashMap::new(),
            required_capabilities: Vec::new(),
        }
    }

    /// Set a description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Add a task to this phase.
    pub fn task(mut self, spec: TaskSpec) -> Self {
        self.tasks.push(spec);
        self
    }

    /// Add multiple tasks.
    pub fn tasks(mut self, specs: Vec<TaskSpec>) -> Self {
        self.tasks.extend(specs);
        self
    }

    /// Declare a dependency on another phase by name.
    pub fn depends_on(mut self, phase_name: impl Into<String>) -> Self {
        self.dependencies.push(phase_name.into());
        self
    }

    /// Set the concurrency limit.
    pub fn concurrency(mut self, limit: usize) -> Self {
        self.concurrency_limit = Some(limit);
        self
    }

    /// Set the retry policy.
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Set the phase timeout.
    pub fn timeout(mut self, dur: Duration) -> Self {
        self.timeout = Some(dur);
        self
    }

    /// Insert a phase-scoped variable.
    pub fn var(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }

    /// Declare a required capability for agents handling this phase.
    pub fn requires(mut self, capability: impl Into<String>) -> Self {
        self.required_capabilities.push(capability.into());
        self
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Phase({name}, {n} tasks, deps=[{deps}])",
            name = self.name,
            n = self.tasks.len(),
            deps = self.dependencies.join(", "),
        )
    }
}

// ---------------------------------------------------------------------------
// MissionState
// ---------------------------------------------------------------------------

/// Lifecycle state of a mission.
///
/// Missions transition through these states as the orchestrator executes them.
/// The state machine prevents invalid transitions (e.g., you cannot go from
/// `Completed` back to `Executing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    /// Mission is being planned — phases are being resolved, agents selected.
    Planning,
    /// Mission is actively executing phases.
    Executing,
    /// Mission is paused — no new tasks are dispatched, but running tasks
    /// are allowed to finish.
    Paused,
    /// All phases completed successfully.
    Completed,
    /// One or more phases failed and the retry budget is exhausted.
    Failed,
    /// Mission was explicitly cancelled by the operator.
    Cancelled,
}

impl MissionState {
    /// Returns `true` if this is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Returns `true` if the mission is still in progress.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Executing | Self::Planning)
    }
}

impl fmt::Display for MissionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Mission
// ---------------------------------------------------------------------------

/// A complete mission definition — the top-level unit of orchestrated work.
///
/// Missions carry everything the orchestrator needs to plan, execute, and
/// report on a complex multi-phase operation. They are serializable to
/// YAML/JSON for storage and version control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    /// Unique identifier.
    pub id: MissionId,
    /// Human-readable name (e.g., `"deploy-v2"`, `"security-audit-q2"`).
    pub name: String,
    /// Detailed description of the mission's purpose and scope.
    pub description: String,
    /// Ordered list of phases. The orchestrator respects the dependency graph
    /// declared within phases, but uses this ordering as a tiebreaker.
    pub phases: Vec<Phase>,
    /// Global variables accessible to all phases and tasks via template
    /// expansion. Phase-level vars override globals with the same key.
    pub global_vars: HashMap<String, serde_json::Value>,
    /// Maximum wall-clock time for the entire mission. `None` = no limit.
    pub timeout: Option<Duration>,
    /// Default retry policy applied to phases that don't declare their own.
    pub retry_policy: RetryPolicy,
    /// When this mission was created.
    pub created_at: DateTime<Utc>,
    /// Arbitrary metadata (labels, annotations, owner, team).
    pub metadata: HashMap<String, String>,
}

impl Mission {
    /// Start building a mission with the given name.
    pub fn builder(name: impl Into<String>) -> MissionBuilder {
        MissionBuilder {
            name: name.into(),
            description: String::new(),
            phases: Vec::new(),
            global_vars: HashMap::new(),
            timeout: None,
            retry_policy: RetryPolicy::default(),
            metadata: HashMap::new(),
        }
    }

    /// Parse a mission from a JSON string.
    pub fn from_json(json: &str) -> DafResult<Self> {
        serde_json::from_str(json).map_err(|e| DafError::SerializationError(e.to_string()))
    }

    /// Serialize the mission to a JSON string.
    pub fn to_json(&self) -> DafResult<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| DafError::SerializationError(e.to_string()))
    }

    /// Resolve the phase execution order using topological sort based on
    /// declared dependencies. Returns the phases grouped into waves — each
    /// wave contains phases whose dependencies are all satisfied by previous
    /// waves.
    ///
    /// Returns an error if the dependency graph contains a cycle.
    pub fn resolve_execution_order(&self) -> DafResult<Vec<Vec<&Phase>>> {
        let phase_names: HashMap<&str, usize> = self
            .phases
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_str(), i))
            .collect();

        // Validate all dependency references.
        for phase in &self.phases {
            for dep in &phase.dependencies {
                if !phase_names.contains_key(dep.as_str()) {
                    return Err(DafError::ConfigError(format!(
                        "phase '{}' depends on unknown phase '{}'",
                        phase.name, dep
                    )));
                }
            }
        }

        // Kahn's algorithm for topological sort, grouping into waves.
        let n = self.phases.len();
        let mut in_degree = vec![0usize; n];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (i, phase) in self.phases.iter().enumerate() {
            for dep_name in &phase.dependencies {
                let dep_idx = phase_names[dep_name.as_str()];
                dependents[dep_idx].push(i);
                in_degree[i] += 1;
            }
        }

        let mut waves: Vec<Vec<&Phase>> = Vec::new();
        let mut remaining = n;

        // Seed: all phases with no dependencies.
        let mut current_wave: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();

        while !current_wave.is_empty() {
            let wave_phases: Vec<&Phase> = current_wave.iter().map(|&i| &self.phases[i]).collect();
            remaining -= current_wave.len();

            let mut next_wave = Vec::new();
            for &idx in &current_wave {
                for &dep_idx in &dependents[idx] {
                    in_degree[dep_idx] -= 1;
                    if in_degree[dep_idx] == 0 {
                        next_wave.push(dep_idx);
                    }
                }
            }

            waves.push(wave_phases);
            current_wave = next_wave;
        }

        if remaining > 0 {
            return Err(DafError::ConfigError(
                "mission contains a dependency cycle between phases".into(),
            ));
        }

        Ok(waves)
    }

    /// Total number of tasks across all phases.
    pub fn total_tasks(&self) -> usize {
        self.phases.iter().map(|p| p.tasks.len()).sum()
    }

    /// Look up a phase by name.
    pub fn phase_by_name(&self, name: &str) -> Option<&Phase> {
        self.phases.iter().find(|p| p.name == name)
    }

    /// Get the effective retry policy for a phase — the phase's own policy
    /// if it has one, otherwise the mission-level default.
    pub fn effective_retry_policy<'a>(&'a self, phase: &'a Phase) -> &'a RetryPolicy {
        phase.retry_policy.as_ref().unwrap_or(&self.retry_policy)
    }
}

impl fmt::Display for Mission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Mission({id} \"{name}\", {n} phases, {t} tasks)",
            id = self.id,
            name = self.name,
            n = self.phases.len(),
            t = self.total_tasks(),
        )
    }
}

// ---------------------------------------------------------------------------
// MissionBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for [`Mission`].
pub struct MissionBuilder {
    name: String,
    description: String,
    phases: Vec<Phase>,
    global_vars: HashMap<String, serde_json::Value>,
    timeout: Option<Duration>,
    retry_policy: RetryPolicy,
    metadata: HashMap<String, String>,
}

impl MissionBuilder {
    /// Set the mission description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Add a phase to the mission.
    pub fn phase(mut self, phase: Phase) -> Self {
        self.phases.push(phase);
        self
    }

    /// Add multiple phases.
    pub fn phases(mut self, phases: Vec<Phase>) -> Self {
        self.phases.extend(phases);
        self
    }

    /// Set a global variable.
    pub fn var(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.global_vars.insert(key.into(), value);
        self
    }

    /// Set the mission timeout.
    pub fn timeout(mut self, dur: Duration) -> Self {
        self.timeout = Some(dur);
        self
    }

    /// Set the default retry policy.
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Insert metadata.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Consume the builder and produce the [`Mission`].
    pub fn build(self) -> Mission {
        Mission {
            id: MissionId::new(),
            name: self.name,
            description: self.description,
            phases: self.phases,
            global_vars: self.global_vars,
            timeout: self.timeout,
            retry_policy: self.retry_policy,
            created_at: Utc::now(),
            metadata: self.metadata,
        }
    }
}

// ---------------------------------------------------------------------------
// PhaseResult
// ---------------------------------------------------------------------------

/// The outcome of executing a single phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    /// Name of the phase.
    pub phase_name: String,
    /// Whether the phase succeeded.
    pub succeeded: bool,
    /// Number of tasks that completed successfully.
    pub tasks_succeeded: usize,
    /// Number of tasks that failed.
    pub tasks_failed: usize,
    /// Number of tasks that were skipped (due to earlier failures).
    pub tasks_skipped: usize,
    /// Wall-clock duration of the phase.
    pub duration: Duration,
    /// Error message if the phase failed.
    pub error: Option<String>,
    /// Agents that participated in this phase.
    pub agents_used: Vec<AgentId>,
    /// Number of retry attempts consumed.
    pub retries_used: u32,
}

impl fmt::Display for PhaseResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.succeeded { "OK" } else { "FAILED" };
        write!(
            f,
            "[{status}] {name} ({succ}/{total} tasks, {dur:?})",
            name = self.phase_name,
            succ = self.tasks_succeeded,
            total = self.tasks_succeeded + self.tasks_failed + self.tasks_skipped,
            dur = self.duration,
        )
    }
}

// ---------------------------------------------------------------------------
// MissionResult
// ---------------------------------------------------------------------------

/// The final outcome of a mission execution.
///
/// Contains per-phase results, timing information, and resource utilization
/// data suitable for dashboards and retrospectives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionResult {
    /// Identifier of the mission.
    pub mission_id: MissionId,
    /// Terminal state of the mission.
    pub state: MissionState,
    /// Results for each phase, in execution order.
    pub phase_results: Vec<PhaseResult>,
    /// Total wall-clock duration from start to finish.
    pub total_duration: Duration,
    /// Map from agent ID to the fraction of time that agent was actively
    /// working (0.0 = idle the whole time, 1.0 = busy the whole time).
    pub agent_utilization: HashMap<String, f64>,
    /// When the mission started executing.
    pub started_at: DateTime<Utc>,
    /// When the mission finished.
    pub finished_at: DateTime<Utc>,
}

impl MissionResult {
    /// Returns `true` if the mission completed successfully.
    pub fn is_success(&self) -> bool {
        self.state == MissionState::Completed
    }

    /// Total number of tasks that succeeded across all phases.
    pub fn total_tasks_succeeded(&self) -> usize {
        self.phase_results.iter().map(|r| r.tasks_succeeded).sum()
    }

    /// Total number of tasks that failed across all phases.
    pub fn total_tasks_failed(&self) -> usize {
        self.phase_results.iter().map(|r| r.tasks_failed).sum()
    }

    /// Overall success rate as a fraction in [0.0, 1.0].
    pub fn success_rate(&self) -> f64 {
        let total = self.total_tasks_succeeded() + self.total_tasks_failed();
        if total == 0 {
            return 1.0;
        }
        self.total_tasks_succeeded() as f64 / total as f64
    }
}

impl fmt::Display for MissionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MissionResult({id} {state}, {succ}/{total} tasks OK, {dur:?})",
            id = self.mission_id,
            state = self.state,
            succ = self.total_tasks_succeeded(),
            total = self.total_tasks_succeeded() + self.total_tasks_failed(),
            dur = self.total_duration,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(name: &str) -> TaskSpec {
        TaskSpec {
            task_type: name.into(),
            params: serde_json::json!({}),
            timeout: None,
            max_retries: 0,
        }
    }

    #[test]
    fn mission_builder_basic() {
        let m = Mission::builder("test-mission")
            .description("A test")
            .phase(Phase::new("build").task(sample_task("cargo.build")))
            .phase(
                Phase::new("test")
                    .depends_on("build")
                    .task(sample_task("cargo.test")),
            )
            .var("env", serde_json::json!("staging"))
            .build();

        assert_eq!(m.name, "test-mission");
        assert_eq!(m.phases.len(), 2);
        assert_eq!(m.total_tasks(), 2);
        assert_eq!(m.phases[1].dependencies, vec!["build"]);
    }

    #[test]
    fn mission_id_is_time_ordered() {
        let a = MissionId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = MissionId::new();
        assert!(a.0 < b.0);
    }

    #[test]
    fn resolve_linear_order() {
        let m = Mission::builder("linear")
            .phase(Phase::new("a"))
            .phase(Phase::new("b").depends_on("a"))
            .phase(Phase::new("c").depends_on("b"))
            .build();

        let waves = m.resolve_execution_order().unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0][0].name, "a");
        assert_eq!(waves[1][0].name, "b");
        assert_eq!(waves[2][0].name, "c");
    }

    #[test]
    fn resolve_parallel_phases() {
        let m = Mission::builder("parallel")
            .phase(Phase::new("a"))
            .phase(Phase::new("b"))
            .phase(Phase::new("c").depends_on("a").depends_on("b"))
            .build();

        let waves = m.resolve_execution_order().unwrap();
        assert_eq!(waves.len(), 2);
        // a and b should be in the first wave (order within wave is unspecified).
        assert_eq!(waves[0].len(), 2);
        assert_eq!(waves[1].len(), 1);
        assert_eq!(waves[1][0].name, "c");
    }

    #[test]
    fn resolve_detects_cycle() {
        let m = Mission::builder("cycle")
            .phase(Phase::new("a").depends_on("b"))
            .phase(Phase::new("b").depends_on("a"))
            .build();

        let result = m.resolve_execution_order();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn resolve_unknown_dependency() {
        let m = Mission::builder("bad-dep")
            .phase(Phase::new("a").depends_on("nonexistent"))
            .build();

        let result = m.resolve_execution_order();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown phase"));
    }

    #[test]
    fn retry_policy_backoff() {
        let policy = RetryPolicy {
            max_retries: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        };

        assert_eq!(policy.backoff_for_attempt(0), Duration::from_secs(1));
        assert_eq!(policy.backoff_for_attempt(1), Duration::from_secs(2));
        assert_eq!(policy.backoff_for_attempt(2), Duration::from_secs(4));
        assert_eq!(policy.backoff_for_attempt(3), Duration::from_secs(8));
        assert_eq!(policy.backoff_for_attempt(4), Duration::from_secs(16));
        // Attempt 5 would be 32s, but capped at 30s.
        assert_eq!(policy.backoff_for_attempt(5), Duration::from_secs(30));
    }

    #[test]
    fn mission_state_terminal() {
        assert!(MissionState::Completed.is_terminal());
        assert!(MissionState::Failed.is_terminal());
        assert!(MissionState::Cancelled.is_terminal());
        assert!(!MissionState::Executing.is_terminal());
        assert!(!MissionState::Planning.is_terminal());
        assert!(!MissionState::Paused.is_terminal());
    }

    #[test]
    fn mission_json_roundtrip() {
        let m = Mission::builder("roundtrip")
            .phase(Phase::new("alpha").task(sample_task("test.run")))
            .build();

        let json = m.to_json().unwrap();
        let m2 = Mission::from_json(&json).unwrap();
        assert_eq!(m.id, m2.id);
        assert_eq!(m.name, m2.name);
        assert_eq!(m.phases.len(), m2.phases.len());
    }

    #[test]
    fn phase_by_name_lookup() {
        let m = Mission::builder("lookup")
            .phase(Phase::new("alpha"))
            .phase(Phase::new("beta"))
            .build();

        assert!(m.phase_by_name("alpha").is_some());
        assert!(m.phase_by_name("gamma").is_none());
    }

    #[test]
    fn mission_result_success_rate() {
        let result = MissionResult {
            mission_id: MissionId::new(),
            state: MissionState::Completed,
            phase_results: vec![
                PhaseResult {
                    phase_name: "build".into(),
                    succeeded: true,
                    tasks_succeeded: 8,
                    tasks_failed: 2,
                    tasks_skipped: 0,
                    duration: Duration::from_secs(10),
                    error: None,
                    agents_used: vec![],
                    retries_used: 0,
                },
            ],
            total_duration: Duration::from_secs(10),
            agent_utilization: HashMap::new(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
        };

        assert!((result.success_rate() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn phase_display() {
        let p = Phase::new("deploy")
            .depends_on("build")
            .depends_on("test")
            .task(sample_task("docker.push"));
        let display = p.to_string();
        assert!(display.contains("deploy"));
        assert!(display.contains("1 tasks"));
        assert!(display.contains("build"));
    }
}
