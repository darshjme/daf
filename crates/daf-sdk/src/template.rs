//! Pre-configured agent templates for common patterns.
//!
//! Templates accelerate development by providing ready-made
//! [`AgentBuilder`](crate::builder::AgentBuilder) configurations for the
//! most frequent agent archetypes. Each template sets sensible defaults
//! for kind, capabilities, middleware, and resource limits.
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::prelude::*;
//!
//! // A worker that processes tasks from a queue.
//! let worker = WorkerTemplate::new("image-resizer")
//!     .capability("resize", "1.0.0", "Resize images")
//!     .on_task("resize", resize_handler)
//!     .build()?;
//!
//! // A router that dispatches messages by content.
//! let router = RouterTemplate::new("api-gateway")
//!     .route("command:deploy", deploy_handler)
//!     .route("command:rollback", rollback_handler)
//!     .build()?;
//! ```

use std::time::Duration;

use daf_core::agent::{AgentKind, ResourceLimits};
use daf_core::error::DafResult;

use crate::builder::AgentBuilder;
use crate::config::SdkConfig;
use crate::handler::{EventHandler, MessageHandler, TaskHandler};
use crate::lifecycle::AgentInstance;
use crate::middleware::{LoggingMiddleware, MetricsMiddleware, TimeoutMiddleware};

// ---------------------------------------------------------------------------
// WorkerTemplate
// ---------------------------------------------------------------------------

/// Template for a general-purpose worker agent.
///
/// Workers pull tasks from a queue and execute them. This template
/// pre-configures:
/// - Kind: [`AgentKind::Worker`]
/// - Middleware: logging + metrics
/// - Heartbeat: 30s
pub struct WorkerTemplate {
    builder: AgentBuilder,
}

impl WorkerTemplate {
    /// Create a new worker template with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let builder = AgentBuilder::new(name)
            .kind(AgentKind::Worker)
            .with_middleware(LoggingMiddleware::new())
            .with_middleware(MetricsMiddleware::new())
            .heartbeat_interval(Duration::from_secs(30));

        Self { builder }
    }

    /// Add a capability to the worker.
    pub fn capability(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.builder = self.builder.capability(name, version, description);
        self
    }

    /// Register a task handler for a capability.
    pub fn on_task(
        mut self,
        capability: impl Into<String>,
        handler: impl TaskHandler,
    ) -> Self {
        self.builder = self.builder.on_task(capability, handler);
        self
    }

    /// Register a message handler.
    pub fn on_message(
        mut self,
        pattern: impl Into<String>,
        handler: impl MessageHandler,
    ) -> Self {
        self.builder = self.builder.on_message(pattern, handler);
        self
    }

    /// Set the SDK configuration.
    pub fn with_config(mut self, config: SdkConfig) -> Self {
        self.builder = self.builder.with_config(config);
        self
    }

    /// Set resource limits.
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.builder = self.builder.with_resource_limits(limits);
        self
    }

    /// Add metadata.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.builder = self.builder.metadata(key, value);
        self
    }

    /// Build the agent instance.
    pub fn build(self) -> DafResult<AgentInstance> {
        self.builder.build()
    }
}

// ---------------------------------------------------------------------------
// RouterTemplate
// ---------------------------------------------------------------------------

/// Template for a message routing agent.
///
/// Routers inspect incoming messages and forward them to the appropriate
/// handler based on patterns. This template pre-configures:
/// - Kind: [`AgentKind::Router`]
/// - Middleware: logging
/// - Heartbeat: 15s (routers should respond quickly)
pub struct RouterTemplate {
    builder: AgentBuilder,
}

impl RouterTemplate {
    /// Create a new router template with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let builder = AgentBuilder::new(name)
            .kind(AgentKind::Router)
            .with_middleware(LoggingMiddleware::new())
            .heartbeat_interval(Duration::from_secs(15));

        Self { builder }
    }

    /// Register a message route. Messages matching `pattern` will be
    /// dispatched to `handler`.
    pub fn route(
        mut self,
        pattern: impl Into<String>,
        handler: impl MessageHandler,
    ) -> Self {
        self.builder = self.builder.on_message(pattern, handler);
        self
    }

    /// Set a fallback handler for unmatched messages.
    pub fn default_handler(mut self, handler: impl MessageHandler) -> Self {
        self.builder = self.builder.default_message_handler(handler);
        self
    }

    /// Add a capability.
    pub fn capability(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.builder = self.builder.capability(name, version, description);
        self
    }

    /// Set the SDK configuration.
    pub fn with_config(mut self, config: SdkConfig) -> Self {
        self.builder = self.builder.with_config(config);
        self
    }

    /// Build the agent instance.
    pub fn build(self) -> DafResult<AgentInstance> {
        self.builder.build()
    }
}

// ---------------------------------------------------------------------------
// PipelineTemplate
// ---------------------------------------------------------------------------

/// Template for a multi-stage pipeline agent.
///
/// Pipelines process work through a sequence of handlers, each stage
/// feeding into the next. This template pre-configures:
/// - Kind: [`AgentKind::Specialist`]
/// - Middleware: logging + metrics + timeout (5 minutes default)
/// - Heartbeat: 30s
pub struct PipelineTemplate {
    builder: AgentBuilder,
    timeout: Duration,
}

impl PipelineTemplate {
    /// Create a new pipeline template with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let timeout = Duration::from_secs(300);
        let builder = AgentBuilder::new(name)
            .kind(AgentKind::Specialist)
            .with_middleware(LoggingMiddleware::new())
            .with_middleware(MetricsMiddleware::new())
            .with_middleware(TimeoutMiddleware::new(timeout))
            .heartbeat_interval(Duration::from_secs(30));

