//! Sprint execution — the GSD (Get Stuff Done) methodology.
//!
//! Sprints are time-boxed execution windows that decompose a mission into
//! manageable waves of parallel work. The sprint engine:
//!
//! 1. Plans the sprint by breaking a mission's phases into waves.
//! 2. Executes each wave concurrently, waiting for all tasks to finish.
//! 3. Tracks progress in real-time (tasks done, ETA, current wave).
//! 4. Captures a retrospective at the end for memory and learning.
//!
//! This is what gives DAF its "get stuff done" personality — instead of
//! executing tasks one-by-one, it aggressively parallelizes within waves
//! while respecting dependency ordering between waves.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};
use uuid::Uuid;

use daf_core::error::DafResult;
use daf_core::AgentId;
use daf_graph::node::TaskSpec;

use crate::mission::{Mission, MissionId};

// ---------------------------------------------------------------------------
// SprintId
// ---------------------------------------------------------------------------

/// Time-ordered sprint identifier backed by UUID v7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SprintId(pub Uuid);

impl SprintId {
    /// Generate a new time-ordered sprint identifier.
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

impl Default for SprintId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SprintId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sprint:{}", &self.0.to_string()[..8])
    }
}

// ---------------------------------------------------------------------------
// SprintState
// ---------------------------------------------------------------------------

/// Lifecycle state of a sprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SprintState {
    /// Sprint is being planned — waves are being computed.
    Planning,
    /// Sprint is actively executing waves.
    Running,
    /// Sprint is paused between waves.
    Paused,
    /// All waves completed successfully.
    Completed,
    /// Sprint failed — one or more waves had unrecoverable errors.
    Failed,
    /// Sprint was cancelled.
    Cancelled,
}

impl SprintState {
    /// Returns `true` if this is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for SprintState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Planning => "planning",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Wave
// ---------------------------------------------------------------------------

/// A wave is a set of tasks that can execute concurrently because all their
/// dependencies are satisfied. Waves are the unit of parallelism within a
/// sprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wave {
    /// Wave index (0-based) within the sprint.
    pub index: usize,
    /// Tasks to execute in this wave.
    pub tasks: Vec<TaskSpec>,
    /// Phase name this wave belongs to.
    pub phase_name: String,
    /// Maximum concurrency for this wave (from the phase's concurrency_limit).
    pub concurrency_limit: Option<usize>,
}

impl fmt::Display for Wave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Wave({idx}, phase={phase}, {n} tasks)",
            idx = self.index,
            phase = self.phase_name,
            n = self.tasks.len(),
        )
    }
}

// ---------------------------------------------------------------------------
// WaveResult
// ---------------------------------------------------------------------------

/// The outcome of executing a single wave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveResult {
    /// Wave index within the sprint.
    pub wave_index: usize,
    /// Phase name this wave belonged to.
    pub phase_name: String,
    /// Number of tasks that succeeded.
    pub tasks_succeeded: usize,
    /// Number of tasks that failed.
    pub tasks_failed: usize,
    /// Wall-clock duration of the wave.
    pub duration: Duration,
    /// Individual task outcomes.
    pub task_outcomes: Vec<TaskOutcome>,
}

/// Outcome of a single task within a wave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    /// The task type identifier.
    pub task_type: String,
    /// Whether the task succeeded.
    pub succeeded: bool,
    /// Error message if the task failed.
    pub error: Option<String>,
    /// Which agent executed this task.
    pub agent_id: Option<AgentId>,
    /// How long the task took.
    pub duration: Duration,
}

impl WaveResult {
    /// Returns `true` if all tasks in the wave succeeded.
    pub fn is_success(&self) -> bool {
        self.tasks_failed == 0
    }

    /// Total number of tasks in the wave.
    pub fn total_tasks(&self) -> usize {
        self.tasks_succeeded + self.tasks_failed
    }
}

