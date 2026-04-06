//! Task execution model.
//!
//! Tasks are the atomic unit of work in DAF. An orchestrator decomposes a
//! goal into a directed acyclic graph of tasks, each assigned to an agent
//! whose capabilities match the task requirements. Tasks transition through
//! a well-defined [`TaskState`] machine from [`Pending`](TaskState::Pending)
//! to a terminal state.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::{AgentId, AgentKind};

// ---------------------------------------------------------------------------
// TaskId
// ---------------------------------------------------------------------------

/// Time-ordered task identifier backed by UUID v7.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(Uuid);

impl TaskId {
    /// Generate a new time-ordered task identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaskId({})", &self.0.to_string()[..8])
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for TaskId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

// ---------------------------------------------------------------------------
// TaskState
// ---------------------------------------------------------------------------

/// State machine governing task lifecycle.
///
/// ```text
///  Pending ──▶ Queued ──▶ Running ──▶ Succeeded
///                            │
///                            ├──▶ Paused ──▶ Running
///                            │
///                            ├──▶ Failed
///                            │
///                            ├──▶ Cancelled
///                            │
///                            └──▶ Retrying ──▶ Queued
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Task has been created but not yet eligible for scheduling (dependencies
    /// may still be unresolved).
    Pending,
    /// Task is in the scheduler queue waiting for an available agent.
    Queued,
    /// An agent is actively executing this task.
    Running,
    /// Execution was temporarily suspended (e.g. agent preempted).
    Paused,
    /// Task completed successfully.
    Succeeded,
    /// Task failed after exhausting all retries.
    Failed,
    /// Task was explicitly cancelled by an operator or orchestrator.
    Cancelled,
    /// Task failed but a retry has been scheduled.
    Retrying,
}

impl TaskState {
    /// Returns `true` for terminal states that will never transition again.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// Returns `true` when the task is consuming an agent's attention.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running | Self::Retrying)
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Retrying => "retrying",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// TaskPriority
// ---------------------------------------------------------------------------

/// Numeric priority for task scheduling. Lower values are higher priority.
///
/// This is distinct from [`message::Priority`](crate::message::Priority)
/// because task scheduling may use a continuous numeric range rather than
/// discrete buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskPriority(u32);

impl TaskPriority {
    /// Highest possible priority.
    pub const MAX: Self = Self(0);
    /// Default priority for most tasks.
    pub const DEFAULT: Self = Self(100);
    /// Lowest priority (background work).
    pub const MIN: Self = Self(u32::MAX);

    /// Create a new priority with the given numeric weight.
    /// Lower values indicate higher priority.
    pub fn new(weight: u32) -> Self {
        Self(weight)
    }

    /// Return the numeric weight.
    pub fn weight(&self) -> u32 {
        self.0
    }
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl PartialOrd for TaskPriority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TaskPriority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Lower weight = higher priority, so we reverse the comparison.
        self.0.cmp(&other.0)
    }
}

impl fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "priority({})", self.0)
    }
}

// ---------------------------------------------------------------------------
// TaskSpec
// ---------------------------------------------------------------------------

/// Declarative specification of a task to be executed.
///
/// The orchestrator constructs task specs and the scheduler matches them
/// against agent capabilities for assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Unique task identifier.
    pub id: TaskId,
    /// Human-readable task name.
    pub name: String,
    /// Detailed description of what the task should accomplish.
    pub description: String,
    /// Which kind of agent is required (or `None` for any).
    pub agent_kind_required: Option<AgentKind>,
    /// Structured input data for the task.
    pub inputs: serde_json::Value,
    /// Maximum wall-clock time before the task is killed.
    pub timeout: Duration,
    /// Maximum number of retry attempts on failure.
    pub max_retries: u32,
    /// Tasks that must complete successfully before this one can start.
    pub dependencies: Vec<TaskId>,
    /// Scheduling priority.
    pub priority: TaskPriority,
    /// Required capability names the executing agent must advertise.
    pub required_capabilities: Vec<String>,
    /// Arbitrary labels for filtering and grouping.
    pub labels: std::collections::HashMap<String, String>,
}

