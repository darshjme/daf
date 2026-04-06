//! Fluent agent builder.
//!
//! The [`AgentBuilder`] is the primary entry point for creating DAF agents.
//! It provides a chainable API that collects agent metadata, handlers, and
//! configuration, then produces an [`AgentInstance`](crate::lifecycle::AgentInstance)
//! ready to start.
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::prelude::*;
//!
//! let agent = AgentBuilder::new("security-auditor")
//!     .kind(AgentKind::Specialist)
//!     .capability("code_review", "1.0.0", "Reviews code for vulnerabilities")
//!     .capability("security_scan", "2.0.0", "Runs SAST/DAST scans")
//!     .on_message("command:*", my_command_handler)
//!     .on_task("code_review", my_review_handler)
//!     .with_config(SdkConfig::development())
//!     .build()?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use daf_core::agent::{AgentCapability, AgentContext, AgentKind, AgentManifest, ResourceLimits};
use daf_core::error::{DafError, DafResult};
use uuid::Uuid;

use crate::config::SdkConfig;
use crate::handler::{HandlerRegistry, MessageHandler, TaskHandler, EventHandler};
use crate::lifecycle::AgentInstance;
use crate::middleware::Middleware;

// ---------------------------------------------------------------------------
// AgentBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for constructing DAF agents.
///
/// Required fields:
/// - `name` — set via [`AgentBuilder::new`]
///
/// Optional fields (with sensible defaults):
/// - `kind` — defaults to [`AgentKind::Worker`]
/// - `capabilities` — empty by default
/// - `resource_limits` — default limits from [`ResourceLimits::default`]
/// - `metadata` — empty
/// - `config` — development defaults
/// - `handlers` — empty registry
pub struct AgentBuilder {
    name: String,
    kind: AgentKind,
    capabilities: Vec<AgentCapability>,
    resource_limits: ResourceLimits,
    metadata: HashMap<String, String>,
    config: SdkConfig,
    handler_registry: HandlerRegistry,
    middleware: Vec<Arc<dyn Middleware>>,
    heartbeat_interval: Option<Duration>,
    description: Option<String>,
}