impl fmt::Display for WaveResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.is_success() { "OK" } else { "FAILED" };
        write!(
            f,
            "[{status}] Wave {idx} ({succ}/{total} tasks, {dur:?})",
            idx = self.wave_index,
            succ = self.tasks_succeeded,
            total = self.total_tasks(),
            dur = self.duration,
        )
    }
}

// ---------------------------------------------------------------------------
// SprintProgress
// ---------------------------------------------------------------------------

/// Real-time progress tracking for a running sprint.
///
/// All fields use atomic operations so the progress can be read from
/// any thread without holding a lock.
#[derive(Debug)]
pub struct SprintProgress {
    /// Total number of tasks across all waves.
    pub tasks_total: AtomicUsize,
    /// Number of tasks completed (success or failure).
    pub tasks_done: AtomicUsize,
    /// Number of tasks that succeeded.
    pub tasks_succeeded: AtomicUsize,
    /// Number of tasks that failed.
    pub tasks_failed: AtomicUsize,
    /// Index of the current wave being executed.
    pub current_wave: AtomicUsize,
    /// Total number of waves in the sprint.
    pub total_waves: AtomicUsize,
    /// When the sprint started executing.
    pub started_at: RwLock<Option<DateTime<Utc>>>,
}

impl SprintProgress {
    /// Create a new progress tracker.
    pub fn new(total_tasks: usize, total_waves: usize) -> Self {
        Self {
            tasks_total: AtomicUsize::new(total_tasks),
            tasks_done: AtomicUsize::new(0),
            tasks_succeeded: AtomicUsize::new(0),
            tasks_failed: AtomicUsize::new(0),
            current_wave: AtomicUsize::new(0),
            total_waves: AtomicUsize::new(total_waves),
            started_at: RwLock::new(None),
        }
    }

    /// Record a task success.
    pub fn record_success(&self) {
        self.tasks_done.fetch_add(1, Ordering::Relaxed);
        self.tasks_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a task failure.
    pub fn record_failure(&self) {
        self.tasks_done.fetch_add(1, Ordering::Relaxed);
        self.tasks_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Advance to the next wave.
    pub fn advance_wave(&self) {
        self.current_wave.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark the start time.
    pub fn mark_started(&self) {
        *self.started_at.write() = Some(Utc::now());
    }

    /// Completion percentage (0.0..100.0).
    pub fn percent_complete(&self) -> f64 {
        let total = self.tasks_total.load(Ordering::Relaxed);
        if total == 0 {
            return 100.0;
        }
        let done = self.tasks_done.load(Ordering::Relaxed);
        (done as f64 / total as f64) * 100.0
    }

    /// Estimated time remaining based on current throughput.
    pub fn eta(&self) -> Option<Duration> {
        let started = (*self.started_at.read())?;
        let elapsed = (Utc::now() - started).to_std().ok()?;
        let done = self.tasks_done.load(Ordering::Relaxed);
        if done == 0 {
            return None;
        }
        let total = self.tasks_total.load(Ordering::Relaxed);
        let remaining = total.saturating_sub(done);
        let avg_per_task = elapsed.as_secs_f64() / done as f64;
        Some(Duration::from_secs_f64(avg_per_task * remaining as f64))
    }

    /// Create a serializable snapshot of the current progress.
    pub fn snapshot(&self) -> SprintProgressSnapshot {
        SprintProgressSnapshot {
            tasks_total: self.tasks_total.load(Ordering::Relaxed),
            tasks_done: self.tasks_done.load(Ordering::Relaxed),
            tasks_succeeded: self.tasks_succeeded.load(Ordering::Relaxed),
            tasks_failed: self.tasks_failed.load(Ordering::Relaxed),
            current_wave: self.current_wave.load(Ordering::Relaxed),
            total_waves: self.total_waves.load(Ordering::Relaxed),
            percent_complete: self.percent_complete(),
            eta_seconds: self.eta().map(|d| d.as_secs()),
        }
    }
}

/// Serializable snapshot of sprint progress for external consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintProgressSnapshot {
    pub tasks_total: usize,
    pub tasks_done: usize,
    pub tasks_succeeded: usize,
    pub tasks_failed: usize,
    pub current_wave: usize,
    pub total_waves: usize,
    pub percent_complete: f64,
    pub eta_seconds: Option<u64>,
}

// ---------------------------------------------------------------------------
// SprintRetrospective
// ---------------------------------------------------------------------------

/// Post-sprint analysis capturing what worked, what didn't, and lessons
/// learned. Stored in the memory system for future planning improvements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintRetrospective {
    /// Sprint identifier.
    pub sprint_id: SprintId,
    /// Mission this sprint was part of.
    pub mission_id: MissionId,
    /// When the retrospective was conducted.
    pub timestamp: DateTime<Utc>,
    /// Overall success rate.
    pub success_rate: f64,
    /// Total wall-clock duration.
    pub total_duration: Duration,
    /// Number of waves executed.
    pub waves_executed: usize,
    /// Things that went well.
    pub went_well: Vec<String>,
    /// Things that didn't go well.
    pub went_poorly: Vec<String>,
    /// Actionable improvements for next time.
    pub improvements: Vec<String>,
    /// Per-agent performance data.
    pub agent_performance: HashMap<String, AgentSprintPerformance>,
}

/// Per-agent performance within a sprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSprintPerformance {
    /// Number of tasks this agent handled.
    pub tasks_handled: usize,
    /// Number of tasks that succeeded.
    pub tasks_succeeded: usize,
    /// Average task duration.
    pub avg_duration: Duration,
    /// Whether this agent was a bottleneck (slowest in its wave).
    pub was_bottleneck: bool,
}

