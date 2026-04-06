//! System health monitoring.
//!
//! [`SystemHealth`] aggregates health status from every runtime subsystem into
//! a single view. The periodic health-check loop probes each subsystem and
//! produces a [`SystemHealth`] snapshot that can be served via a `/health`
//! HTTP endpoint or logged for observability.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// HealthStatus
// ---------------------------------------------------------------------------

/// Overall or per-subsystem health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Everything is operating normally.
    Healthy,
    /// The system is functional but at least one subsystem is impaired.
    Degraded,
    /// The system or subsystem is not functional.
    Unhealthy,
    /// Health status has not been determined yet (initial state).
    Unknown,
}

impl HealthStatus {
    /// Returns `true` if the status indicates normal operation.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// Combine two statuses, returning the worse of the two.
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unhealthy, _) | (_, Self::Unhealthy) => Self::Unhealthy,
            (Self::Degraded, _) | (_, Self::Degraded) => Self::Degraded,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Healthy, Self::Healthy) => Self::Healthy,
        }
    }
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        };
        write!(f, "{s}")
    }
}

impl Default for HealthStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

// ---------------------------------------------------------------------------
// SubsystemKind
// ---------------------------------------------------------------------------

/// Identifies a runtime subsystem for health reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemKind {
    Transport,
    Logger,
    Memory,
    Registry,
    Orchestrator,
}

impl fmt::Display for SubsystemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Transport => "transport",
            Self::Logger => "logger",
            Self::Memory => "memory",
            Self::Registry => "registry",
            Self::Orchestrator => "orchestrator",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// SubsystemHealth
// ---------------------------------------------------------------------------

/// Health report for a single subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemHealth {
    /// Which subsystem this report is for.
    pub kind: SubsystemKind,

    /// Current health status.
    pub status: HealthStatus,

    /// Human-readable description of the current state.
    pub message: String,

    /// When this check was last performed.
    pub last_checked: DateTime<Utc>,

    /// Additional key-value details (e.g., queue depth, connection count).
    pub details: HashMap<String, String>,
}

impl SubsystemHealth {
    /// Create a healthy subsystem report.
    pub fn healthy(kind: SubsystemKind) -> Self {
        Self {
            kind,
            status: HealthStatus::Healthy,
            message: "operating normally".into(),
            last_checked: Utc::now(),
            details: HashMap::new(),
        }
    }

    /// Create a degraded subsystem report.
    pub fn degraded(kind: SubsystemKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: HealthStatus::Degraded,
            message: message.into(),
            last_checked: Utc::now(),
            details: HashMap::new(),
        }
    }

    /// Create an unhealthy subsystem report.
    pub fn unhealthy(kind: SubsystemKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: HealthStatus::Unhealthy,
            message: message.into(),
            last_checked: Utc::now(),
            details: HashMap::new(),
        }
    }

    /// Attach a detail key-value pair.
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }
}

impl fmt::Display for SubsystemHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{status}] {kind}: {msg}", status = self.status, kind = self.kind, msg = self.message)
    }
}

// ---------------------------------------------------------------------------
// SystemHealth
// ---------------------------------------------------------------------------

/// Aggregated health snapshot of the entire runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHealth {
    /// Overall health — derived from the worst subsystem status.
    pub status: HealthStatus,

    /// Per-subsystem health reports.
    pub subsystems: HashMap<SubsystemKind, SubsystemHealth>,

    /// When this snapshot was produced.
    pub timestamp: DateTime<Utc>,

    /// Node name for context.
    pub node_name: String,

    /// DAF version.
    pub version: String,

    /// Uptime of the runtime in seconds.
    pub uptime_secs: u64,
}

