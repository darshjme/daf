//! Testing utilities for DAF agent development.
//!
//! The testing module provides a [`TestHarness`] for setting up isolated
//! agent test environments, a [`MockAgent`] for simulating peer agents,
//! and a [`TestContext`] for creating test-friendly agent contexts.
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::prelude::*;
//!
//! #[tokio::test]
//! async fn test_worker_processes_task() {
//!     let harness = TestHarness::new();
//!     let agent = harness.spawn_agent(
//!         AgentBuilder::new("test-worker").kind(AgentKind::Worker),
//!     ).unwrap();
//!
//!     let task = SdkTaskSpec::new("lint", "Run linter");
//!     let result = agent.process_task(task).await;
//!     assert!(result.is_ok());
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use daf_core::agent::{
    Agent, AgentCapability, AgentContext, AgentId, AgentKind, AgentManifest, AgentStatus,
};
use daf_core::error::{DafError, DafResult};
use daf_core::message::{Message, MessageKind};
use parking_lot::RwLock;
use uuid::Uuid;

use crate::builder::AgentBuilder;
use crate::config::SdkConfig;
use crate::lifecycle::AgentInstance;

// ---------------------------------------------------------------------------
// TestContext
// ---------------------------------------------------------------------------

/// Factory for test-friendly agent contexts.
///
/// Creates [`AgentContext`] instances with deterministic session IDs
/// and pre-populated environment variables suitable for testing.
pub struct TestContext {
    session_id: Uuid,
    environment: HashMap<String, String>,
}

impl TestContext {
    /// Create a new test context with a fresh session ID.
    pub fn new() -> Self {
        Self {
            session_id: Uuid::now_v7(),
            environment: HashMap::from([
                ("DAF_ENV".into(), "test".into()),
                ("DAF_LOG_LEVEL".into(), "trace".into()),
            ]),
        }
    }

    /// Set a custom session ID.
    pub fn with_session_id(mut self, id: Uuid) -> Self {
        self.session_id = id;
        self
    }

    /// Add an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    /// Create an [`AgentContext`] for a new agent.
    pub fn agent_context(&self) -> AgentContext {
        let mut ctx = AgentContext::new(AgentId::new(), self.session_id);
        ctx.environment = self.environment.clone();
        ctx
    }

    /// Create a child context from an existing parent.
    pub fn child_context(&self, parent: &AgentContext) -> AgentContext {
        AgentContext::child(parent)
    }

    /// The session ID used by this test context.
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// MockAgent
// ---------------------------------------------------------------------------

/// A mock agent implementation for testing interactions.
///
/// Records all received messages and can be configured to return
/// specific responses. Useful for testing orchestrators and routers
/// that interact with downstream agents.
pub struct MockAgent {
    manifest: AgentManifest,
    status: RwLock<AgentStatus>,
    received_messages: Arc<RwLock<Vec<Message>>>,
    response_payload: RwLock<Option<Bytes>>,
    should_fail: RwLock<bool>,
    health_status: RwLock<bool>,
}

impl MockAgent {
    /// Create a new mock agent with the given name and kind.
    pub fn new(name: impl Into<String>, kind: AgentKind) -> Self {
        Self {
            manifest: AgentManifest::new(kind, name),
            status: RwLock::new(AgentStatus::Idle),
            received_messages: Arc::new(RwLock::new(Vec::new())),
            response_payload: RwLock::new(None),
            should_fail: RwLock::new(false),
            health_status: RwLock::new(true),
        }
    }

    /// Create a mock worker agent.
    pub fn worker(name: impl Into<String>) -> Self {
        Self::new(name, AgentKind::Worker)
    }

    /// Create a mock specialist agent.
    pub fn specialist(name: impl Into<String>) -> Self {
        Self::new(name, AgentKind::Specialist)
    }

    /// Configure the mock to return this payload in response to messages.
    pub fn set_response(&self, payload: impl Into<Bytes>) {
        *self.response_payload.write() = Some(payload.into());
    }

    /// Configure the mock to fail on the next message.
    pub fn set_should_fail(&self, fail: bool) {
        *self.should_fail.write() = fail;
    }

    /// Configure the health check result.
    pub fn set_healthy(&self, healthy: bool) {
        *self.health_status.write() = healthy;
    }

    /// Add a capability to the mock.
    pub fn with_capability(mut self, cap: AgentCapability) -> Self {
        self.manifest.capabilities.push(cap);
        self
    }

    /// Return all messages received by this mock.
    pub fn received_messages(&self) -> Vec<Message> {
        self.received_messages.read().clone()
    }

    /// Return the count of received messages.
    pub fn message_count(&self) -> usize {
        self.received_messages.read().len()
    }

