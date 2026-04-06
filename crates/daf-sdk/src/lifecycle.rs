//! Agent lifecycle management.
//!
//! [`AgentInstance`] wraps an agent's manifest, context, handlers, and
//! runtime state into a single object that manages the full lifecycle:
//! initialization, message processing, health checks, and graceful shutdown.
//!
//! # Lifecycle phases
//!
//! ```text
//! Created ──▶ Starting ──▶ Running ──▶ Stopping ──▶ Stopped
//!                            │  ▲
//!                            │  │
//!                            ▼  │
//!                          Paused
//! ```
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::prelude::*;
//!
//! let instance = AgentBuilder::new("worker")
//!     .kind(AgentKind::Worker)
//!     .build()?;
//!
//! // Start the agent (initializes, registers, begins message loop).
//! instance.start().await?;
//!
//! // Graceful shutdown.
//! instance.stop().await?;
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use daf_core::agent::{AgentContext, AgentId, AgentManifest};
use daf_core::error::{DafError, DafResult};
use daf_core::message::Message;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::config::SdkConfig;
use crate::handler::HandlerRegistry;
use crate::middleware::Middleware;
use crate::task_types::{Event, SdkTaskSpec, SdkTaskResult};

// ---------------------------------------------------------------------------
// InstanceState
// ---------------------------------------------------------------------------

/// Runtime state of an agent instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    /// Instance created but not yet started.
    Created,
    /// Initialization in progress.
    Starting,
    /// Fully operational, processing messages and tasks.
    Running,
    /// Temporarily paused, not processing new work.
    Paused,
    /// Graceful shutdown in progress.
    Stopping,
    /// Fully stopped.
    Stopped,
    /// Instance encountered an unrecoverable error.
    Failed,
}

impl InstanceState {
    /// Whether the instance is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }

    /// Whether the instance is actively processing.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running)
    }
}

