//! Orchestrator metrics and telemetry.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Per-agent metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub total_execution_ms: u64,
}

/// Aggregate orchestrator metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestratorMetrics {
    pub agents_spawned: u64,
    pub agents_removed: u64,
    pub tasks_dispatched: u64,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub missions_started: u64,
    pub missions_completed: u64,
}

/// Lock-free metrics collector using atomic counters.
#[derive(Debug)]
pub struct MetricsCollector {
    agents_spawned: AtomicU64,
    agents_removed: AtomicU64,
    tasks_dispatched: AtomicU64,
    tasks_completed: AtomicU64,
    tasks_failed: AtomicU64,
    missions_started: AtomicU64,
    missions_completed: AtomicU64,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            agents_spawned: AtomicU64::new(0),
            agents_removed: AtomicU64::new(0),
            tasks_dispatched: AtomicU64::new(0),
            tasks_completed: AtomicU64::new(0),
            tasks_failed: AtomicU64::new(0),
            missions_started: AtomicU64::new(0),
            missions_completed: AtomicU64::new(0),
        }
    }

    pub fn agent_spawned(&self) {
        self.agents_spawned.fetch_add(1, Ordering::Relaxed);
    }

    pub fn agent_removed(&self) {
        self.agents_removed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn task_dispatched(&self) {
        self.tasks_dispatched.fetch_add(1, Ordering::Relaxed);
    }

    pub fn task_completed(&self) {
        self.tasks_completed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn task_failed(&self) {
        self.tasks_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn mission_started(&self) {
        self.missions_started.fetch_add(1, Ordering::Relaxed);
    }

    pub fn mission_completed(&self) {
        self.missions_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Take a snapshot of all metrics.
    pub fn snapshot(&self) -> OrchestratorMetrics {
        OrchestratorMetrics {
            agents_spawned: self.agents_spawned.load(Ordering::Relaxed),
            agents_removed: self.agents_removed.load(Ordering::Relaxed),
            tasks_dispatched: self.tasks_dispatched.load(Ordering::Relaxed),
            tasks_completed: self.tasks_completed.load(Ordering::Relaxed),
            tasks_failed: self.tasks_failed.load(Ordering::Relaxed),
            missions_started: self.missions_started.load(Ordering::Relaxed),
            missions_completed: self.missions_completed.load(Ordering::Relaxed),
        }
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}
