//! Core runtime struct and builder.
//!
//! The [`Runtime`] is the central coordinator that owns every subsystem and
//! manages the full lifecycle from boot to shutdown. It transitions through
//! a state machine: `Initializing -> Running -> ShuttingDown -> Stopped`.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use tracing::{error, info, warn};

use daf_core::{DafError, DafResult};

use crate::config::RuntimeConfig;
use crate::health::{HealthMonitor, SubsystemHealth, SubsystemKind};
use crate::metrics::RuntimeMetrics;
use crate::node::{cluster_leave, Node, PeerTracker};
use crate::signal::ShutdownSignal;

// ---------------------------------------------------------------------------
// RuntimeState
// ---------------------------------------------------------------------------

/// State machine governing the runtime lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    /// Subsystems are being initialized.
    Initializing,
    /// The runtime is fully operational and processing work.
    Running,
    /// Graceful shutdown has been initiated.
    ShuttingDown,
    /// All subsystems have been torn down.
    Stopped,
}

impl std::fmt::Display for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Initializing => "initializing",
            Self::Running => "running",
            Self::ShuttingDown => "shutting_down",
            Self::Stopped => "stopped",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// The executable heart of DAF.
///
/// Owns every subsystem handle and coordinates startup, health monitoring,
/// and shutdown. Construct via [`RuntimeBuilder`] or [`bootstrap`](crate::bootstrap).
pub struct Runtime {
    /// Node identity.
    node: Node,

    /// Full configuration snapshot.
    config: RuntimeConfig,

    /// Current lifecycle state.
    state: Arc<RwLock<RuntimeState>>,

    /// Cooperative shutdown signal.
    shutdown: ShutdownSignal,

    /// Health monitor.
    health: HealthMonitor,

    /// Runtime metrics.
    metrics: RuntimeMetrics,

    /// Peer tracker for cluster membership.
    peers: PeerTracker,

    /// When this runtime instance was created.
    created_at: DateTime<Utc>,
}

impl Runtime {
    /// Construct a new runtime from a validated configuration.
    ///
    /// This performs minimal initialization. For the full bootstrap sequence
    /// including subsystem startup, use [`bootstrap`](crate::bootstrap).
    pub fn new(config: RuntimeConfig) -> DafResult<Self> {
        config.validate()?;

        let node = Node::new(&config.node_name, config.bind_address);
        let shutdown = ShutdownSignal::new(config.shutdown_timeout);
        let health = HealthMonitor::new(&config.node_name, config.health_check_interval);
        let metrics = RuntimeMetrics::new();
        let peers = PeerTracker::new();

        info!(
            node = %node,
            state = %RuntimeState::Initializing,
            "runtime created"
        );

        Ok(Self {
            node,
            config,
            state: Arc::new(RwLock::new(RuntimeState::Initializing)),
            shutdown,
            health,
            metrics,
            peers,
            created_at: Utc::now(),
        })
    }

    /// Start the runtime — transition to `Running` and begin all background
    /// loops (signal listener, health monitor, metrics exporter).
    ///
    /// This method blocks until a shutdown signal is received and the
    /// graceful shutdown completes.
    pub async fn start(&self) -> DafResult<()> {
        self.transition(RuntimeState::Running)?;
        info!(node = %self.node, "runtime started");

        // Spawn the forced-shutdown deadline watcher.
        let _deadline_handle = self.shutdown.spawn_force_deadline();

        // Spawn health monitoring loop.
        let health_handle = self.spawn_health_loop();

        // Spawn metrics snapshot loop.
        let metrics_handle = self.spawn_metrics_loop();

        // Spawn config reload listener.
        let reload_handle = self.spawn_reload_listener();

        // Block on signal listener — this is the main event loop.
        self.shutdown.listen_for_signals().await;

        // Shutdown was triggered.
        self.shutdown_inner().await?;

        // Cancel background tasks.
        health_handle.abort();
        metrics_handle.abort();
        reload_handle.abort();

        Ok(())
    }

    /// Initiate graceful shutdown.
    ///
    /// Can be called programmatically (in addition to signal-triggered shutdown).
    pub async fn shutdown(&self) -> DafResult<()> {
        self.shutdown.trigger();
        self.shutdown_inner().await
    }