impl AgentBuilder {
    /// Start building an agent with the given name.
    ///
    /// The name should be a kebab-case identifier like `"code-reviewer"` or
    /// `"deploy-worker"`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: AgentKind::Worker,
            capabilities: Vec::new(),
            resource_limits: ResourceLimits::default(),
            metadata: HashMap::new(),
            config: SdkConfig::development(),
            handler_registry: HandlerRegistry::new(),
            middleware: Vec::new(),
            heartbeat_interval: None,
            description: None,
        }
    }

    /// Set the agent kind.
    pub fn kind(mut self, kind: AgentKind) -> Self {
        self.kind = kind;
        self
    }

    /// Add a capability.
    pub fn capability(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.capabilities.push(AgentCapability::new(
            name.into(),
            version.into(),
            description.into(),
        ));
        self
    }

    /// Add a pre-built capability descriptor.
    pub fn with_capability(mut self, cap: AgentCapability) -> Self {
        self.capabilities.push(cap);
        self
    }

    /// Set resource limits.
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_limits = limits;
        self
    }

    /// Add a metadata key-value pair.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Set a human-readable description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the SDK configuration.
    pub fn with_config(mut self, config: SdkConfig) -> Self {
        self.config = config;
        self
    }

    /// Register a message handler for the given pattern.
    ///
    /// Patterns support exact match and `*` wildcard suffix:
    /// - `"command:deploy"` — matches exactly
    /// - `"event:*"` — matches any `event:` prefix
    /// - `"*"` — catch-all
    pub fn on_message(
        mut self,
        pattern: impl Into<String>,
        handler: impl MessageHandler,
    ) -> Self {
        self.handler_registry
            .register_message_handler(pattern, handler);
        self
    }

    /// Register a task handler for the given capability.
    pub fn on_task(
        mut self,
        capability: impl Into<String>,
        handler: impl TaskHandler,
    ) -> Self {
        self.handler_registry
            .register_task_handler(capability, handler);
        self
    }

    /// Register an event handler for the given event type pattern.
    pub fn on_event(
        mut self,
        pattern: impl Into<String>,
        handler: impl EventHandler,
    ) -> Self {
        self.handler_registry
            .register_event_handler(pattern, handler);
        self
    }

    /// Set the default message handler for unmatched messages.
    pub fn default_message_handler(mut self, handler: impl MessageHandler) -> Self {
        self.handler_registry
            .set_default_message_handler(handler);
        self
    }

    /// Set the default task handler for unmatched capabilities.
    pub fn default_task_handler(mut self, handler: impl TaskHandler) -> Self {
        self.handler_registry
            .set_default_task_handler(handler);
        self
    }

    /// Add a middleware layer.
    pub fn with_middleware(mut self, middleware: impl Middleware) -> Self {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Override the heartbeat interval.
    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = Some(interval);
        self
    }

    /// Replace the entire handler registry.
    pub fn with_handler_registry(mut self, registry: HandlerRegistry) -> Self {
        self.handler_registry = registry;
        self
    }

    /// Validate the builder state and produce an [`AgentInstance`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The agent name is empty.
    /// - The SDK configuration fails validation.
    pub fn build(self) -> DafResult<AgentInstance> {
        // Validate required fields.
        if self.name.is_empty() {
            return Err(DafError::ConfigError(
                "agent name must not be empty".into(),
            ));
        }

        // Validate configuration.
        self.config.validate()?;

        // Build the manifest.
        let mut manifest = AgentManifest::new(self.kind, &self.name);
        for cap in &self.capabilities {
            manifest.capabilities.push(cap.clone());
        }
        manifest.resource_limits = self.resource_limits;
        manifest.metadata = self.metadata;

        if let Some(desc) = &self.description {
            manifest
                .metadata
                .insert("description".into(), desc.clone());
        }

        // Build the agent context.
        let ctx = AgentContext::new(manifest.id, Uuid::now_v7());

        // Determine heartbeat interval.
        let heartbeat = self
            .heartbeat_interval
            .unwrap_or(self.config.heartbeat_interval);

        Ok(AgentInstance::new(
            manifest,
            ctx,
            self.config,
            self.handler_registry,
            self.middleware,
            heartbeat,
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_minimal() {
        let instance = AgentBuilder::new("test-worker").build().unwrap();
        assert_eq!(instance.manifest().name, "test-worker");
        assert_eq!(instance.manifest().kind, AgentKind::Worker);
        assert!(instance.manifest().capabilities.is_empty());
    }

    #[test]
    fn builder_with_kind() {
        let instance = AgentBuilder::new("router")
            .kind(AgentKind::Router)
            .build()
            .unwrap();
        assert_eq!(instance.manifest().kind, AgentKind::Router);
    }

    #[test]
    fn builder_with_capabilities() {
        let instance = AgentBuilder::new("specialist")
            .kind(AgentKind::Specialist)
            .capability("code_review", "1.0.0", "Reviews code")
            .capability("lint", "2.0.0", "Runs linter")
            .build()
            .unwrap();

        assert_eq!(instance.manifest().capabilities.len(), 2);
        assert!(instance.manifest().has_capability("code_review"));
        assert!(instance.manifest().has_capability("lint"));
    }

    #[test]
    fn builder_with_metadata() {
        let instance = AgentBuilder::new("worker")
            .metadata("team", "platform")
            .metadata("version", "1.0.0")
            .description("A test worker agent")
            .build()
            .unwrap();

        assert_eq!(
            instance.manifest().metadata.get("team").unwrap(),
            "platform"
        );
        assert_eq!(
            instance.manifest().metadata.get("description").unwrap(),
            "A test worker agent"
        );
    }

    #[test]
    fn builder_with_config() {
        let config = SdkConfig::test();
        let instance = AgentBuilder::new("test")
            .with_config(config)
            .build()
            .unwrap();

        // Just verify it doesn't fail — config is test mode.
        assert_eq!(instance.manifest().name, "test");
    }

    #[test]
    fn builder_rejects_empty_name() {
        let result = AgentBuilder::new("").build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));
    }

    #[test]
    fn builder_rejects_invalid_config() {
        let mut config = SdkConfig::development();
        config.max_concurrent_tasks = 0; // invalid

        let result = AgentBuilder::new("test")
            .with_config(config)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_custom_heartbeat() {
        let instance = AgentBuilder::new("test")
            .heartbeat_interval(Duration::from_secs(5))
            .build()
            .unwrap();

        assert_eq!(instance.heartbeat_interval(), Duration::from_secs(5));
    }

    #[test]
    fn builder_with_resource_limits() {
        let limits = ResourceLimits {
            max_memory_bytes: Some(1024 * 1024 * 1024),
            max_cpu_ms: Some(600_000),
            max_connections: Some(128),
            max_message_queue: Some(2048),
        };

        let instance = AgentBuilder::new("heavy-worker")
            .with_resource_limits(limits.clone())
            .build()
            .unwrap();

        assert_eq!(
            instance.manifest().resource_limits.max_memory_bytes,
            Some(1024 * 1024 * 1024)
        );
    }
}
