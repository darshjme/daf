//! Integration test utilities for the DAF framework.
//!
//! Provides helpers to spin up minimal runtimes, create test agents,
//! establish channels, and build task graphs for integration testing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use tokio::sync::Mutex;
use uuid::Uuid;

use daf_core::agent::{
    Agent, AgentCapability, AgentContext, AgentId, AgentKind, AgentManifest, AgentStatus,
};
use daf_core::error::{DafError, DafResult};
use daf_core::message::{Message, MessageKind, Priority};

// ---------------------------------------------------------------------------
// Test tracing initialization
// ---------------------------------------------------------------------------

/// Initialize tracing for tests (call once per test binary; safe to call
/// multiple times — subsequent calls are no-ops).
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("daf=debug,integration=debug")
        .with_test_writer()
        .try_init();
}

// ---------------------------------------------------------------------------
// Test agent
// ---------------------------------------------------------------------------

/// A minimal agent implementation for integration tests.
///
/// Records all received messages and tracks lifecycle transitions so tests
/// can assert on behavior after the fact.
#[derive(Debug)]
pub struct TestAgent {
    manifest: AgentManifest,
    status: Mutex<AgentStatus>,
    received_messages: Mutex<Vec<Message>>,
    execute_result: Mutex<Option<serde_json::Value>>,
    call_count: AtomicUsize,
}

impl TestAgent {
    /// Create a test agent with the given name and kind.
    pub fn new(name: &str, kind: AgentKind) -> Self {
        Self {
            manifest: AgentManifest::new(kind, name),
            status: Mutex::new(AgentStatus::Idle),
            received_messages: Mutex::new(Vec::new()),
            execute_result: Mutex::new(Some(serde_json::json!({"status": "ok"}))),
            call_count: AtomicUsize::new(0),
        }
    }

    /// Create a worker agent with a default name.
    pub fn worker() -> Self {
        Self::new("test-worker", AgentKind::Worker)
    }

    /// Create a specialist agent.
    pub fn specialist(name: &str) -> Self {
        let mut agent = Self::new(name, AgentKind::Specialist);
        agent.manifest = agent
            .manifest
            .clone()
            .with_capability(AgentCapability::new(name, "1.0.0", "Test capability"));
        agent
    }

    /// Create an orchestrator agent.
    pub fn orchestrator() -> Self {
        Self::new("test-orchestrator", AgentKind::Orchestrator)
    }

    /// Set the value that `execute` will return.
    pub async fn set_execute_result(&self, result: serde_json::Value) {
        *self.execute_result.lock().await = Some(result);
    }

    /// Return all messages received by this agent.
    pub async fn received_messages(&self) -> Vec<Message> {
        self.received_messages.lock().await.clone()
    }

    /// Return how many times `handle_message` was called.
    pub fn message_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Agent for TestAgent {
    async fn initialize(&self, _ctx: &AgentContext) -> DafResult<()> {
        *self.status.lock().await = AgentStatus::Idle;
        Ok(())
    }

    async fn execute(&self, _ctx: &AgentContext) -> DafResult<serde_json::Value> {
        *self.status.lock().await = AgentStatus::Executing;
        let result = self
            .execute_result
            .lock()
            .await
            .clone()
            .unwrap_or(serde_json::json!({"status": "done"}));
        *self.status.lock().await = AgentStatus::Completed;
        Ok(result)
    }

    async fn handle_message(&self, _ctx: &AgentContext, msg: Message) -> DafResult<()> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        self.received_messages.lock().await.push(msg);
        Ok(())
    }

    async fn shutdown(&self, _ctx: &AgentContext, _timeout: Duration) -> DafResult<()> {
        *self.status.lock().await = AgentStatus::Terminated;
        Ok(())
    }

    async fn health_check(&self) -> DafResult<()> {
        Ok(())
    }

    fn capabilities(&self) -> Vec<AgentCapability> {
        self.manifest.capabilities.clone()
    }