impl TaskSpec {
    /// Create a minimal task spec with sensible defaults.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: TaskId::new(),
            name: name.into(),
            description: description.into(),
            agent_kind_required: None,
            inputs: serde_json::Value::Object(serde_json::Map::new()),
            timeout: Duration::from_secs(300),
            max_retries: 3,
            dependencies: Vec::new(),
            priority: TaskPriority::default(),
            required_capabilities: Vec::new(),
            labels: std::collections::HashMap::new(),
        }
    }

    /// Set the required agent kind.
    pub fn with_agent_kind(mut self, kind: AgentKind) -> Self {
        self.agent_kind_required = Some(kind);
        self
    }

    /// Set the structured inputs.
    pub fn with_inputs(mut self, inputs: serde_json::Value) -> Self {
        self.inputs = inputs;
        self
    }

    /// Set the execution timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the max retry count.
    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Add a dependency on another task.
    pub fn depends_on(mut self, dep: TaskId) -> Self {
        self.dependencies.push(dep);
        self
    }

    /// Set the priority.
    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Require a specific capability.
    pub fn require_capability(mut self, name: impl Into<String>) -> Self {
        self.required_capabilities.push(name.into());
        self
    }

    /// Returns `true` if all dependencies are in the provided set of
    /// completed task IDs.
    pub fn dependencies_satisfied(&self, completed: &std::collections::HashSet<TaskId>) -> bool {
        self.dependencies.iter().all(|dep| completed.contains(dep))
    }
}

// ---------------------------------------------------------------------------
// TaskResult
// ---------------------------------------------------------------------------

/// The outcome of executing a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Which task produced this result.
    pub task_id: TaskId,
    /// Final state (should be terminal).
    pub state: TaskState,
    /// Structured output data.
    pub output: serde_json::Value,
    /// Error message if the task failed.
    pub error: Option<String>,
    /// Wall-clock duration of execution.
    pub duration: Duration,
    /// Agent that executed the task.
    pub executed_by: Option<AgentId>,
    /// Attempt number (1-based).
    pub attempt: u32,
    /// Arbitrary metrics emitted during execution.
    pub metrics: std::collections::HashMap<String, f64>,
    /// Timestamp when the result was produced.
    pub completed_at: DateTime<Utc>,
}

impl TaskResult {
    /// Create a successful result.
    pub fn success(task_id: TaskId, output: serde_json::Value, duration: Duration) -> Self {
        Self {
            task_id,
            state: TaskState::Succeeded,
            output,
            error: None,
            duration,
            executed_by: None,
            attempt: 1,
            metrics: std::collections::HashMap::new(),
            completed_at: Utc::now(),
        }
    }

    /// Create a failure result.
    pub fn failure(task_id: TaskId, error: impl Into<String>, duration: Duration) -> Self {
        Self {
            task_id,
            state: TaskState::Failed,
            output: serde_json::Value::Null,
            error: Some(error.into()),
            duration,
            executed_by: None,
            attempt: 1,
            metrics: std::collections::HashMap::new(),
            completed_at: Utc::now(),
        }
    }

    /// Set the executing agent.
    pub fn with_agent(mut self, agent: AgentId) -> Self {
        self.executed_by = Some(agent);
        self
    }

    /// Set the attempt number.
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    /// Record a metric.
    pub fn with_metric(mut self, name: impl Into<String>, value: f64) -> Self {
        self.metrics.insert(name.into(), value);
        self
    }

    /// Returns `true` if the task succeeded.
    pub fn is_success(&self) -> bool {
        self.state == TaskState::Succeeded
    }
}

// ---------------------------------------------------------------------------
// TaskHandle
// ---------------------------------------------------------------------------

/// Handle for tracking and controlling a running task.
///
/// Held by the orchestrator to monitor progress, cancel, or await
/// completion of a task that has been dispatched to an agent.
#[derive(Debug, Clone)]
pub struct TaskHandle {
    /// Task identifier.
    pub task_id: TaskId,
    /// Current state.
    pub state: TaskState,
    /// Agent currently assigned to this task.
    pub assigned_to: Option<AgentId>,
    /// When the task was dispatched.
    pub dispatched_at: Option<DateTime<Utc>>,
    /// When the current state was entered.
    pub state_changed_at: DateTime<Utc>,
    /// Number of attempts so far.
    pub attempts: u32,
}

