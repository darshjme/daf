//! SDK-level task and event types.
//!
//! These are thin wrappers around the core task types, extended with
//! SDK-specific conveniences for the builder and handler APIs.
//! They exist so that the SDK can evolve its developer-facing API
//! independently of the core protocol types.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use daf_core::AgentId;

// ---------------------------------------------------------------------------
// SdkTaskSpec
// ---------------------------------------------------------------------------

/// SDK-level task specification.
///
/// Wraps [`daf_core::task::TaskSpec`] with additional ergonomic helpers
/// for the handler API. Agents receive this type in their task handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkTaskSpec {
    /// Unique task identifier.
    pub id: Uuid,
    /// Human-readable task name.
    pub name: String,
    /// Detailed description.
    pub description: String,
    /// Structured input data.
    pub inputs: serde_json::Value,
    /// Maximum execution time.
    pub timeout: Duration,
    /// Required capability names.
    pub required_capabilities: Vec<String>,
    /// Arbitrary labels.
    pub labels: HashMap<String, String>,
}

impl SdkTaskSpec {
    /// Create a minimal task spec.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            description: description.into(),
            inputs: serde_json::Value::Object(serde_json::Map::new()),
            timeout: Duration::from_secs(300),
            required_capabilities: Vec::new(),
            labels: HashMap::new(),
        }
    }

    /// Set structured inputs.
    pub fn with_inputs(mut self, inputs: serde_json::Value) -> Self {
        self.inputs = inputs;
        self
    }

    /// Set the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Add a required capability.
    pub fn require_capability(mut self, cap: impl Into<String>) -> Self {
        self.required_capabilities.push(cap.into());
        self
    }

    /// Add a label.
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }
}

impl fmt::Display for SdkTaskSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Task({}: {})", self.name, &self.id.to_string()[..8])
    }
}

// ---------------------------------------------------------------------------
// SdkTaskResult
// ---------------------------------------------------------------------------

/// The outcome of executing an SDK-level task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkTaskResult {
    /// Which task produced this result.
    pub task_id: Uuid,
    /// Whether the task succeeded.
    pub success: bool,
    /// Structured output data.
    pub output: serde_json::Value,
    /// Error message if the task failed.
    pub error: Option<String>,
    /// Wall-clock duration of execution.
    pub duration: Duration,
    /// Arbitrary metrics emitted during execution.
    pub metrics: HashMap<String, f64>,
    /// Timestamp when the result was produced.
    pub completed_at: DateTime<Utc>,
}

impl SdkTaskResult {
    /// Create a successful result.
    pub fn success(task_id: Uuid, output: serde_json::Value, duration: Duration) -> Self {
        Self {
            task_id,
            success: true,
            output,
            error: None,
            duration,
            metrics: HashMap::new(),
            completed_at: Utc::now(),
        }
    }

    /// Create a failure result.
    pub fn failure(task_id: Uuid, error: impl Into<String>, duration: Duration) -> Self {
        Self {
            task_id,
            success: false,
            output: serde_json::Value::Null,
            error: Some(error.into()),
            duration,
            metrics: HashMap::new(),
            completed_at: Utc::now(),
        }
    }

    /// Record a metric.
    pub fn with_metric(mut self, name: impl Into<String>, value: f64) -> Self {
        self.metrics.insert(name.into(), value);
        self
    }
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// A system or domain event for the event handler API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event identifier.
    pub id: Uuid,
    /// Event type discriminator (e.g. `"agent:spawned"`, `"task:completed"`).
    pub event_type: String,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Agent that produced the event.
    pub source: Option<AgentId>,
    /// Structured event payload.
    pub data: serde_json::Value,
}

impl Event {
    /// Create a new event.
    pub fn new(event_type: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            id: Uuid::now_v7(),
            event_type: event_type.into(),
            timestamp: Utc::now(),
            source: None,
            data,
        }
    }

    /// Set the source agent.
    pub fn with_source(mut self, source: AgentId) -> Self {
        self.source = Some(source);
        self
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Event({}: {})",
            self.event_type,
            &self.id.to_string()[..8]
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sdk_task_spec_builder() {
        let spec = SdkTaskSpec::new("lint", "Run linter")
            .with_inputs(json!({"path": "/src"}))
            .with_timeout(Duration::from_secs(60))
            .require_capability("lint")
            .with_label("team", "platform");

        assert_eq!(spec.name, "lint");
        assert_eq!(spec.timeout, Duration::from_secs(60));
        assert!(spec.required_capabilities.contains(&"lint".to_string()));
        assert_eq!(spec.labels.get("team").unwrap(), "platform");
    }

    #[test]
    fn sdk_task_result_success() {
        let id = Uuid::now_v7();
        let result = SdkTaskResult::success(id, json!({"ok": true}), Duration::from_secs(1));
        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn sdk_task_result_failure() {
        let id = Uuid::now_v7();
        let result = SdkTaskResult::failure(id, "boom", Duration::from_secs(1));
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("boom"));
    }

    #[test]
    fn event_creation() {
        let event = Event::new("task:completed", json!({"task_id": "abc"}));
        assert_eq!(event.event_type, "task:completed");
        assert!(event.source.is_none());
    }

    #[test]
    fn event_with_source() {
        let agent = AgentId::new();
        let event = Event::new("agent:spawned", json!({})).with_source(agent);
        assert_eq!(event.source, Some(agent));
    }

    #[test]
    fn task_spec_display() {
        let spec = SdkTaskSpec::new("build", "Build project");
        let display = spec.to_string();
        assert!(display.starts_with("Task(build:"));
    }

    #[test]
    fn event_display() {
        let event = Event::new("test:event", json!(null));
        let display = event.to_string();
        assert!(display.starts_with("Event(test:event:"));
    }

    #[test]
    fn task_result_serialization_roundtrip() {
        let id = Uuid::now_v7();
        let result = SdkTaskResult::success(id, json!({"x": 1}), Duration::from_secs(2))
            .with_metric("tokens", 42.0);
        let json = serde_json::to_string(&result).unwrap();
        let back: SdkTaskResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task_id, id);
        assert!(back.success);
        assert_eq!(*back.metrics.get("tokens").unwrap(), 42.0);
    }
}