    fn status(&self) -> AgentStatus {
        // For tests, return Idle as a safe default without async lock.
        AgentStatus::Idle
    }

    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// Test message helpers
// ---------------------------------------------------------------------------

/// Build a simple request message from source to target.
pub fn request_message(source: AgentId, target: AgentId, payload: &str) -> Message {
    Message::builder(MessageKind::Request, source)
        .target(target)
        .payload(Bytes::from(payload.to_owned()))
        .header("content-type", "text/plain")
        .build()
}

/// Build a command message.
pub fn command_message(source: AgentId, target: AgentId, payload: serde_json::Value) -> Message {
    Message::builder(MessageKind::Command, source)
        .target(target)
        .payload_json(&payload)
        .expect("json serialize")
        .build()
}

/// Build an event message (broadcast, no target).
pub fn event_message(source: AgentId, payload: &str) -> Message {
    Message::builder(MessageKind::Event, source)
        .payload(Bytes::from(payload.to_owned()))
        .build()
}

/// Build a heartbeat message.
pub fn heartbeat_message(source: AgentId) -> Message {
    Message::builder(MessageKind::Heartbeat, source)
        .ttl(Duration::from_secs(5))
        .build()
}

// ---------------------------------------------------------------------------
// Test context helpers
// ---------------------------------------------------------------------------

/// Create a fresh agent context for testing.
pub fn test_context() -> AgentContext {
    AgentContext::new(AgentId::new(), Uuid::now_v7())
}

/// Create a child context from an existing parent.
pub fn child_context(parent: &AgentContext) -> AgentContext {
    AgentContext::child(parent)
}

// ---------------------------------------------------------------------------
// Test channel pair
// ---------------------------------------------------------------------------

/// A bidirectional channel pair for testing inter-agent communication.
pub struct TestChannel {
    pub tx: tokio::sync::mpsc::Sender<Message>,
    pub rx: Mutex<tokio::sync::mpsc::Receiver<Message>>,
}

impl TestChannel {
    /// Create a pair of connected channels (A <-> B).
    pub fn pair(buffer: usize) -> (Self, Self) {
        let (tx_a, rx_b) = tokio::sync::mpsc::channel(buffer);
        let (tx_b, rx_a) = tokio::sync::mpsc::channel(buffer);

        let a = TestChannel {
            tx: tx_a,
            rx: Mutex::new(rx_a),
        };
        let b = TestChannel {
            tx: tx_b,
            rx: Mutex::new(rx_b),
        };
        (a, b)
    }

    /// Send a message through this channel.
    pub async fn send(&self, msg: Message) -> DafResult<()> {
        self.tx
            .send(msg)
            .await
            .map_err(|_| DafError::transport("channel closed", false))
    }

    /// Receive a message with a timeout.
    pub async fn recv(&self, timeout: Duration) -> DafResult<Message> {
        let mut rx = self.rx.lock().await;
        tokio::time::timeout(timeout, rx.recv())
            .await
            .map_err(|_| DafError::TimeoutError {
                operation: "channel recv".into(),
                duration: timeout,
            })?
            .ok_or_else(|| DafError::transport("channel closed", false))
    }
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/// Assert that a message has the expected kind and payload content.
pub fn assert_message_content(msg: &Message, expected_kind: MessageKind, expected_payload: &str) {
    assert_eq!(msg.kind, expected_kind, "unexpected message kind");
    let actual = msg.payload_str().expect("payload should be valid UTF-8");
    assert_eq!(actual, expected_payload, "payload mismatch");
}

/// Assert that a set of messages arrived in order by timestamp.
pub fn assert_messages_ordered(messages: &[Message]) {
    for window in messages.windows(2) {
        assert!(
            window[0].timestamp <= window[1].timestamp,
            "messages out of order: {:?} > {:?}",
            window[0].timestamp,
            window[1].timestamp
        );
    }
}