impl TaskHandle {
    /// Create a new handle for a pending task.
    pub fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            state: TaskState::Pending,
            assigned_to: None,
            dispatched_at: None,
            state_changed_at: Utc::now(),
            attempts: 0,
        }
    }

    /// Transition to a new state.
    pub fn transition(&mut self, new_state: TaskState) {
        self.state = new_state;
        self.state_changed_at = Utc::now();
    }

    /// Assign the task to an agent and move to Queued.
    pub fn assign(&mut self, agent: AgentId) {
        self.assigned_to = Some(agent);
        self.dispatched_at = Some(Utc::now());
        self.transition(TaskState::Queued);
    }

    /// Record a retry attempt.
    pub fn record_retry(&mut self) {
        self.attempts += 1;
        self.transition(TaskState::Retrying);
    }

    /// Duration since the task was dispatched, or `None` if not yet dispatched.
    pub fn elapsed(&self) -> Option<Duration> {
        self.dispatched_at.map(|t| {
            (Utc::now() - t)
                .to_std()
                .unwrap_or(Duration::ZERO)
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_time_ordered() {
        let a = TaskId::new();
        std::thread::sleep(Duration::from_millis(2));
        let b = TaskId::new();
        assert!(a.as_uuid() < b.as_uuid());
    }

    #[test]
    fn task_state_terminal() {
        assert!(TaskState::Succeeded.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Cancelled.is_terminal());
        assert!(!TaskState::Running.is_terminal());
        assert!(!TaskState::Pending.is_terminal());
    }

    #[test]
    fn task_priority_ordering() {
        let high = TaskPriority::new(10);
        let low = TaskPriority::new(100);
        assert!(high < low, "lower weight = higher priority = sorts first");
    }

    #[test]
    fn task_spec_builder() {
        let dep = TaskId::new();
        let spec = TaskSpec::new("lint", "Run linter")
            .with_agent_kind(AgentKind::Worker)
            .with_timeout(Duration::from_secs(60))
            .with_max_retries(2)
            .depends_on(dep)
            .require_capability("lint");

        assert_eq!(spec.name, "lint");
        assert_eq!(spec.agent_kind_required, Some(AgentKind::Worker));
        assert_eq!(spec.timeout, Duration::from_secs(60));
        assert_eq!(spec.max_retries, 2);
        assert_eq!(spec.dependencies, vec![dep]);
        assert!(spec.required_capabilities.contains(&"lint".to_string()));
    }

    #[test]
    fn task_spec_dependencies_satisfied() {
        let dep1 = TaskId::new();
        let dep2 = TaskId::new();
        let spec = TaskSpec::new("build", "Build project")
            .depends_on(dep1)
            .depends_on(dep2);

        let mut completed = std::collections::HashSet::new();
        assert!(!spec.dependencies_satisfied(&completed));
        completed.insert(dep1);
        assert!(!spec.dependencies_satisfied(&completed));
        completed.insert(dep2);
        assert!(spec.dependencies_satisfied(&completed));
    }

    #[test]
    fn task_result_success() {
        let id = TaskId::new();
        let result =
            TaskResult::success(id, serde_json::json!({"ok": true}), Duration::from_secs(1));
        assert!(result.is_success());
        assert!(result.error.is_none());
    }

    #[test]
    fn task_result_failure() {
        let id = TaskId::new();
        let result = TaskResult::failure(id, "boom", Duration::from_secs(1));
        assert!(!result.is_success());
        assert_eq!(result.error.as_deref(), Some("boom"));
    }

    #[test]
    fn task_handle_lifecycle() {
        let id = TaskId::new();
        let mut handle = TaskHandle::new(id);
        assert_eq!(handle.state, TaskState::Pending);

        handle.assign(AgentId::new());
        assert_eq!(handle.state, TaskState::Queued);
        assert!(handle.assigned_to.is_some());
        assert!(handle.dispatched_at.is_some());

        handle.transition(TaskState::Running);
        assert_eq!(handle.state, TaskState::Running);

        handle.record_retry();
        assert_eq!(handle.state, TaskState::Retrying);
        assert_eq!(handle.attempts, 1);
    }

    #[test]
    fn task_spec_serde_roundtrip() {
        let spec = TaskSpec::new("test", "A test task")
            .with_inputs(serde_json::json!({"x": 1}));
        let json = serde_json::to_string(&spec).unwrap();
        let back: TaskSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "test");
        assert_eq!(back.inputs["x"], 1);
    }
}