impl SystemHealth {
    /// Build a system health snapshot from individual subsystem reports.
    pub fn from_subsystems(
        subsystems: Vec<SubsystemHealth>,
        node_name: impl Into<String>,
        version: impl Into<String>,
        uptime: Duration,
    ) -> Self {
        let overall = subsystems
            .iter()
            .map(|s| s.status)
            .fold(HealthStatus::Healthy, HealthStatus::merge);

        let map: HashMap<SubsystemKind, SubsystemHealth> =
            subsystems.into_iter().map(|s| (s.kind, s)).collect();

        Self {
            status: overall,
            subsystems: map,
            timestamp: Utc::now(),
            node_name: node_name.into(),
            version: version.into(),
            uptime_secs: uptime.as_secs(),
        }
    }

    /// Get the health of a specific subsystem.
    pub fn subsystem(&self, kind: SubsystemKind) -> Option<&SubsystemHealth> {
        self.subsystems.get(&kind)
    }

    /// Serialize to JSON (for `/health` endpoint).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::json!({
            "status": "unknown",
            "error": "failed to serialize health data"
        }))
    }
}

impl fmt::Display for SystemHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SystemHealth({status}, {n} subsystems, uptime={up}s)",
            status = self.status,
            n = self.subsystems.len(),
            up = self.uptime_secs,
        )
    }
}

// ---------------------------------------------------------------------------
// HealthMonitor
// ---------------------------------------------------------------------------

/// Periodic health-check loop that produces [`SystemHealth`] snapshots.
///
/// The monitor runs as a background task, calling registered health-check
/// functions at a configurable interval and storing the latest snapshot
/// for queries.
#[derive(Debug, Clone)]
pub struct HealthMonitor {
    inner: Arc<HealthMonitorInner>,
}

#[derive(Debug)]
struct HealthMonitorInner {
    /// Latest health snapshot.
    latest: RwLock<SystemHealth>,
    /// Check interval.
    interval: Duration,
    /// Node name.
    node_name: String,
    /// Runtime start time.
    started_at: DateTime<Utc>,
}

impl HealthMonitor {
    /// Create a new health monitor.
    pub fn new(
        node_name: impl Into<String>,
        interval: Duration,
    ) -> Self {
        let node_name = node_name.into();
        let initial = SystemHealth {
            status: HealthStatus::Unknown,
            subsystems: HashMap::new(),
            timestamp: Utc::now(),
            node_name: node_name.clone(),
            version: crate::VERSION.to_string(),
            uptime_secs: 0,
        };
        Self {
            inner: Arc::new(HealthMonitorInner {
                latest: RwLock::new(initial),
                interval,
                node_name,
                started_at: Utc::now(),
            }),
        }
    }

    /// Get the latest health snapshot.
    pub fn latest(&self) -> SystemHealth {
        self.inner.latest.read().clone()
    }

    /// Directly update the stored health snapshot.
    ///
    /// This is called by the runtime's health-check loop after probing all
    /// subsystems.
    pub fn update(&self, reports: Vec<SubsystemHealth>) {
        let uptime = (Utc::now() - self.inner.started_at)
            .to_std()
            .unwrap_or(Duration::ZERO);

        let health = SystemHealth::from_subsystems(
            reports,
            &self.inner.node_name,
            crate::VERSION,
            uptime,
        );

        if !health.status.is_healthy() {
            warn!(
                status = %health.status,
                "system health check: {}",
                health.status
            );
        } else {
            debug!("system health check: healthy");
        }

        *self.inner.latest.write() = health;
    }