// ---------------------------------------------------------------------------
// Sprint
// ---------------------------------------------------------------------------

/// A time-boxed execution window for a subset of mission work.
///
/// The sprint is the core execution primitive — it takes a sequence of
/// waves and executes them one-by-one, running all tasks within each wave
/// concurrently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sprint {
    /// Unique identifier.
    pub id: SprintId,
    /// Human-readable name.
    pub name: String,
    /// Mission this sprint is executing.
    pub mission_id: MissionId,
    /// Waves to execute, in order.
    pub waves: Vec<Wave>,
    /// Current state.
    pub state: SprintState,
    /// When the sprint was created.
    pub created_at: DateTime<Utc>,
    /// When the sprint started executing.
    pub started_at: Option<DateTime<Utc>>,
    /// Deadline — if the sprint doesn't finish by this time, it is failed.
    pub deadline: Option<DateTime<Utc>>,
    /// Results for completed waves.
    pub wave_results: Vec<WaveResult>,
}

impl Sprint {
    /// Create a new sprint from a set of waves.
    pub fn new(
        name: impl Into<String>,
        mission_id: MissionId,
        waves: Vec<Wave>,
    ) -> Self {
        Self {
            id: SprintId::new(),
            name: name.into(),
            mission_id,
            waves,
            state: SprintState::Planning,
            created_at: Utc::now(),
            started_at: None,
            deadline: None,
            wave_results: Vec::new(),
        }
    }

    /// Set a deadline for the sprint.
    pub fn with_deadline(mut self, deadline: DateTime<Utc>) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Total number of tasks across all waves.
    pub fn total_tasks(&self) -> usize {
        self.waves.iter().map(|w| w.tasks.len()).sum()
    }

    /// Check if the sprint has exceeded its deadline.
    pub fn is_overdue(&self) -> bool {
        if let Some(deadline) = self.deadline {
            Utc::now() > deadline
        } else {
            false
        }
    }

    /// Record a wave result and advance the sprint state.
    pub fn record_wave_result(&mut self, result: WaveResult) {
        let all_succeeded = result.is_success();
        self.wave_results.push(result);

        if !all_succeeded {
            self.state = SprintState::Failed;
        } else if self.wave_results.len() >= self.waves.len() {
            self.state = SprintState::Completed;
        }
    }

