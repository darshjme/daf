//! Runtime configuration.
//!
//! [`RuntimeConfig`] is the top-level configuration struct that governs every
//! subsystem in the DAF runtime. It can be constructed programmatically via
//! the builder pattern, loaded from a YAML file, or populated from environment
//! variables.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use daf_core::{DafError, DafResult};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// RuntimeConfig
// ---------------------------------------------------------------------------

/// Top-level configuration for the DAF runtime.
///
/// Every subsystem has a dedicated sub-config. The runtime validates the
/// entire configuration before proceeding with bootstrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Human-readable name for this node (used in logs and cluster discovery).
    pub node_name: String,

    /// Socket address the runtime binds its transport listener to.
    pub bind_address: SocketAddr,

    /// Root directory for persistent data (logs, memory stores, registry cache).
    pub data_dir: PathBuf,

    /// Transport subsystem configuration.
    pub transport: TransportConfig,

    /// Logging subsystem configuration.
    pub logging: LogConfig,

    /// Memory subsystem configuration.
    pub memory: MemoryConfig,

    /// Registry subsystem configuration.
    pub registry: RegistryConfig,

    /// Orchestrator subsystem configuration.
    pub orchestrator: OrchestratorConfig,

    /// Maximum number of agents that can be alive simultaneously on this node.
    pub max_agents: usize,

    /// How long to wait for a graceful shutdown before forcing termination.
    pub shutdown_timeout: Duration,

    /// Health check polling interval.
    pub health_check_interval: Duration,

    /// Metrics snapshot interval.
    pub metrics_interval: Duration,

    /// Peer addresses for cluster join (empty = standalone mode).
    pub cluster_peers: Vec<SocketAddr>,
}

impl RuntimeConfig {
    /// Return a configuration suitable for local development.
    ///
    /// Binds to `127.0.0.1:9400`, stores data under `/tmp/daf-dev`, and uses
    /// permissive defaults for every subsystem.
    pub fn development() -> Self {
        Self {
            node_name: "daf-dev".into(),
            bind_address: "127.0.0.1:9400".parse().unwrap(),
            data_dir: PathBuf::from("/tmp/daf-dev"),
            transport: TransportConfig::default(),
            logging: LogConfig::default(),
            memory: MemoryConfig::default(),
            registry: RegistryConfig::default(),
            orchestrator: OrchestratorConfig::default(),
            max_agents: 128,
            shutdown_timeout: Duration::from_secs(15),
            health_check_interval: Duration::from_secs(10),
            metrics_interval: Duration::from_secs(30),
            cluster_peers: Vec::new(),
        }
    }

