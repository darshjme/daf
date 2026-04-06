//! Basic DAF agent example — shows how to create a minimal agent
//! that receives tasks, processes them, and reports results.
//!
//! This is the simplest possible DAF agent: an "echo" agent that receives
//! messages and echoes them back to the sender. It demonstrates:
//!
//! - Implementing the [`Agent`] trait
//! - Building an [`AgentManifest`] with capabilities
//! - Handling inbound messages
//! - Graceful shutdown with timeout
//! - Status tracking through the agent lifecycle
//!
//! Run with:
//!   cargo run -p daf-example-basic-agent

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Mutex;
use tracing::{info, warn};

use daf_core::agent::{
    Agent, AgentCapability, AgentContext, AgentId, AgentKind, AgentManifest, AgentStatus,
    ResourceLimits,
};
use daf_core::error::{DafError, DafResult};
use daf_core::message::{Message, MessageKind, Priority};

// ---------------------------------------------------------------------------
// EchoAgent — the simplest possible DAF agent
// ---------------------------------------------------------------------------

/// An echo agent that mirrors every inbound request back to its sender.
///
/// This struct holds all mutable state behind a `Mutex` so we can satisfy
/// the `Agent` trait's `&self` requirement while still mutating internals.
struct EchoAgent {
    /// The declarative manifest describing this agent's identity and capabilities.
    manifest: AgentManifest,

    /// Current lifecycle status. Wrapped in a Mutex because `handle_message`
    /// and `execute` may run concurrently.
    status: Arc<Mutex<AgentStatus>>,

    /// Running count of messages processed (for demonstration purposes).
    messages_processed: Arc<Mutex<u64>>,
}

impl EchoAgent {
    /// Create a new EchoAgent with a fully-populated manifest.
    fn new() -> Self {
        // Build the manifest that describes what this agent is and can do.
        // The orchestrator uses this to decide which tasks to route here.
        let manifest = AgentManifest::new(AgentKind::Worker, "echo-agent")
            .with_capability(AgentCapability::new(
                "echo",
                "1.0.0",
                "Echoes inbound messages back to the sender",
            ))
            .with_capability(AgentCapability::new(
                "ping",
                "1.0.0",
                "Responds to health pings with a pong",
            ))
            .with_resource_limits(ResourceLimits {
                max_memory_bytes: Some(64 * 1024 * 1024), // 64 MiB — echo is lightweight
                max_cpu_ms: Some(60_000),                  // 1 minute max
                max_connections: Some(16),
                max_message_queue: Some(256),
            })
            .with_metadata("team", "examples")
            .with_metadata("version", env!("CARGO_PKG_VERSION"));

        Self {
            manifest,
            status: Arc::new(Mutex::new(AgentStatus::Spawning)),
            messages_processed: Arc::new(Mutex::new(0)),
        }
    }
}

#[async_trait]
impl Agent for EchoAgent {
    /// Called once after spawn, before any messages arrive.
    ///
    /// This is where you would open database connections, load ML models,
    /// warm caches, or do any other expensive one-time setup. For the echo
    /// agent we just log and transition to Idle.
    async fn initialize(&self, ctx: &AgentContext) -> DafResult<()> {
        info!(
            agent_id = %ctx.agent_id,
            session = %ctx.session_id,
            "Echo agent initializing"
        );

        // Simulate some startup work (loading config, etc.)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Transition from Spawning -> Idle
        let mut status = self.status.lock().await;
        *status = AgentStatus::Idle;

        info!("Echo agent ready — waiting for messages");
        Ok(())
    }

    /// The main work loop. For a long-running agent this would be a loop
    /// that pulls tasks from a queue. Here we just demonstrate the pattern.
    async fn execute(&self, ctx: &AgentContext) -> DafResult<serde_json::Value> {
        info!(agent_id = %ctx.agent_id, "Echo agent executing main loop");

        // Transition to Executing
        {
            let mut status = self.status.lock().await;
            *status = AgentStatus::Executing;
        }

        // In a real agent, this would be a select! loop listening for tasks,
        // shutdown signals, and heartbeat ticks. Here we simulate a workload
        // that runs for a few seconds before completing.
        let mut tick = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            tick += 1;

            let count = *self.messages_processed.lock().await;
            info!(tick, messages_processed = count, "Echo agent heartbeat");

            // For demo purposes, exit after 5 ticks.
            if tick >= 5 {
                break;
            }
        }

        // Transition to Completed
        {
            let mut status = self.status.lock().await;
            *status = AgentStatus::Completed;
        }