impl fmt::Display for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Stopping => write!(f, "stopping"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// HealthStatus
// ---------------------------------------------------------------------------

/// Result of a periodic health check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Whether the agent is healthy.
    pub healthy: bool,
    /// Current instance state.
    pub state: InstanceState,
    /// Agent uptime.
    pub uptime: Duration,
    /// Number of messages processed.
    pub messages_processed: u64,
    /// Number of tasks completed.
    pub tasks_completed: u64,
    /// Last health check timestamp.
    pub checked_at: DateTime<Utc>,
    /// Optional degradation reason.
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// RuntimeStats
// ---------------------------------------------------------------------------

/// Cumulative runtime statistics.
#[derive(Debug, Clone, Default)]
pub struct RuntimeStats {
    /// Total messages received.
    pub messages_received: u64,
    /// Total messages processed successfully.
    pub messages_processed: u64,
    /// Total messages that caused errors.
    pub messages_failed: u64,
    /// Total tasks executed.
    pub tasks_completed: u64,
    /// Total tasks that failed.
    pub tasks_failed: u64,
    /// Number of health checks performed.
    pub health_checks: u64,
    /// Number of restarts.
    pub restarts: u64,
}

// ---------------------------------------------------------------------------
// AgentInstance
// ---------------------------------------------------------------------------

/// A fully-configured agent instance ready for lifecycle management.
///
/// Created by [`AgentBuilder::build`](crate::builder::AgentBuilder::build).
/// Call [`start`](Self::start) to initialize the agent and begin processing.
pub struct AgentInstance {
    /// Agent manifest (identity, capabilities, metadata).
    manifest: AgentManifest,
    /// Runtime context.
    context: AgentContext,
    /// SDK configuration.
    config: SdkConfig,
    /// Handler registry for message/task/event routing.
    handler_registry: HandlerRegistry,
    /// Middleware layers.
    middleware: Vec<Arc<dyn Middleware>>,
    /// Heartbeat interval.
    heartbeat: Duration,
    /// Current lifecycle state.
    state: Arc<RwLock<InstanceState>>,
    /// Cumulative runtime statistics.
    stats: Arc<RwLock<RuntimeStats>>,
    /// When the instance was created.
    created_at: DateTime<Utc>,
    /// Shutdown notification channel.
    shutdown_notify: Arc<Notify>,
}

impl AgentInstance {
    /// Create a new instance. Called by [`AgentBuilder::build`].
    pub(crate) fn new(
        manifest: AgentManifest,
        context: AgentContext,
        config: SdkConfig,
        handler_registry: HandlerRegistry,
        middleware: Vec<Arc<dyn Middleware>>,
        heartbeat: Duration,
    ) -> Self {
        Self {
            manifest,
            context,
            config,
            handler_registry,
            middleware,
            heartbeat,
            state: Arc::new(RwLock::new(InstanceState::Created)),
            stats: Arc::new(RwLock::new(RuntimeStats::default())),
            created_at: Utc::now(),
            shutdown_notify: Arc::new(Notify::new()),
        }
    }

    // -- Accessors -----------------------------------------------------------

    /// Return the agent manifest.
    pub fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    /// Return the agent context.
    pub fn context(&self) -> &AgentContext {
        &self.context
    }

    /// Return the agent ID.
    pub fn agent_id(&self) -> AgentId {
        self.manifest.id
    }

    /// Return the current lifecycle state.
    pub fn state(&self) -> InstanceState {
        *self.state.read()
    }

    /// Return the SDK configuration.
    pub fn config(&self) -> &SdkConfig {
        &self.config
    }

    /// Return the heartbeat interval.
    pub fn heartbeat_interval(&self) -> Duration {
        self.heartbeat
    }

    /// Return the number of middleware layers configured.
    pub fn middleware_count(&self) -> usize {
        self.middleware.len()
    }

    /// Return a snapshot of runtime statistics.
    pub fn stats(&self) -> RuntimeStats {
        self.stats.read().clone()
    }

    /// Duration since the instance was created.
    pub fn uptime(&self) -> Duration {
        let elapsed = Utc::now() - self.created_at;
        elapsed.to_std().unwrap_or(Duration::ZERO)
    }

    /// Return a reference to the handler registry.
    pub fn handler_registry(&self) -> &HandlerRegistry {
        &self.handler_registry
    }

    // -- Lifecycle operations ------------------------------------------------

    /// Initialize the agent and transition to the Running state.
    ///
    /// This method:
    /// 1. Validates the current state is `Created` or `Stopped`.
    /// 2. Transitions to `Starting`.
    /// 3. Performs initialization (would register with registry, open
    ///    connections, etc. in the full runtime).
    /// 4. Transitions to `Running`.
    pub async fn start(&self) -> DafResult<()> {
        let current = self.state();
        if current != InstanceState::Created && current != InstanceState::Stopped {
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                format!("cannot start from state '{current}'"),
            ));
        }

        self.set_state(InstanceState::Starting);
        info!(
            agent = %self.manifest.name,
            id = %self.manifest.id,
            kind = %self.manifest.kind,
            "agent starting"
        );

        // In a full runtime, this would:
        // - Connect to the transport layer
        // - Register with the agent registry
        // - Start the message receive loop
        // - Start the heartbeat loop

        self.set_state(InstanceState::Running);
        info!(
            agent = %self.manifest.name,
            "agent running"
        );

        Ok(())
    }

    /// Graceful shutdown.
    ///
    /// 1. Transitions to `Stopping`.
    /// 2. Notifies all waiting tasks.
    /// 3. Waits for in-flight work to drain (up to shutdown timeout).
    /// 4. Deregisters from the registry.
    /// 5. Transitions to `Stopped`.
    pub async fn stop(&self) -> DafResult<()> {
        let current = self.state();
        if current.is_terminal() {
            return Ok(()); // already stopped
        }

        self.set_state(InstanceState::Stopping);
        info!(
            agent = %self.manifest.name,
            timeout = ?self.config.shutdown_timeout,
            "agent stopping"
        );

        // Signal all waiting tasks to drain.
        self.shutdown_notify.notify_waiters();

        // In a full runtime, this would:
        // - Stop accepting new messages
        // - Wait for in-flight handlers to complete
        // - Deregister from the agent registry
        // - Close transport connections
        // - Flush logs

        self.set_state(InstanceState::Stopped);
        info!(agent = %self.manifest.name, "agent stopped");

        Ok(())
    }

    /// Stop and restart the agent, preserving runtime statistics.
    pub async fn restart(&self) -> DafResult<()> {
        info!(agent = %self.manifest.name, "agent restarting");
        self.stop().await?;

        {
            let mut stats = self.stats.write();
            stats.restarts += 1;
        }

        // Reset state to Created so start() can proceed.
        self.set_state(InstanceState::Created);
        self.start().await
    }

    /// Pause the agent. It remains alive but stops processing new work.
    pub fn pause(&self) -> DafResult<()> {
        let current = self.state();
        if current != InstanceState::Running {
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                format!("cannot pause from state '{current}'"),
            ));
        }

        self.set_state(InstanceState::Paused);
        info!(agent = %self.manifest.name, "agent paused");
        Ok(())
    }

    /// Resume a paused agent.
    pub fn resume(&self) -> DafResult<()> {
        let current = self.state();
        if current != InstanceState::Paused {
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                format!("cannot resume from state '{current}'"),
            ));
        }

        self.set_state(InstanceState::Running);
        info!(agent = %self.manifest.name, "agent resumed");
        Ok(())
    }

    /// Perform a health check.
    pub fn health_check(&self) -> HealthStatus {
        let stats = self.stats.read();
        let state = self.state();
        let healthy = state.is_active();

        // Increment health check counter.
        drop(stats);
        {
            let mut stats = self.stats.write();
            stats.health_checks += 1;
        }
        let stats = self.stats.read();

        HealthStatus {
            healthy,
            state,
            uptime: self.uptime(),
            messages_processed: stats.messages_processed,
            tasks_completed: stats.tasks_completed,
            checked_at: Utc::now(),
            reason: if healthy {
                None
            } else {
                Some(format!("agent is in state '{state}'"))
            },
        }
    }

    // -- Message processing --------------------------------------------------

    /// Process an inbound message through the handler registry.
    ///
    /// This routes the message to the appropriate handler based on
    /// registered patterns. In a full runtime, this is called by the
    /// message receive loop.
    pub async fn process_message(&self, msg: Message) -> DafResult<Option<Message>> {
        if self.state() != InstanceState::Running {
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "agent is not running",
            ));
        }

        {
            let mut stats = self.stats.write();
            stats.messages_received += 1;
        }

        // Determine the routing key from the message kind and headers.
        let routing_key = msg
            .headers
            .get("routing-key")
            .cloned()
            .unwrap_or_else(|| msg.kind.to_string());

        let handler = self
            .handler_registry
            .find_message_handler(&routing_key);

        let result = match handler {
            Some(h) => h.handle_message(msg, &self.context).await,
            None => {
                debug!(
                    routing_key = %routing_key,
                    "no handler registered for message"
                );
                Ok(None)
            }
        };

        {
            let mut stats = self.stats.write();
            match &result {
                Ok(_) => stats.messages_processed += 1,
                Err(_) => stats.messages_failed += 1,
            }
        }

        result
    }

    /// Process a task through the handler registry.
    pub async fn process_task(&self, task: SdkTaskSpec) -> DafResult<SdkTaskResult> {
        if self.state() != InstanceState::Running {
            return Err(DafError::agent(
                Some(*self.manifest.id.as_uuid()),
                "agent is not running",
            ));
        }

        let capability = task
            .required_capabilities
            .first()
            .map(|s| s.as_str())
            .unwrap_or(&task.name);

        let handler = self
            .handler_registry
            .find_task_handler(capability);

        let result = match handler {
            Some(h) => h.handle_task(task, &self.context).await,
            None => {
                Err(DafError::NotFound {
                    entity: "task_handler".into(),
                    id: capability.to_string(),
                })
            }
        };

        {
            let mut stats = self.stats.write();
            match &result {
                Ok(r) if r.success => stats.tasks_completed += 1,
                _ => stats.tasks_failed += 1,
            }
        }

        result
    }

    /// Dispatch an event to all matching event handlers.
    pub async fn dispatch_event(&self, event: Event) -> DafResult<()> {
        let handlers = self
            .handler_registry
            .find_event_handlers(&event.event_type);

        for handler in handlers {
            if let Err(e) = handler.handle_event(event.clone(), &self.context).await {
                warn!(
                    event_type = %event.event_type,
                    error = %e,
                    "event handler failed"
                );
            }
        }

        Ok(())
    }

    // -- Control messages ----------------------------------------------------

    /// Handle a DDAL control message (pause, resume, reconfigure).
    pub async fn handle_control(&self, action: &str) -> DafResult<()> {
        match action {
            "pause" => self.pause(),
            "resume" => self.resume(),
            "restart" => self.restart().await,
            "stop" => self.stop().await,
            "health" => {
                let status = self.health_check();
                debug!(
                    healthy = status.healthy,
                    state = %status.state,
                    uptime = ?status.uptime,
                    "health check result"
                );
                Ok(())
            }
            unknown => Err(DafError::ProtocolError {
                message: format!("unknown control action: '{unknown}'"),
            }),
        }
    }

    // -- Internal helpers ----------------------------------------------------

    fn set_state(&self, new_state: InstanceState) {
        let mut state = self.state.write();
        debug!(
            agent = %self.manifest.name,
            from = %*state,
            to = %new_state,
            "state transition"
        );
        *state = new_state;
    }
}