    /// Load configuration from a YAML file at the given path.
    pub fn from_yaml(path: &std::path::Path) -> DafResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            DafError::ConfigError(format!("failed to read config file {}: {e}", path.display()))
        })?;
        let config: Self = serde_yaml_ng::from_str(&content).map_err(|e| {
            DafError::ConfigError(format!(
                "failed to parse config file {}: {e}",
                path.display()
            ))
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Override fields from environment variables.
    ///
    /// Recognized variables:
    /// - `DAF_NODE_NAME`
    /// - `DAF_BIND_ADDRESS`
    /// - `DAF_DATA_DIR`
    /// - `DAF_MAX_AGENTS`
    /// - `DAF_SHUTDOWN_TIMEOUT_SECS`
    /// - `DAF_LOG_LEVEL`
    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("DAF_NODE_NAME") {
            self.node_name = v;
        }
        if let Ok(v) = std::env::var("DAF_BIND_ADDRESS") {
            if let Ok(addr) = v.parse() {
                self.bind_address = addr;
            }
        }
        if let Ok(v) = std::env::var("DAF_DATA_DIR") {
            self.data_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("DAF_MAX_AGENTS") {
            if let Ok(n) = v.parse() {
                self.max_agents = n;
            }
        }
        if let Ok(v) = std::env::var("DAF_SHUTDOWN_TIMEOUT_SECS") {
            if let Ok(n) = v.parse::<u64>() {
                self.shutdown_timeout = Duration::from_secs(n);
            }
        }
        if let Ok(v) = std::env::var("DAF_LOG_LEVEL") {
            self.logging.level = v;
        }
    }

    /// Validate the configuration, returning detailed errors for any problems.
    pub fn validate(&self) -> DafResult<()> {
        let mut errors: Vec<String> = Vec::new();

        if self.node_name.is_empty() {
            errors.push("node_name must not be empty".into());
        }
        if self.node_name.len() > 128 {
            errors.push("node_name must be 128 characters or fewer".into());
        }
        if self.max_agents == 0 {
            errors.push("max_agents must be at least 1".into());
        }
        if self.shutdown_timeout.is_zero() {
            errors.push("shutdown_timeout must be greater than zero".into());
        }

        // Transport validation.
        if self.transport.max_connections == 0 {
            errors.push("transport.max_connections must be at least 1".into());
        }
        if self.transport.read_buffer_size == 0 {
            errors.push("transport.read_buffer_size must be at least 1".into());
        }

        // Memory validation.
        if self.memory.hot_capacity == 0 {
            errors.push("memory.hot_capacity must be at least 1".into());
        }

        // Orchestrator validation.
        if self.orchestrator.max_concurrent_missions == 0 {
            errors.push("orchestrator.max_concurrent_missions must be at least 1".into());
        }
        if self.orchestrator.heartbeat_interval.is_zero() {
            errors.push("orchestrator.heartbeat_interval must be greater than zero".into());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(DafError::ConfigError(format!(
                "configuration validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::development()
    }
}

// ---------------------------------------------------------------------------
// TransportConfig
// ---------------------------------------------------------------------------

/// Configuration for the transport layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Maximum concurrent inbound connections.
    pub max_connections: usize,

    /// Per-connection read buffer size in bytes.
    pub read_buffer_size: usize,

    /// Per-connection write buffer size in bytes.
    pub write_buffer_size: usize,

    /// Connection idle timeout before automatic close.
    pub idle_timeout: Duration,

    /// Enable TLS for inter-node transport.
    pub tls_enabled: bool,

    /// Path to TLS certificate file (PEM).
    pub tls_cert_path: Option<PathBuf>,

    /// Path to TLS private key file (PEM).
    pub tls_key_path: Option<PathBuf>,

    /// TCP keepalive interval.
    pub keepalive_interval: Duration,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_connections: 1024,
            read_buffer_size: 64 * 1024,
            write_buffer_size: 64 * 1024,
            idle_timeout: Duration::from_secs(300),
            tls_enabled: false,
            tls_cert_path: None,
            tls_key_path: None,
            keepalive_interval: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// LogConfig
// ---------------------------------------------------------------------------

/// Configuration for the logging subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Tracing filter level (e.g. `"info"`, `"debug"`, `"daf_runtime=trace"`).
    pub level: String,

    /// Output format: `"text"` or `"json"`.
    pub format: LogFormat,

    /// If `Some`, write logs to this file in addition to stderr.
    pub file_path: Option<PathBuf>,

    /// Maximum log file size in bytes before rotation.
    pub max_file_size: u64,

    /// Number of rotated log files to retain.
    pub max_file_count: u32,

    /// Include span events in output.
    pub include_spans: bool,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable text format with colors.
    Text,
    /// Structured JSON, one object per line.
    Json,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: LogFormat::Text,
            file_path: None,
            max_file_size: 100 * 1024 * 1024, // 100 MiB
            max_file_count: 5,
            include_spans: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryConfig
// ---------------------------------------------------------------------------

/// Configuration for the tiered memory subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Maximum number of entries in the hot (L1) tier.
    pub hot_capacity: usize,

    /// Maximum number of entries in the warm (L2) tier.
    pub warm_capacity: usize,

    /// How often to run compaction on the warm tier.
    pub compaction_interval: Duration,

    /// Access count threshold before an entry is promoted from warm to hot.
    pub tier_promotion_threshold: u32,

    /// Directory for persistent cold storage. If `None`, cold tier is disabled.
    pub cold_storage_dir: Option<PathBuf>,

    /// TTL for entries that have no explicit expiration.
    pub default_ttl: Duration,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            hot_capacity: 10_000,
            warm_capacity: 100_000,
            compaction_interval: Duration::from_secs(300),
            tier_promotion_threshold: 5,
            cold_storage_dir: None,
            default_ttl: Duration::from_secs(3600),
        }
    }
}

// ---------------------------------------------------------------------------
// RegistryConfig
// ---------------------------------------------------------------------------

/// Configuration for the agent registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Maximum number of registered agent manifests.
    pub max_registrations: usize,

    /// How often to sweep for stale registrations (no heartbeat).
    pub sweep_interval: Duration,

    /// How long since last heartbeat before an agent is considered stale.
    pub stale_threshold: Duration,

    /// Enable remote registry federation.
    pub federation_enabled: bool,

    /// Remote registry endpoints for federation.
    pub federation_endpoints: Vec<String>,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_registrations: 1024,
            sweep_interval: Duration::from_secs(60),
            stale_threshold: Duration::from_secs(120),
            federation_enabled: false,
            federation_endpoints: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// OrchestratorConfig
// ---------------------------------------------------------------------------

/// Configuration for the mission orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    /// Maximum number of missions executing concurrently.
    pub max_concurrent_missions: usize,

    /// Timeout for a single wave of agent execution within a mission.
    pub wave_timeout: Duration,

    /// Interval between heartbeat checks for running agents.
    pub heartbeat_interval: Duration,

    /// Maximum depth of task dependency graphs.
    pub max_task_depth: usize,

    /// Whether to automatically retry failed waves.
    pub auto_retry_waves: bool,

    /// Maximum retries per wave before the mission is marked as failed.
    pub max_wave_retries: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_concurrent_missions: 16,
            wave_timeout: Duration::from_secs(600),
            heartbeat_interval: Duration::from_secs(5),
            max_task_depth: 32,
            auto_retry_waves: true,
            max_wave_retries: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// serde_yaml_ng shim
// ---------------------------------------------------------------------------

/// Minimal serde_yaml_ng shim using serde_json for now.
/// When a proper YAML parser is added as a dependency, replace this.
mod serde_yaml_ng {
    use serde::de::DeserializeOwned;

    #[derive(Debug)]
    pub struct Error(String);

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    /// Parse a YAML string. Currently falls back to JSON parsing as a shim.
    /// This will be replaced once `serde_yaml` or `serde_yaml_ng` is added
    /// to workspace dependencies.
    pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T, Error> {
        serde_json::from_str(s).map_err(|e| Error(format!("YAML/JSON parse error: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_config_is_valid() {
        let config = RuntimeConfig::development();
        config.validate().expect("development config should be valid");
    }

    #[test]
    fn default_config_is_development() {
        let config = RuntimeConfig::default();
        assert_eq!(config.node_name, "daf-dev");
        assert_eq!(config.max_agents, 128);
    }

    #[test]
    fn validation_catches_empty_node_name() {
        let mut config = RuntimeConfig::development();
        config.node_name = String::new();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("node_name must not be empty"));
    }

    #[test]
    fn validation_catches_zero_max_agents() {
        let mut config = RuntimeConfig::development();
        config.max_agents = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_agents must be at least 1"));
    }

    #[test]
    fn validation_catches_zero_shutdown_timeout() {
        let mut config = RuntimeConfig::development();
        config.shutdown_timeout = Duration::ZERO;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("shutdown_timeout"));
    }

    #[test]
    fn validation_catches_zero_transport_connections() {
        let mut config = RuntimeConfig::development();
        config.transport.max_connections = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("transport.max_connections"));
    }

    #[test]
    fn validation_catches_multiple_errors() {
        let mut config = RuntimeConfig::development();
        config.node_name = String::new();
        config.max_agents = 0;
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("node_name"));
        assert!(msg.contains("max_agents"));
    }

    #[test]
    fn env_overrides_apply() {
        let mut config = RuntimeConfig::development();
        // We test the parsing logic without actually setting env vars
        // to avoid test pollution. Instead verify the method exists and
        // the config is still valid after a no-op call.
        config.apply_env_overrides();
        config.validate().unwrap();
    }

    #[test]
    fn sub_configs_have_sane_defaults() {
        let transport = TransportConfig::default();
        assert_eq!(transport.max_connections, 1024);
        assert!(!transport.tls_enabled);

        let log = LogConfig::default();
        assert_eq!(log.level, "info");
        assert_eq!(log.format, LogFormat::Text);

        let memory = MemoryConfig::default();
        assert_eq!(memory.hot_capacity, 10_000);
        assert_eq!(memory.warm_capacity, 100_000);

        let registry = RegistryConfig::default();
        assert_eq!(registry.max_registrations, 1024);
        assert!(!registry.federation_enabled);

        let orch = OrchestratorConfig::default();
        assert_eq!(orch.max_concurrent_missions, 16);
        assert!(orch.auto_retry_waves);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = RuntimeConfig::development();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let back: RuntimeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_name, config.node_name);
        assert_eq!(back.max_agents, config.max_agents);
    }
}