    /// Internal shutdown sequence.
    async fn shutdown_inner(&self) -> DafResult<()> {
        if *self.state.read() == RuntimeState::Stopped {
            return Ok(());
        }

        self.transition(RuntimeState::ShuttingDown)?;
        info!(node = %self.node, "initiating graceful shutdown");

        // Leave the cluster.
        cluster_leave(&self.node, &self.peers).await;

        // Give subsystems time to drain.
        let timeout = self.config.shutdown_timeout;
        let drain_result = tokio::time::timeout(timeout, self.drain_subsystems()).await;

        match drain_result {
            Ok(Ok(())) => {
                info!("all subsystems drained successfully");
            }
            Ok(Err(e)) => {
                error!(error = %e, "error during subsystem drain");
            }
            Err(_) => {
                warn!(
                    timeout_secs = timeout.as_secs(),
                    "shutdown timeout exceeded, forcing stop"
                );
                self.shutdown.force();
            }
        }

        self.transition(RuntimeState::Stopped)?;
        info!(
            node = %self.node,
            uptime_secs = self.uptime().as_secs(),
            "runtime stopped"
        );

        Ok(())
    }

    /// Drain all subsystems during shutdown.
    async fn drain_subsystems(&self) -> DafResult<()> {
        // In the current implementation, subsystem crates are placeholders.
        // When they are fleshed out, this method will call shutdown on each:
        // - orchestrator.shutdown()
        // - transport.shutdown()
        // - logger.flush()
        // - memory.flush()
        // - registry.deregister_all()
        //
        // For now, yield to allow any pending async work to complete.
        tokio::task::yield_now().await;
        Ok(())
    }