        Self { builder, timeout }
    }

    /// Set the pipeline timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Add a pipeline stage as a message handler.
    pub fn stage(
        mut self,
        name: impl Into<String>,
        handler: impl MessageHandler,
    ) -> Self {
        self.builder = self.builder.on_message(name, handler);
        self
    }

    /// Add a capability.
    pub fn capability(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.builder = self.builder.capability(name, version, description);
        self
    }

    /// Register a task handler.
    pub fn on_task(
        mut self,
        capability: impl Into<String>,
        handler: impl TaskHandler,
    ) -> Self {
        self.builder = self.builder.on_task(capability, handler);
        self
    }

    /// Set the SDK configuration.
    pub fn with_config(mut self, config: SdkConfig) -> Self {
        self.builder = self.builder.with_config(config);
        self
    }

    /// Build the agent instance.
    pub fn build(self) -> DafResult<AgentInstance> {
        self.builder.build()
    }
}

// ---------------------------------------------------------------------------
// MonitorTemplate
// ---------------------------------------------------------------------------

/// Template for a passive monitoring agent.
///
/// Monitors observe system events and collect metrics without mutating
/// application state. This template pre-configures:
/// - Kind: [`AgentKind::Monitor`]
/// - Middleware: logging
/// - Heartbeat: 10s (monitors need tight health visibility)
/// - Resource limits: reduced (monitors are lightweight)
pub struct MonitorTemplate {
    builder: AgentBuilder,
}

impl MonitorTemplate {
    /// Create a new monitor template with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let limits = ResourceLimits {
            max_memory_bytes: Some(128 * 1024 * 1024), // 128 MiB
            max_cpu_ms: Some(60_000),                   // 1 minute
            max_connections: Some(16),
            max_message_queue: Some(256),
        };

        let builder = AgentBuilder::new(name)
            .kind(AgentKind::Monitor)
            .with_middleware(LoggingMiddleware::new())
            .with_resource_limits(limits)
            .heartbeat_interval(Duration::from_secs(10));

        Self { builder }
    }

    /// Register an event handler for the given event pattern.
    pub fn on_event(
        mut self,
        pattern: impl Into<String>,
        handler: impl EventHandler,
    ) -> Self {
        self.builder = self.builder.on_event(pattern, handler);
        self
    }

    /// Register a message handler (e.g. for control messages).
    pub fn on_message(
        mut self,
        pattern: impl Into<String>,
        handler: impl MessageHandler,
    ) -> Self {
        self.builder = self.builder.on_message(pattern, handler);
        self
    }

    /// Add a capability.
    pub fn capability(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.builder = self.builder.capability(name, version, description);
        self
    }

    /// Set the SDK configuration.
    pub fn with_config(mut self, config: SdkConfig) -> Self {
        self.builder = self.builder.with_config(config);
        self
    }

    /// Add metadata.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.builder = self.builder.metadata(key, value);
        self
    }

    /// Build the agent instance.
    pub fn build(self) -> DafResult<AgentInstance> {
        self.builder.build()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_template_builds() {
        let instance = WorkerTemplate::new("test-worker")
            .capability("process", "1.0.0", "Process items")
            .metadata("team", "platform")
            .build()
            .unwrap();

        assert_eq!(instance.manifest().kind, AgentKind::Worker);
        assert_eq!(instance.manifest().name, "test-worker");
        assert!(instance.manifest().has_capability("process"));
    }

    #[test]
    fn router_template_builds() {
        let instance = RouterTemplate::new("api-router")
            .capability("routing", "1.0.0", "Route messages")
            .build()
            .unwrap();

        assert_eq!(instance.manifest().kind, AgentKind::Router);
        assert_eq!(instance.manifest().name, "api-router");
    }

    #[test]
    fn pipeline_template_builds() {
        let instance = PipelineTemplate::new("etl-pipeline")
            .timeout(Duration::from_secs(600))
            .capability("transform", "1.0.0", "Transform data")
            .build()
            .unwrap();

        assert_eq!(instance.manifest().kind, AgentKind::Specialist);
        assert_eq!(instance.manifest().name, "etl-pipeline");
    }

    #[test]
    fn monitor_template_builds() {
        let instance = MonitorTemplate::new("health-monitor")
            .capability("observe", "1.0.0", "Observe system events")
            .metadata("scope", "cluster")
            .build()
            .unwrap();

        assert_eq!(instance.manifest().kind, AgentKind::Monitor);
        assert_eq!(instance.manifest().name, "health-monitor");
        // Monitor has reduced resource limits.
        assert_eq!(
            instance.manifest().resource_limits.max_memory_bytes,
            Some(128 * 1024 * 1024)
        );
    }

    #[test]
    fn worker_template_with_config() {
        let config = SdkConfig::test();
        let instance = WorkerTemplate::new("cfg-worker")
            .with_config(config)
            .build()
            .unwrap();

        assert_eq!(instance.manifest().name, "cfg-worker");
    }

    #[test]
    fn worker_template_with_resource_limits() {
        let limits = ResourceLimits {
            max_memory_bytes: Some(1024 * 1024 * 1024),
            max_cpu_ms: Some(600_000),
            max_connections: Some(256),
            max_message_queue: Some(4096),
        };

        let instance = WorkerTemplate::new("heavy-worker")
            .with_resource_limits(limits)
            .build()
            .unwrap();

        assert_eq!(
            instance.manifest().resource_limits.max_memory_bytes,
            Some(1024 * 1024 * 1024)
        );
    }
}