    /// Generate a retrospective from the sprint's execution data.
    pub fn retrospective(&self) -> SprintRetrospective {
        let total_tasks: usize = self
            .wave_results
            .iter()
            .map(|w| w.tasks_succeeded + w.tasks_failed)
            .sum();
        let succeeded: usize = self.wave_results.iter().map(|w| w.tasks_succeeded).sum();
        let success_rate = if total_tasks > 0 {
            succeeded as f64 / total_tasks as f64
        } else {
            1.0
        };

        let total_duration: Duration = self.wave_results.iter().map(|w| w.duration).sum();

        let mut went_well = Vec::new();
        let mut went_poorly = Vec::new();
        let mut improvements = Vec::new();

        if success_rate >= 0.95 {
            went_well.push(format!("High success rate: {:.1}%", success_rate * 100.0));
        }
        if success_rate < 0.8 {
            went_poorly.push(format!("Low success rate: {:.1}%", success_rate * 100.0));
            improvements.push("Investigate failing tasks and add better error handling".into());
        }

        let failed_waves: Vec<_> = self
            .wave_results
            .iter()
            .filter(|w| !w.is_success())
            .collect();
        if !failed_waves.is_empty() {
            went_poorly.push(format!("{} wave(s) had failures", failed_waves.len()));
        }

        if self.wave_results.len() > 1 {
            went_well.push(format!(
                "Executed {} waves with wave-based parallelism",
                self.wave_results.len()
            ));
        }

        SprintRetrospective {
            sprint_id: self.id,
            mission_id: self.mission_id,
            timestamp: Utc::now(),
            success_rate,
            total_duration,
            waves_executed: self.wave_results.len(),
            went_well,
            went_poorly,
            improvements,
            agent_performance: HashMap::new(),
        }
    }
}

impl fmt::Display for Sprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Sprint({id} \"{name}\", {n} waves, {state})",
            id = self.id,
            name = self.name,
            n = self.waves.len(),
            state = self.state,
        )
    }
}

// ---------------------------------------------------------------------------
// SprintPlanner
// ---------------------------------------------------------------------------

/// Decomposes a mission into sprints and waves for execution.
///
/// The planner uses the mission's phase dependency graph to determine which
/// phases can execute in parallel (forming waves), and groups waves into
/// sprints based on time budgets and resource constraints.
pub struct SprintPlanner {
    /// Maximum tasks per wave (for resource capping).
    pub max_tasks_per_wave: Option<usize>,
    /// Maximum duration per sprint.
    pub max_sprint_duration: Option<Duration>,
}

impl SprintPlanner {
    /// Create a planner with default settings.
    pub fn new() -> Self {
        Self {
            max_tasks_per_wave: None,
            max_sprint_duration: None,
        }
    }

    /// Set the maximum tasks per wave.
    pub fn with_max_tasks_per_wave(mut self, max: usize) -> Self {
        self.max_tasks_per_wave = Some(max);
        self
    }

    /// Set the maximum sprint duration.
    pub fn with_max_sprint_duration(mut self, dur: Duration) -> Self {
        self.max_sprint_duration = Some(dur);
        self
    }