        let count = *self.messages_processed.lock().await;
        Ok(serde_json::json!({
            "status": "completed",
            "messages_processed": count,
            "ticks": tick,
        }))
    }

    /// Handle an inbound message from another agent or the runtime.
    ///
    /// This is the core of the echo agent: we read the payload, construct
    /// a response, and would normally send it back via the transport layer.
    /// Since this example runs standalone, we just log the echo.
    async fn handle_message(&self, ctx: &AgentContext, msg: Message) -> DafResult<()> {
        info!(
            agent_id = %ctx.agent_id,
            msg_id = %msg.id,
            kind = %msg.kind,
            source = %msg.source,
            payload_size = msg.payload.len(),
            "Received message"
        );

        // Increment our counter
        {
            let mut count = self.messages_processed.lock().await;
            *count += 1;
        }

        // Read the payload as a string (or fall back to hex representation)
        let payload_text = msg
            .payload_str()
            .unwrap_or("<binary payload>");

        info!(echo = payload_text, "Echoing message back");

        // In production, you would construct a Response message and send it
        // back through the transport layer. Here we just build one to show
        // the pattern:
        if msg.expects_reply() {
            let _response = Message::builder(MessageKind::Response, ctx.agent_id)
                .target(msg.source)
                .correlation_id(msg.id)
                .priority(Priority::Normal)
                .payload(Bytes::copy_from_slice(&msg.payload))
                .header("x-echo", "true")
                .build();

            info!(
                correlation_id = %msg.id,
                "Response constructed (would send via transport in production)"
            );
        }

        Ok(())
    }

    /// Graceful shutdown. Clean up resources, flush buffers, close connections.
    ///
    /// The runtime guarantees it will wait up to `timeout` for this to return.
    /// After that, the agent process is force-killed.
    async fn shutdown(&self, ctx: &AgentContext, timeout: Duration) -> DafResult<()> {
        info!(
            agent_id = %ctx.agent_id,
            timeout_secs = timeout.as_secs(),
            "Echo agent shutting down"
        );

        let count = *self.messages_processed.lock().await;
        info!(total_messages = count, "Final message count");

        // Transition to Terminated
        let mut status = self.status.lock().await;
        *status = AgentStatus::Terminated;

        info!("Echo agent shutdown complete");
        Ok(())
    }

    /// Health probe — called periodically by the runtime.
    ///
    /// Return Ok(()) if healthy, or a DafError describing the degradation.
    /// The runtime uses this to decide whether to restart the agent.
    async fn health_check(&self) -> DafResult<()> {
        let status = self.status.lock().await;
        match *status {
            AgentStatus::Failed => Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "agent is in failed state",
            )),
            AgentStatus::Terminated => Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "agent has been terminated",
            )),
            _ => Ok(()),
        }
    }

    /// Report capabilities — used by the scheduler for task matching.
    fn capabilities(&self) -> Vec<AgentCapability> {
        self.manifest.capabilities.clone()
    }

    /// Report current lifecycle status.
    fn status(&self) -> AgentStatus {
        // We can't await here (sync fn), so we try_lock.
        // In production, use an AtomicU8 or similar for lock-free reads.
        self.status
            .try_lock()
            .map(|s| *s)
            .unwrap_or(AgentStatus::Executing)
    }

    /// Return the manifest.
    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// main — bootstrap and run the agent
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize structured logging. In production this would come from
    // DafConfig, but for examples we use the RUST_LOG env var.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(true)
        .init();

    info!("=== DAF Basic Agent Example ===");

    // 1. Create the agent
    let agent = EchoAgent::new();

    info!(
        name = agent.manifest().name,
        kind = %agent.manifest().kind,
        capabilities = ?agent.capabilities().iter().map(|c| c.to_string()).collect::<Vec<_>>(),
        "Agent created"
    );

    // 2. Build the runtime context. In production the runtime constructs
    //    this; here we build it manually.
    let ctx = AgentContext::new(agent.manifest().id, uuid::Uuid::now_v7())
        .with_env("DAF_ENV", "development")
        .with_work_dir(std::env::current_dir()?);

    info!(
        session = %ctx.session_id,
        agent_id = %ctx.agent_id,
        "Context created"
    );

    // 3. Initialize the agent (one-time setup)
    agent.initialize(&ctx).await?;
    info!("Agent initialized — status: {}", agent.status());

    // 4. Simulate some inbound messages concurrently with execute
    let msg_ctx = ctx.clone();
    let agent_ref = &agent;

    // Spawn a background task that sends messages into the agent
    let msg_handle = tokio::spawn({
        let msg_ctx = msg_ctx.clone();
        // We need to demonstrate message handling, so we create fake messages
        async move {
            // Small delay to let execute() start first
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Create a few sample messages
            let sender = AgentId::new();
            let messages = vec![
                ("Hello, DAF!", MessageKind::Request),
                ("Status check", MessageKind::Command),
                ("Heartbeat ping", MessageKind::Heartbeat),
            ];

            for (payload, kind) in &messages {
                let msg = Message::builder(*kind, sender)
                    .target(msg_ctx.agent_id)
                    .payload(Bytes::from(*payload))
                    .build();

                // We can't call agent methods from a spawned task without Arc,
                // so we just log what we would send.
                info!(
                    payload = *payload,
                    kind = %kind,
                    msg_id = %msg.id,
                    "Would send message to agent"
                );

                tokio::time::sleep(Duration::from_millis(800)).await;
            }
        }
    });

    // 5. Run the main execution loop
    let result = agent.execute(&ctx).await?;
    info!(result = %result, "Agent execution completed");

    // Wait for the message sender to finish
    msg_handle.await?;

    // 6. Run a health check
    match agent.health_check().await {
        Ok(()) => info!("Health check: HEALTHY"),
        Err(e) => warn!("Health check: DEGRADED — {e}"),
    }

    // 7. Graceful shutdown
    agent
        .shutdown(&ctx, Duration::from_secs(5))
        .await?;

    info!(
        final_status = %agent.status(),
        "=== Example complete ==="
    );

    Ok(())
}