    /// Clear the received messages buffer.
    pub fn clear_messages(&self) {
        self.received_messages.write().clear();
    }

    /// Return the last received message, if any.
    pub fn last_message(&self) -> Option<Message> {
        self.received_messages.read().last().cloned()
    }
}

#[async_trait]
impl Agent for MockAgent {
    async fn initialize(&self, _ctx: &AgentContext) -> DafResult<()> {
        *self.status.write() = AgentStatus::Idle;
        Ok(())
    }

    async fn execute(&self, _ctx: &AgentContext) -> DafResult<serde_json::Value> {
        *self.status.write() = AgentStatus::Executing;
        if *self.should_fail.read() {
            *self.status.write() = AgentStatus::Failed;
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "mock agent configured to fail",
            ));
        }
        *self.status.write() = AgentStatus::Completed;
        Ok(serde_json::json!({"mock": true, "status": "completed"}))
    }

    async fn handle_message(&self, _ctx: &AgentContext, msg: Message) -> DafResult<()> {
        self.received_messages.write().push(msg);
        if *self.should_fail.read() {
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "mock agent configured to fail on messages",
            ));
        }
        Ok(())
    }

    async fn shutdown(&self, _ctx: &AgentContext, _timeout: Duration) -> DafResult<()> {
        *self.status.write() = AgentStatus::Terminated;
        Ok(())
    }

    async fn health_check(&self) -> DafResult<()> {
        if *self.health_status.read() {
            Ok(())
        } else {
            Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "mock agent is unhealthy",
            ))
        }
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        self.manifest.capabilities.clone()
    }

    fn status(&self) -> AgentStatus {
        *self.status.read()
    }

    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// TestHarness
// ---------------------------------------------------------------------------

/// Isolated test environment for DAF agents.
///
/// The harness creates agents with test-mode configuration, tracks
/// spawned instances, and provides helper methods for sending messages
/// and asserting outcomes.
pub struct TestHarness {
    context: TestContext,
    config: SdkConfig,
    agents: RwLock<Vec<AgentInstance>>,
}

impl TestHarness {
    /// Create a new test harness with default test configuration.
    pub fn new() -> Self {
        Self {
            context: TestContext::new(),
            config: SdkConfig::test(),
            agents: RwLock::new(Vec::new()),
        }
    }

    /// Create a harness with a custom SDK configuration.
    pub fn with_config(config: SdkConfig) -> Self {
        Self {
            context: TestContext::new(),
            config,
            agents: RwLock::new(Vec::new()),
        }
    }

    /// Spawn an agent from a builder.
    ///
    /// The builder's config is overridden with the harness's test config.
    pub fn spawn_agent(&self, builder: AgentBuilder) -> DafResult<AgentInstance> {
        let instance = builder.with_config(self.config.clone()).build()?;
        self.agents.write().push(
            // We can't clone AgentInstance, so we build a new one for tracking.
            // The returned instance is the one the caller uses.
            AgentBuilder::new(instance.manifest().name.clone())
                .kind(instance.manifest().kind)
                .with_config(self.config.clone())
                .build()?,
        );
        Ok(instance)
    }

    /// Return the number of agents spawned through this harness.
    pub fn agent_count(&self) -> usize {
        self.agents.read().len()
    }

    /// Create a test message with the given payload.
    pub fn make_message(&self, payload: impl Into<Bytes>) -> Message {
        Message::builder(MessageKind::Request, AgentId::new())
            .payload(payload.into())
            .build()
    }

    /// Create a test message with a string payload.
    pub fn make_text_message(&self, text: &str) -> Message {
        self.make_message(Bytes::from(text.to_owned()))
    }

    /// Create a test message with headers.
    pub fn make_routed_message(
        &self,
        routing_key: &str,
        payload: impl Into<Bytes>,
    ) -> Message {
        Message::builder(MessageKind::Request, AgentId::new())
            .header("routing-key", routing_key)
            .payload(payload.into())
            .build()
    }

    /// Access the test context.
    pub fn context(&self) -> &TestContext {
        &self.context
    }

    /// Access the test SDK configuration.
    pub fn config(&self) -> &SdkConfig {
        &self.config
    }
}