    /// Plan a sprint from a mission by resolving the phase dependency graph
    /// into an ordered sequence of waves.
    ///
    /// Each phase becomes one or more waves (split if the phase has more
    /// tasks than `max_tasks_per_wave`). Phases that can execute in parallel
    /// (same wave level in the topo sort) have their waves interleaved.
    #[instrument(skip(self, mission), fields(mission = %mission.id))]
    pub fn plan(&self, mission: &Mission) -> DafResult<Sprint> {
        let phase_waves = mission.resolve_execution_order()?;

        let mut waves: Vec<Wave> = Vec::new();
        let mut wave_index = 0;

        for phase_group in &phase_waves {
            for phase in phase_group {
                let tasks = &phase.tasks;
                if tasks.is_empty() {
                    continue;
                }

                // Split into sub-waves if max_tasks_per_wave is set.
                let chunks: Vec<&[TaskSpec]> = if let Some(max) = self.max_tasks_per_wave {
                    tasks.chunks(max).collect()
                } else {
                    vec![tasks.as_slice()]
                };

                for chunk in chunks {
                    waves.push(Wave {
                        index: wave_index,
                        tasks: chunk.to_vec(),
                        phase_name: phase.name.clone(),
                        concurrency_limit: phase.concurrency_limit,
                    });
                    wave_index += 1;
                }
            }
        }

        info!(
            mission = %mission.id,
            waves = waves.len(),
            total_tasks = waves.iter().map(|w| w.tasks.len()).sum::<usize>(),
            "sprint planned"
        );

        let sprint_name = format!("sprint-{}", &mission.name);
        let mut sprint = Sprint::new(sprint_name, mission.id, waves);

        if let Some(dur) = self.max_sprint_duration {
            let deadline = Utc::now()
                + chrono::Duration::from_std(dur)
                    .unwrap_or_else(|_| chrono::Duration::hours(24));
            sprint = sprint.with_deadline(deadline);
        }

        Ok(sprint)
    }
}

