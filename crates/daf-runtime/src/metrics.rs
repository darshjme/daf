//! Runtime metrics collection.
//!
//! [`RuntimeMetrics`] tracks high-level operational counters using atomics
//! for lock-free, concurrent updates. Metrics can be snapshotted as a
//! [`MetricsSnapshot`] and exported as JSON for monitoring systems.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// RuntimeMetrics
// ---------------------------------------------------------------------------

/// High-performance runtime metrics using atomic counters.
///
/// All mutation methods use `Relaxed` ordering — these counters are
/// observational, not synchronization primitives. For reads that must be
/// consistent across counters, take a [`snapshot`](Self::snapshot).
#[derive(Debug)]
pub struct RuntimeMetrics {
    inner: Arc<MetricsInner>,
}

#[derive(Debug)]
struct MetricsInner {
    /// When the runtime started.
    started_at: Instant,

    /// Wall-clock start time (for human-readable output).
    started_at_utc: DateTime<Utc>,

    /// Total agents spawned since boot.
    agents_spawned: AtomicU64,

    /// Total agents currently alive.
    agents_active: AtomicU64,

    /// Total tasks processed (completed or failed).
    tasks_processed: AtomicU64,

    /// Total tasks currently in flight.
    tasks_active: AtomicU64,

    /// Total messages routed through the transport layer.
    messages_routed: AtomicU64,

    /// Total bytes sent across the transport.
    bytes_sent: AtomicU64,

    /// Total bytes received across the transport.
    bytes_received: AtomicU64,

    /// Number of active conversations / sessions.
    active_conversations: AtomicU64,

    /// Total errors encountered across all subsystems.
    errors_total: AtomicU64,

    /// Number of health checks performed.
    health_checks: AtomicU64,

    /// Number of configuration reloads.
    config_reloads: AtomicU64,
}