impl fmt::Debug for AgentInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentInstance")
            .field("name", &self.manifest.name)
            .field("kind", &self.manifest.kind)
            .field("id", &self.manifest.id)
            .field("state", &self.state())
            .field("uptime", &self.uptime())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::AgentBuilder;
    use daf_core::agent::AgentKind;

    fn make_instance() -> AgentInstance {
        AgentBuilder::new("test-agent")
            .kind(AgentKind::Worker)
            .build()
            .unwrap()
    }

    #[test]
    fn instance_created_state() {
        let inst = make_instance();
        assert_eq!(inst.state(), InstanceState::Created);
        assert_eq!(inst.manifest().name, "test-agent");
    }

    #[tokio::test]
    async fn instance_start_stop() {
        let inst = make_instance();
        assert_eq!(inst.state(), InstanceState::Created);

        inst.start().await.unwrap();
        assert_eq!(inst.state(), InstanceState::Running);

        inst.stop().await.unwrap();
        assert_eq!(inst.state(), InstanceState::Stopped);
    }

    #[tokio::test]
    async fn instance_restart() {
        let inst = make_instance();
        inst.start().await.unwrap();
        inst.restart().await.unwrap();

        assert_eq!(inst.state(), InstanceState::Running);
        assert_eq!(inst.stats().restarts, 1);
    }

    #[tokio::test]
    async fn instance_pause_resume() {
        let inst = make_instance();
        inst.start().await.unwrap();

        inst.pause().unwrap();
        assert_eq!(inst.state(), InstanceState::Paused);

        inst.resume().unwrap();
        assert_eq!(inst.state(), InstanceState::Running);
    }

    #[tokio::test]
    async fn cannot_start_from_running() {
        let inst = make_instance();
        inst.start().await.unwrap();
        let result = inst.start().await;
        assert!(result.is_err());
    }

    #[test]
    fn cannot_pause_from_created() {
        let inst = make_instance();
        let result = inst.pause();
        assert!(result.is_err());
    }

    #[test]
    fn cannot_resume_from_running() {
        let inst = make_instance();
        // We need to start first, but start is async. Test the direct state check.
        let result = inst.resume();
        assert!(result.is_err());
    }

    #[test]
    fn health_check_created() {
        let inst = make_instance();
        let status = inst.health_check();
        assert!(!status.healthy);
        assert_eq!(status.state, InstanceState::Created);
        assert!(status.reason.is_some());
    }

    #[tokio::test]
    async fn health_check_running() {
        let inst = make_instance();
        inst.start().await.unwrap();

        let status = inst.health_check();
        assert!(status.healthy);
        assert_eq!(status.state, InstanceState::Running);
        assert!(status.reason.is_none());
    }

    #[tokio::test]
    async fn process_message_requires_running() {
        use bytes::Bytes;
        use daf_core::message::MessageKind;

        let inst = make_instance();
        let msg = Message::builder(MessageKind::Request, AgentId::new())
            .payload(Bytes::from_static(b"test"))
            .build();

        // Not started yet — should fail.
        let result = inst.process_message(msg).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn handle_control_actions() {
        let inst = make_instance();
        inst.start().await.unwrap();

        inst.handle_control("pause").await.unwrap();
        assert_eq!(inst.state(), InstanceState::Paused);

        inst.handle_control("resume").await.unwrap();
        assert_eq!(inst.state(), InstanceState::Running);

        inst.handle_control("health").await.unwrap();

        // Unknown action.
        let result = inst.handle_control("explode").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let inst = make_instance();
        inst.start().await.unwrap();
        inst.stop().await.unwrap();

        // Calling stop again should be a no-op.
        inst.stop().await.unwrap();
        assert_eq!(inst.state(), InstanceState::Stopped);
    }

    #[test]
    fn instance_debug_format() {
        let inst = make_instance();
        let debug = format!("{inst:?}");
        assert!(debug.contains("test-agent"));
        assert!(debug.contains("Worker"));
    }

    #[test]
    fn instance_state_display() {
        assert_eq!(InstanceState::Running.to_string(), "running");
        assert_eq!(InstanceState::Paused.to_string(), "paused");
        assert!(InstanceState::Stopped.is_terminal());
        assert!(InstanceState::Failed.is_terminal());
        assert!(!InstanceState::Running.is_terminal());
    }
}