impl Default for SprintPlanner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::{Mission, Phase};

    fn sample_task(name: &str) -> TaskSpec {
        TaskSpec {
            task_type: name.into(),
            params: serde_json::json!({}),
            timeout: None,
            max_retries: 0,
        }
    }

    #[test]
    fn sprint_id_uniqueness() {
        let a = SprintId::new();
        let b = SprintId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn plan_simple_mission() {
        let mission = Mission::builder("simple")
            .phase(
                Phase::new("build")
                    .task(sample_task("compile"))
                    .task(sample_task("lint")),
            )
            .phase(
                Phase::new("test")
                    .depends_on("build")
                    .task(sample_task("unit_test"))
                    .task(sample_task("integration_test")),
            )
            .build();

        let planner = SprintPlanner::new();
        let sprint = planner.plan(&mission).unwrap();

        assert_eq!(sprint.waves.len(), 2);
        assert_eq!(sprint.waves[0].tasks.len(), 2);
        assert_eq!(sprint.waves[1].tasks.len(), 2);
        assert_eq!(sprint.total_tasks(), 4);
    }

    #[test]
    fn plan_with_task_limit() {
        let mission = Mission::builder("chunked")
            .phase(
                Phase::new("build")
                    .task(sample_task("t1"))
                    .task(sample_task("t2"))
                    .task(sample_task("t3"))
                    .task(sample_task("t4"))
                    .task(sample_task("t5")),
            )
            .build();

        let planner = SprintPlanner::new().with_max_tasks_per_wave(2);
        let sprint = planner.plan(&mission).unwrap();

        // 5 tasks with max 2 per wave = 3 waves.
        assert_eq!(sprint.waves.len(), 3);
        assert_eq!(sprint.waves[0].tasks.len(), 2);
        assert_eq!(sprint.waves[1].tasks.len(), 2);
        assert_eq!(sprint.waves[2].tasks.len(), 1);
    }

    #[test]
    fn progress_tracking() {
        let progress = SprintProgress::new(10, 3);
        assert!((progress.percent_complete() - 0.0).abs() < f64::EPSILON);

        progress.record_success();
        progress.record_success();
        progress.record_failure();

        assert!((progress.percent_complete() - 30.0).abs() < f64::EPSILON);
        assert_eq!(progress.tasks_succeeded.load(Ordering::Relaxed), 2);
        assert_eq!(progress.tasks_failed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn progress_snapshot() {
        let progress = SprintProgress::new(5, 2);
        progress.record_success();
        progress.record_success();

        let snap = progress.snapshot();
        assert_eq!(snap.tasks_total, 5);
        assert_eq!(snap.tasks_done, 2);
        assert_eq!(snap.tasks_succeeded, 2);
        assert_eq!(snap.tasks_failed, 0);
        assert!((snap.percent_complete - 40.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sprint_records_wave_result() {
        let mut sprint = Sprint::new(
            "test",
            MissionId::new(),
            vec![
                Wave {
                    index: 0,
                    tasks: vec![sample_task("t1")],
                    phase_name: "build".into(),
                    concurrency_limit: None,
                },
                Wave {
                    index: 1,
                    tasks: vec![sample_task("t2")],
                    phase_name: "test".into(),
                    concurrency_limit: None,
                },
            ],
        );
        sprint.state = SprintState::Running;

        sprint.record_wave_result(WaveResult {
            wave_index: 0,
            phase_name: "build".into(),
            tasks_succeeded: 1,
            tasks_failed: 0,
            duration: Duration::from_secs(5),
            task_outcomes: vec![],
        });
        // Not yet complete.
        assert_eq!(sprint.state, SprintState::Running);

        sprint.record_wave_result(WaveResult {
            wave_index: 1,
            phase_name: "test".into(),
            tasks_succeeded: 1,
            tasks_failed: 0,
            duration: Duration::from_secs(3),
            task_outcomes: vec![],
        });
        assert_eq!(sprint.state, SprintState::Completed);
    }

    #[test]
    fn sprint_fails_on_wave_failure() {
        let mut sprint = Sprint::new(
            "failing",
            MissionId::new(),
            vec![Wave {
                index: 0,
                tasks: vec![sample_task("t1")],
                phase_name: "build".into(),
                concurrency_limit: None,
            }],
        );
        sprint.state = SprintState::Running;

        sprint.record_wave_result(WaveResult {
            wave_index: 0,
            phase_name: "build".into(),
            tasks_succeeded: 0,
            tasks_failed: 1,
            duration: Duration::from_secs(2),
            task_outcomes: vec![],
        });
        assert_eq!(sprint.state, SprintState::Failed);
    }

    #[test]
    fn retrospective_generation() {
        let mut sprint = Sprint::new("retro-test", MissionId::new(), vec![]);
        sprint.wave_results.push(WaveResult {
            wave_index: 0,
            phase_name: "build".into(),
            tasks_succeeded: 9,
            tasks_failed: 1,
            duration: Duration::from_secs(10),
            task_outcomes: vec![],
        });

        let retro = sprint.retrospective();
        assert!((retro.success_rate - 0.9).abs() < f64::EPSILON);
        assert_eq!(retro.waves_executed, 1);
    }

    #[test]
    fn sprint_deadline() {
        let sprint = Sprint::new("deadline", MissionId::new(), vec![])
            .with_deadline(Utc::now() - chrono::Duration::hours(1));
        assert!(sprint.is_overdue());

        let sprint = Sprint::new("no-deadline", MissionId::new(), vec![]);
        assert!(!sprint.is_overdue());
    }

    #[test]
    fn sprint_state_terminal() {
        assert!(SprintState::Completed.is_terminal());
        assert!(SprintState::Failed.is_terminal());
        assert!(SprintState::Cancelled.is_terminal());
        assert!(!SprintState::Running.is_terminal());
    }

    #[test]
    fn wave_display() {
        let wave = Wave {
            index: 2,
            tasks: vec![sample_task("a"), sample_task("b")],
            phase_name: "deploy".into(),
            concurrency_limit: Some(4),
        };
        let display = wave.to_string();
        assert!(display.contains("2"));
        assert!(display.contains("deploy"));
        assert!(display.contains("2 tasks"));
    }

    #[test]
    fn wave_result_success() {
        let result = WaveResult {
            wave_index: 0,
            phase_name: "build".into(),
            tasks_succeeded: 5,
            tasks_failed: 0,
            duration: Duration::from_secs(10),
            task_outcomes: vec![],
        };
        assert!(result.is_success());
        assert_eq!(result.total_tasks(), 5);

        let failed = WaveResult {
            wave_index: 1,
            phase_name: "test".into(),
            tasks_succeeded: 3,
            tasks_failed: 2,
            duration: Duration::from_secs(15),
            task_outcomes: vec![],
        };
        assert!(!failed.is_success());
    }
}