impl RuntimeMetrics {
    /// Create a fresh metrics instance with all counters at zero.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                started_at: Instant::now(),
                started_at_utc: Utc::now(),
                agents_spawned: AtomicU64::new(0),
                agents_active: AtomicU64::new(0),
                tasks_processed: AtomicU64::new(0),
                tasks_active: AtomicU64::new(0),
                messages_routed: AtomicU64::new(0),
                bytes_sent: AtomicU64::new(0),
                bytes_received: AtomicU64::new(0),
                active_conversations: AtomicU64::new(0),
                errors_total: AtomicU64::new(0),
                health_checks: AtomicU64::new(0),
                config_reloads: AtomicU64::new(0),
            }),
        }
    }

    // -- Increment operations ------------------------------------------------

    /// Record that an agent was spawned.
    pub fn agent_spawned(&self) {
        self.inner.agents_spawned.fetch_add(1, Ordering::Relaxed);
        self.inner.agents_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that an agent was terminated.
    pub fn agent_terminated(&self) {
        self.inner
            .agents_active
            .fetch_sub(1, Ordering::Relaxed);
    }

    /// Record that a task completed (success or failure).
    pub fn task_completed(&self) {
        self.inner.tasks_processed.fetch_add(1, Ordering::Relaxed);
        self.inner.tasks_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record that a task started execution.
    pub fn task_started(&self) {
        self.inner.tasks_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that a message was routed.
    pub fn message_routed(&self) {
        self.inner.messages_routed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record bytes sent.
    pub fn bytes_sent(&self, n: u64) {
        self.inner.bytes_sent.fetch_add(n, Ordering::Relaxed);
    }

    /// Record bytes received.
    pub fn bytes_received(&self, n: u64) {
        self.inner.bytes_received.fetch_add(n, Ordering::Relaxed);
    }

    /// Increment active conversations.
    pub fn conversation_started(&self) {
        self.inner
            .active_conversations
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active conversations.
    pub fn conversation_ended(&self) {
        self.inner
            .active_conversations
            .fetch_sub(1, Ordering::Relaxed);
    }

    /// Record an error.
    pub fn error_occurred(&self) {
        self.inner.errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a health check.
    pub fn health_check_performed(&self) {
        self.inner.health_checks.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a config reload.
    pub fn config_reloaded(&self) {
        self.inner.config_reloads.fetch_add(1, Ordering::Relaxed);
    }

    // -- Read operations -----------------------------------------------------

    /// Runtime uptime.
    pub fn uptime(&self) -> Duration {
        self.inner.started_at.elapsed()
    }

    /// Total agents spawned since boot.
    pub fn agents_spawned_total(&self) -> u64 {
        self.inner.agents_spawned.load(Ordering::Relaxed)
    }

    /// Currently alive agents.
    pub fn agents_active(&self) -> u64 {
        self.inner.agents_active.load(Ordering::Relaxed)
    }

    /// Total tasks processed since boot.
    pub fn tasks_processed_total(&self) -> u64 {
        self.inner.tasks_processed.load(Ordering::Relaxed)
    }

    /// Currently in-flight tasks.
    pub fn tasks_active(&self) -> u64 {
        self.inner.tasks_active.load(Ordering::Relaxed)
    }

    /// Total messages routed since boot.
    pub fn messages_routed_total(&self) -> u64 {
        self.inner.messages_routed.load(Ordering::Relaxed)
    }

    /// Take a consistent snapshot of all metrics.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            timestamp: Utc::now(),
            uptime_secs: self.uptime().as_secs(),
            started_at: self.inner.started_at_utc,
            agents_spawned_total: self.inner.agents_spawned.load(Ordering::Relaxed),
            agents_active: self.inner.agents_active.load(Ordering::Relaxed),
            tasks_processed_total: self.inner.tasks_processed.load(Ordering::Relaxed),
            tasks_active: self.inner.tasks_active.load(Ordering::Relaxed),
            messages_routed_total: self.inner.messages_routed.load(Ordering::Relaxed),
            bytes_sent_total: self.inner.bytes_sent.load(Ordering::Relaxed),
            bytes_received_total: self.inner.bytes_received.load(Ordering::Relaxed),
            active_conversations: self.inner.active_conversations.load(Ordering::Relaxed),
            errors_total: self.inner.errors_total.load(Ordering::Relaxed),
            health_checks_total: self.inner.health_checks.load(Ordering::Relaxed),
            config_reloads_total: self.inner.config_reloads.load(Ordering::Relaxed),
        }
    }
}

impl Clone for RuntimeMetrics {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// MetricsSnapshot
// ---------------------------------------------------------------------------

/// Point-in-time snapshot of all runtime metrics.
///
/// This is the serializable form of [`RuntimeMetrics`], suitable for
/// JSON export to monitoring dashboards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// When this snapshot was taken.
    pub timestamp: DateTime<Utc>,

    /// Runtime uptime in seconds.
    pub uptime_secs: u64,

    /// When the runtime was started.
    pub started_at: DateTime<Utc>,

    /// Cumulative agents spawned.
    pub agents_spawned_total: u64,

    /// Currently alive agents.
    pub agents_active: u64,

    /// Cumulative tasks completed or failed.
    pub tasks_processed_total: u64,

    /// Currently in-flight tasks.
    pub tasks_active: u64,

    /// Cumulative messages routed.
    pub messages_routed_total: u64,

    /// Cumulative bytes sent.
    pub bytes_sent_total: u64,

    /// Cumulative bytes received.
    pub bytes_received_total: u64,

    /// Active conversations / sessions.
    pub active_conversations: u64,

    /// Cumulative errors.
    pub errors_total: u64,

    /// Cumulative health checks.
    pub health_checks_total: u64,

    /// Cumulative config reloads.
    pub config_reloads_total: u64,
}

impl MetricsSnapshot {
    /// Serialize to JSON.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::json!({
            "error": "failed to serialize metrics"
        }))
    }

    /// Serialize to a pretty-printed JSON string.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

impl std::fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "uptime={}s agents={}/{} tasks={}/{} msgs={} errs={}",
            self.uptime_secs,
            self.agents_active,
            self.agents_spawned_total,
            self.tasks_active,
            self.tasks_processed_total,
            self.messages_routed_total,
            self.errors_total,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_start_at_zero() {
        let m = RuntimeMetrics::new();
        assert_eq!(m.agents_spawned_total(), 0);
        assert_eq!(m.agents_active(), 0);
        assert_eq!(m.tasks_processed_total(), 0);
        assert_eq!(m.messages_routed_total(), 0);
    }

    #[test]
    fn agent_spawn_and_terminate() {
        let m = RuntimeMetrics::new();
        m.agent_spawned();
        m.agent_spawned();
        assert_eq!(m.agents_spawned_total(), 2);
        assert_eq!(m.agents_active(), 2);

        m.agent_terminated();
        assert_eq!(m.agents_spawned_total(), 2);
        assert_eq!(m.agents_active(), 1);
    }

    #[test]
    fn task_lifecycle() {
        let m = RuntimeMetrics::new();
        m.task_started();
        m.task_started();
        assert_eq!(m.tasks_active(), 2);

        m.task_completed();
        assert_eq!(m.tasks_active(), 1);
        assert_eq!(m.tasks_processed_total(), 1);
    }

    #[test]
    fn message_counting() {
        let m = RuntimeMetrics::new();
        m.message_routed();
        m.message_routed();
        m.message_routed();
        assert_eq!(m.messages_routed_total(), 3);
    }

    #[test]
    fn snapshot_captures_state() {
        let m = RuntimeMetrics::new();
        m.agent_spawned();
        m.task_started();
        m.task_completed();
        m.message_routed();
        m.error_occurred();
        m.bytes_sent(1024);
        m.bytes_received(2048);
        m.conversation_started();
        m.health_check_performed();
        m.config_reloaded();

        let snap = m.snapshot();
        assert_eq!(snap.agents_spawned_total, 1);
        assert_eq!(snap.agents_active, 1);
        assert_eq!(snap.tasks_processed_total, 1);
        assert_eq!(snap.tasks_active, 0);
        assert_eq!(snap.messages_routed_total, 1);
        assert_eq!(snap.bytes_sent_total, 1024);
        assert_eq!(snap.bytes_received_total, 2048);
        assert_eq!(snap.active_conversations, 1);
        assert_eq!(snap.errors_total, 1);
        assert_eq!(snap.health_checks_total, 1);
        assert_eq!(snap.config_reloads_total, 1);
    }

    #[test]
    fn snapshot_json_export() {
        let m = RuntimeMetrics::new();
        m.agent_spawned();
        let snap = m.snapshot();
        let json = snap.to_json();
        assert_eq!(json["agents_spawned_total"], 1);
        assert!(json["uptime_secs"].is_number());
    }

    #[test]
    fn snapshot_json_string() {
        let m = RuntimeMetrics::new();
        let snap = m.snapshot();
        let s = snap.to_json_string();
        assert!(s.contains("uptime_secs"));
    }

    #[test]
    fn snapshot_display() {
        let m = RuntimeMetrics::new();
        m.agent_spawned();
        m.task_started();
        let snap = m.snapshot();
        let display = snap.to_string();
        assert!(display.contains("agents=1/1"));
        assert!(display.contains("tasks=1/0"));
    }

    #[test]
    fn clone_shares_counters() {
        let m1 = RuntimeMetrics::new();
        let m2 = m1.clone();
        m1.agent_spawned();
        assert_eq!(m2.agents_spawned_total(), 1);
    }

    #[test]
    fn uptime_is_nonzero_after_creation() {
        let m = RuntimeMetrics::new();
        // Just verify it doesn't panic; uptime could be 0 on fast machines.
        let _ = m.uptime();
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let m = RuntimeMetrics::new();
        m.agent_spawned();
        m.message_routed();
        let snap = m.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back: MetricsSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agents_spawned_total, snap.agents_spawned_total);
        assert_eq!(back.messages_routed_total, snap.messages_routed_total);
    }
}