    /// The configured check interval.
    pub fn interval(&self) -> Duration {
        self.inner.interval
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_merge() {
        assert_eq!(
            HealthStatus::Healthy.merge(HealthStatus::Healthy),
            HealthStatus::Healthy
        );
        assert_eq!(
            HealthStatus::Healthy.merge(HealthStatus::Degraded),
            HealthStatus::Degraded
        );
        assert_eq!(
            HealthStatus::Degraded.merge(HealthStatus::Unhealthy),
            HealthStatus::Unhealthy
        );
        assert_eq!(
            HealthStatus::Healthy.merge(HealthStatus::Unknown),
            HealthStatus::Unknown
        );
    }

    #[test]
    fn subsystem_health_constructors() {
        let h = SubsystemHealth::healthy(SubsystemKind::Transport);
        assert!(h.status.is_healthy());
        assert_eq!(h.kind, SubsystemKind::Transport);

        let d = SubsystemHealth::degraded(SubsystemKind::Memory, "high usage");
        assert_eq!(d.status, HealthStatus::Degraded);
        assert!(d.message.contains("high usage"));

        let u = SubsystemHealth::unhealthy(SubsystemKind::Registry, "connection lost");
        assert_eq!(u.status, HealthStatus::Unhealthy);
    }

    #[test]
    fn subsystem_health_with_details() {
        let h = SubsystemHealth::healthy(SubsystemKind::Transport)
            .with_detail("connections", "42")
            .with_detail("pending", "3");
        assert_eq!(h.details.get("connections").unwrap(), "42");
        assert_eq!(h.details.get("pending").unwrap(), "3");
    }

    #[test]
    fn system_health_aggregation() {
        let reports = vec![
            SubsystemHealth::healthy(SubsystemKind::Transport),
            SubsystemHealth::healthy(SubsystemKind::Logger),
            SubsystemHealth::degraded(SubsystemKind::Memory, "80% full"),
            SubsystemHealth::healthy(SubsystemKind::Registry),
            SubsystemHealth::healthy(SubsystemKind::Orchestrator),
        ];

        let health =
            SystemHealth::from_subsystems(reports, "test-node", "0.1.0", Duration::from_secs(60));

        // One degraded subsystem makes the overall status degraded.
        assert_eq!(health.status, HealthStatus::Degraded);
        assert_eq!(health.subsystems.len(), 5);
        assert_eq!(health.uptime_secs, 60);
    }

    #[test]
    fn system_health_all_healthy() {
        let reports = vec![
            SubsystemHealth::healthy(SubsystemKind::Transport),
            SubsystemHealth::healthy(SubsystemKind::Logger),
        ];

        let health =
            SystemHealth::from_subsystems(reports, "node", "0.1.0", Duration::from_secs(1));
        assert_eq!(health.status, HealthStatus::Healthy);
    }

    #[test]
    fn system_health_json() {
        let reports = vec![SubsystemHealth::healthy(SubsystemKind::Transport)];
        let health =
            SystemHealth::from_subsystems(reports, "node", "0.1.0", Duration::from_secs(5));
        let json = health.to_json();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["node_name"], "node");
    }

    #[test]
    fn health_monitor_update_and_read() {
        let monitor = HealthMonitor::new("test", Duration::from_secs(10));

        assert_eq!(monitor.latest().status, HealthStatus::Unknown);

        monitor.update(vec![
            SubsystemHealth::healthy(SubsystemKind::Transport),
            SubsystemHealth::healthy(SubsystemKind::Logger),
        ]);

        assert_eq!(monitor.latest().status, HealthStatus::Healthy);
    }

    #[test]
    fn health_status_display() {
        assert_eq!(HealthStatus::Healthy.to_string(), "healthy");
        assert_eq!(HealthStatus::Degraded.to_string(), "degraded");
        assert_eq!(HealthStatus::Unhealthy.to_string(), "unhealthy");
        assert_eq!(HealthStatus::Unknown.to_string(), "unknown");
    }

    #[test]
    fn subsystem_kind_display() {
        assert_eq!(SubsystemKind::Transport.to_string(), "transport");
        assert_eq!(SubsystemKind::Orchestrator.to_string(), "orchestrator");
    }

    #[test]
    fn system_health_serde_roundtrip() {
        let reports = vec![
            SubsystemHealth::healthy(SubsystemKind::Transport),
            SubsystemHealth::degraded(SubsystemKind::Memory, "test"),
        ];
        let health =
            SystemHealth::from_subsystems(reports, "node", "0.1.0", Duration::from_secs(100));

        let json = serde_json::to_string(&health).unwrap();
        let back: SystemHealth = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, health.status);
        assert_eq!(back.subsystems.len(), 2);
    }
}