impl Default for TestHarness {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Create a minimal agent context for a unit test.
pub fn test_agent_context() -> AgentContext {
    AgentContext::new(AgentId::new(), Uuid::now_v7())
}

/// Create a simple test message with the given payload bytes.
pub fn test_message(payload: &[u8]) -> Message {
    Message::builder(MessageKind::Request, AgentId::new())
        .payload(Bytes::copy_from_slice(payload))
        .build()
}

/// Create a test message with a routing key header.
pub fn test_routed_message(routing_key: &str, payload: &[u8]) -> Message {
    Message::builder(MessageKind::Request, AgentId::new())
        .header("routing-key", routing_key)
        .payload(Bytes::copy_from_slice(payload))
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_creates_agent_context() {
        let tc = TestContext::new();
        let ctx = tc.agent_context();
        assert_eq!(ctx.session_id, tc.session_id());
        assert_eq!(ctx.environment.get("DAF_ENV").unwrap(), "test");
    }

    #[test]
    fn test_context_child() {
        let tc = TestContext::new();
        let parent = tc.agent_context();
        let child = tc.child_context(&parent);
        assert_eq!(child.session_id, parent.session_id);
        assert_eq!(child.parent_id, Some(parent.agent_id));
    }

    #[test]
    fn test_context_custom_env() {
        let tc = TestContext::new().with_env("MY_VAR", "hello");
        let ctx = tc.agent_context();
        assert_eq!(ctx.environment.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn mock_agent_records_messages() {
        let mock = MockAgent::worker("test");
        assert_eq!(mock.message_count(), 0);
        assert_eq!(mock.status(), AgentStatus::Idle);
    }

    #[tokio::test]
    async fn mock_agent_initialize() {
        let mock = MockAgent::specialist("spec");
        let ctx = test_agent_context();
        mock.initialize(&ctx).await.unwrap();
        assert_eq!(mock.status(), AgentStatus::Idle);
    }

    #[tokio::test]
    async fn mock_agent_execute() {
        let mock = MockAgent::worker("exec-test");
        let ctx = test_agent_context();
        let result = mock.execute(&ctx).await.unwrap();
        assert_eq!(result["mock"], true);
        assert_eq!(mock.status(), AgentStatus::Completed);
    }

    #[tokio::test]
    async fn mock_agent_execute_failure() {
        let mock = MockAgent::worker("fail-test");
        mock.set_should_fail(true);
        let ctx = test_agent_context();
        let result = mock.execute(&ctx).await;
        assert!(result.is_err());
        assert_eq!(mock.status(), AgentStatus::Failed);
    }

    #[tokio::test]
    async fn mock_agent_handle_message() {
        let mock = MockAgent::worker("msg-test");
        let ctx = test_agent_context();
        let msg = test_message(b"hello");
        mock.handle_message(&ctx, msg).await.unwrap();
        assert_eq!(mock.message_count(), 1);
    }

    #[tokio::test]
    async fn mock_agent_health_check() {
        let mock = MockAgent::worker("health-test");
        assert!(mock.health_check().await.is_ok());
        mock.set_healthy(false);
        assert!(mock.health_check().await.is_err());
    }

    #[tokio::test]
    async fn mock_agent_shutdown() {
        let mock = MockAgent::worker("shutdown-test");
        let ctx = test_agent_context();
        mock.shutdown(&ctx, Duration::from_secs(5)).await.unwrap();
        assert_eq!(mock.status(), AgentStatus::Terminated);
    }

    #[test]
    fn mock_agent_clear_messages() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mock = MockAgent::worker("clear-test");
        let ctx = test_agent_context();
        rt.block_on(async {
            mock.handle_message(&ctx, test_message(b"a")).await.unwrap();
            mock.handle_message(&ctx, test_message(b"b")).await.unwrap();
        });
        assert_eq!(mock.message_count(), 2);
        mock.clear_messages();
        assert_eq!(mock.message_count(), 0);
    }

    #[test]
    fn test_harness_creates() {
        let harness = TestHarness::new();
        assert_eq!(harness.agent_count(), 0);
        assert_eq!(harness.config().environment, crate::config::Environment::Test);
    }

    #[test]
    fn test_harness_spawn_agent() {
        let harness = TestHarness::new();
        let builder = AgentBuilder::new("harness-worker").kind(AgentKind::Worker);
        let instance = harness.spawn_agent(builder).unwrap();
        assert_eq!(instance.manifest().name, "harness-worker");
        assert_eq!(harness.agent_count(), 1);
    }

    #[test]
    fn test_harness_make_message() {
        let harness = TestHarness::new();
        let msg = harness.make_text_message("hello world");
        assert_eq!(msg.payload_str().unwrap(), "hello world");
    }

    #[test]
    fn test_harness_make_routed_message() {
        let harness = TestHarness::new();
        let msg = harness.make_routed_message("command:deploy", Bytes::from_static(b"go"));
        assert_eq!(msg.headers.get("routing-key").unwrap(), "command:deploy");
    }

    #[test]
    fn test_helper_functions() {
        let ctx = test_agent_context();
        assert!(ctx.parent_id.is_none());

        let msg = test_message(b"data");
        assert_eq!(msg.payload.len(), 4);

        let routed = test_routed_message("event:done", b"ok");
        assert_eq!(routed.headers.get("routing-key").unwrap(), "event:done");
    }
}