    /// Spawn the periodic health-check background loop.
    fn spawn_health_loop(&self) -> tokio::task::JoinHandle<()> {
        let health = self.health.clone();
        let metrics = self.metrics.clone();
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let interval = health.interval();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        let reports = vec![
                            SubsystemHealth::healthy(SubsystemKind::Transport),
                            SubsystemHealth::healthy(SubsystemKind::Logger),
                            SubsystemHealth::healthy(SubsystemKind::Memory),
                            SubsystemHealth::healthy(SubsystemKind::Registry),
                            SubsystemHealth::healthy(SubsystemKind::Orchestrator),
                        ];
                        health.update(reports);
                        metrics.health_check_performed();
                    }
                    _ = shutdown.wait() => {
                        break;
                    }
                }
            }
        })
    }

    /// Spawn the periodic metrics snapshot loop.
    fn spawn_metrics_loop(&self) -> tokio::task::JoinHandle<()> {
        let metrics = self.metrics.clone();
        let shutdown = self.shutdown.clone();
        let interval = self.config.metrics_interval;

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        let snap = metrics.snapshot();
                        tracing::debug!(
                            uptime = snap.uptime_secs,
                            agents = snap.agents_active,
                            tasks = snap.tasks_active,
                            messages = snap.messages_routed_total,
                            "metrics snapshot"
                        );
                    }
                    _ = shutdown.wait() => {
                        break;
                    }
                }
            }
        })
    }

    /// Spawn the config reload listener.
    fn spawn_reload_listener(&self) -> tokio::task::JoinHandle<()> {
        let shutdown_signal = self.shutdown.clone();
        let metrics = self.metrics.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_signal.wait_reload() => {
                        info!("configuration reload triggered (not yet implemented)");
                        metrics.config_reloaded();
                        // TODO: re-read config file and apply changes.
                    }
                    _ = shutdown_signal.wait() => {
                        break;
                    }
                }
            }
        })
    }

    // -- Accessors -----------------------------------------------------------

    /// Current runtime state.
    pub fn state(&self) -> RuntimeState {
        *self.state.read()
    }

    /// Node identity.
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// Runtime configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Shutdown signal handle.
    pub fn shutdown_signal(&self) -> &ShutdownSignal {
        &self.shutdown
    }

    /// Health monitor.
    pub fn health(&self) -> &HealthMonitor {
        &self.health
    }

    /// Runtime metrics.
    pub fn metrics(&self) -> &RuntimeMetrics {
        &self.metrics
    }

    /// Peer tracker.
    pub fn peers(&self) -> &PeerTracker {
        &self.peers
    }

    /// Latest health snapshot.
    pub fn system_health(&self) -> crate::health::SystemHealth {
        self.health.latest()
    }

    /// How long the runtime has been alive.
    pub fn uptime(&self) -> Duration {
        (Utc::now() - self.created_at)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }

    // -- State transitions ---------------------------------------------------

    /// Attempt a state transition, returning an error if the transition is
    /// invalid.
    fn transition(&self, target: RuntimeState) -> DafResult<()> {
        let mut current = self.state.write();
        let valid = match (*current, target) {
            (RuntimeState::Initializing, RuntimeState::Running) => true,
            (RuntimeState::Running, RuntimeState::ShuttingDown) => true,
            (RuntimeState::ShuttingDown, RuntimeState::Stopped) => true,
            // Allow idempotent transitions.
            (s, t) if s == t => true,
            _ => false,
        };

        if !valid {
            return Err(DafError::Internal(format!(
                "invalid runtime state transition: {current} -> {target}"
            )));
        }

        if *current != target {
            info!(from = %*current, to = %target, "runtime state transition");
            *current = target;
        }

        Ok(())
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("node", &self.node.name)
            .field("state", &self.state())
            .field("bind_address", &self.config.bind_address)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// RuntimeBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for constructing a [`Runtime`] with optional overrides.
///
/// ```rust,no_run
/// use daf_runtime::{RuntimeBuilder, RuntimeConfig};
///
/// # async fn example() {
/// let runtime = RuntimeBuilder::new()
///     .node_name("my-node")
///     .bind_address("0.0.0.0:9500".parse().unwrap())
///     .max_agents(256)
///     .build()
///     .expect("failed to build runtime");
/// # }
/// ```
pub struct RuntimeBuilder {
    config: RuntimeConfig,
}

impl RuntimeBuilder {
    /// Start with the default development configuration.
    pub fn new() -> Self {
        Self {
            config: RuntimeConfig::development(),
        }
    }

    /// Start from a specific configuration.
    pub fn from_config(config: RuntimeConfig) -> Self {
        Self { config }
    }

    /// Set the node name.
    pub fn node_name(mut self, name: impl Into<String>) -> Self {
        self.config.node_name = name.into();
        self
    }

    /// Set the bind address.
    pub fn bind_address(mut self, addr: std::net::SocketAddr) -> Self {
        self.config.bind_address = addr;
        self
    }

    /// Set the data directory.
    pub fn data_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.config.data_dir = path.into();
        self
    }

    /// Set the maximum number of agents.
    pub fn max_agents(mut self, n: usize) -> Self {
        self.config.max_agents = n;
        self
    }

    /// Set the shutdown timeout.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout = timeout;
        self
    }

    /// Set transport configuration.
    pub fn transport(mut self, config: crate::config::TransportConfig) -> Self {
        self.config.transport = config;
        self
    }

    /// Set logging configuration.
    pub fn logging(mut self, config: crate::config::LogConfig) -> Self {
        self.config.logging = config;
        self
    }

    /// Set memory configuration.
    pub fn memory(mut self, config: crate::config::MemoryConfig) -> Self {
        self.config.memory = config;
        self
    }

    /// Set registry configuration.
    pub fn registry(mut self, config: crate::config::RegistryConfig) -> Self {
        self.config.registry = config;
        self
    }

    /// Set orchestrator configuration.
    pub fn orchestrator(mut self, config: crate::config::OrchestratorConfig) -> Self {
        self.config.orchestrator = config;
        self
    }

    /// Set the health check interval.
    pub fn health_check_interval(mut self, interval: Duration) -> Self {
        self.config.health_check_interval = interval;
        self
    }

    /// Set the metrics snapshot interval.
    pub fn metrics_interval(mut self, interval: Duration) -> Self {
        self.config.metrics_interval = interval;
        self
    }

    /// Add cluster peers.
    pub fn cluster_peers(mut self, peers: Vec<std::net::SocketAddr>) -> Self {
        self.config.cluster_peers = peers;
        self
    }

    /// Apply environment variable overrides before building.
    pub fn with_env_overrides(mut self) -> Self {
        self.config.apply_env_overrides();
        self
    }

    /// Validate the configuration and construct the runtime.
    pub fn build(self) -> DafResult<Runtime> {
        Runtime::new(self.config)
    }

    /// Access the config being built (for inspection).
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_new_with_valid_config() {
        let config = RuntimeConfig::development();
        let rt = Runtime::new(config).unwrap();
        assert_eq!(rt.state(), RuntimeState::Initializing);
        assert_eq!(rt.node().name, "daf-dev");
    }

    #[test]
    fn runtime_new_with_invalid_config() {
        let mut config = RuntimeConfig::development();
        config.node_name = String::new();
        let result = Runtime::new(config);
        assert!(result.is_err());
    }

    #[test]
    fn runtime_state_transitions() {
        let rt = Runtime::new(RuntimeConfig::development()).unwrap();
        assert_eq!(rt.state(), RuntimeState::Initializing);

        rt.transition(RuntimeState::Running).unwrap();
        assert_eq!(rt.state(), RuntimeState::Running);

        rt.transition(RuntimeState::ShuttingDown).unwrap();
        assert_eq!(rt.state(), RuntimeState::ShuttingDown);

        rt.transition(RuntimeState::Stopped).unwrap();
        assert_eq!(rt.state(), RuntimeState::Stopped);
    }

    #[test]
    fn runtime_invalid_transition() {
        let rt = Runtime::new(RuntimeConfig::development()).unwrap();
        let result = rt.transition(RuntimeState::Stopped);
        assert!(result.is_err());
    }

    #[test]
    fn runtime_idempotent_transition() {
        let rt = Runtime::new(RuntimeConfig::development()).unwrap();
        rt.transition(RuntimeState::Initializing).unwrap();
        assert_eq!(rt.state(), RuntimeState::Initializing);
    }

    #[test]
    fn builder_defaults() {
        let builder = RuntimeBuilder::new();
        assert_eq!(builder.config().node_name, "daf-dev");
        assert_eq!(builder.config().max_agents, 128);
    }

    #[test]
    fn builder_overrides() {
        let rt = RuntimeBuilder::new()
            .node_name("custom-node")
            .max_agents(512)
            .shutdown_timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        assert_eq!(rt.node().name, "custom-node");
        assert_eq!(rt.config().max_agents, 512);
        assert_eq!(rt.config().shutdown_timeout, Duration::from_secs(30));
    }

    #[test]
    fn builder_bind_address() {
        let addr: std::net::SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let rt = RuntimeBuilder::new().bind_address(addr).build().unwrap();
        assert_eq!(rt.config().bind_address, addr);
    }

    #[test]
    fn builder_from_config() {
        let mut config = RuntimeConfig::development();
        config.node_name = "from-config".into();
        let rt = RuntimeBuilder::from_config(config).build().unwrap();
        assert_eq!(rt.node().name, "from-config");
    }

    #[test]
    fn builder_invalid_config_fails() {
        let result = RuntimeBuilder::new().max_agents(0).build();
        assert!(result.is_err());
    }

    #[test]
    fn runtime_debug_format() {
        let rt = Runtime::new(RuntimeConfig::development()).unwrap();
        let debug = format!("{rt:?}");
        assert!(debug.contains("Runtime"));
        assert!(debug.contains("daf-dev"));
    }

    #[test]
    fn runtime_accessors() {
        let rt = Runtime::new(RuntimeConfig::development()).unwrap();
        assert_eq!(rt.config().max_agents, 128);
        assert!(!rt.shutdown_signal().is_triggered());
        assert_eq!(
            rt.health().latest().status,
            crate::health::HealthStatus::Unknown
        );
        assert_eq!(rt.metrics().agents_spawned_total(), 0);
        assert!(rt.peers().is_empty());
    }

    #[test]
    fn runtime_state_display() {
        assert_eq!(RuntimeState::Initializing.to_string(), "initializing");
        assert_eq!(RuntimeState::Running.to_string(), "running");
        assert_eq!(RuntimeState::ShuttingDown.to_string(), "shutting_down");
        assert_eq!(RuntimeState::Stopped.to_string(), "stopped");
    }

    #[tokio::test]
    async fn runtime_programmatic_shutdown() {
        let rt = Runtime::new(RuntimeConfig::development()).unwrap();
        rt.transition(RuntimeState::Running).unwrap();
        rt.shutdown().await.unwrap();
        assert_eq!(rt.state(), RuntimeState::Stopped);
    }
}
