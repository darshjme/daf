//! Configuration types for the DAF runtime.
//!
//! [`DafConfig`] is the top-level configuration struct loaded from TOML/YAML
//! files or environment variables. It composes sub-configs for transport,
//! logging, and resource defaults.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// DafConfig
// ---------------------------------------------------------------------------

/// Top-level runtime configuration.
///
/// Constructed by loading a config file and optionally overriding fields
/// from environment variables. Use [`Default::default`] for a development
/// configuration that works out of the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DafConfig {
    /// Human-readable cluster name used in logs and metrics.
    pub cluster_name: String,

    /// Unique identifier for this node within the cluster.
    pub node_id: Uuid,

    /// Network address to bind listeners on.
    pub bind_address: IpAddr,

    /// Base port for the primary transport listener.
    pub bind_port: u16,

    /// Directory for persistent state (task logs, agent checkpoints).
    pub data_dir: PathBuf,

    /// Maximum number of agents that may run concurrently on this node.
    pub max_agents: usize,

    /// Maximum number of tasks that may be queued globally.
    pub max_queued_tasks: usize,

    /// How often (in seconds) the runtime sends health probes to agents.
    pub health_check_interval_secs: u64,

    /// Grace period (in seconds) for agent shutdown before force-kill.
    pub shutdown_timeout_secs: u64,

    /// Transport layer configuration.
    pub transport: TransportConfig,

    /// Logging configuration.
    pub log: LogConfig,

    /// Whether to enable the built-in metrics HTTP endpoint.
    pub metrics_enabled: bool,

    /// Port for the metrics/debug HTTP server (only if `metrics_enabled`).
    pub metrics_port: u16,
}

impl Default for DafConfig {
    fn default() -> Self {
        Self {
            cluster_name: "daf-local".into(),
            node_id: Uuid::now_v7(),
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            bind_port: 9400,
            data_dir: PathBuf::from("/var/lib/daf"),
            max_agents: 256,
            max_queued_tasks: 10_000,
            health_check_interval_secs: 30,
            shutdown_timeout_secs: 15,
            transport: TransportConfig::default(),
            log: LogConfig::default(),
            metrics_enabled: true,
            metrics_port: 9401,
        }
    }
}

impl DafConfig {
    /// Create a development configuration that binds to localhost with
    /// a temporary data directory.
    pub fn development() -> Self {
        Self {
            cluster_name: "daf-dev".into(),
            data_dir: std::env::temp_dir().join("daf-dev"),
            max_agents: 32,
            max_queued_tasks: 1_000,
            health_check_interval_secs: 10,
            log: LogConfig {
                level: LogLevel::Debug,
                format: LogFormat::Text,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Validate that the configuration is internally consistent.
    pub fn validate(&self) -> crate::error::DafResult<()> {
        if self.cluster_name.is_empty() {
            return Err(crate::error::DafError::ConfigError(
                "cluster_name must not be empty".into(),
            ));
        }
        if self.max_agents == 0 {
            return Err(crate::error::DafError::ConfigError(
                "max_agents must be > 0".into(),
            ));
        }
        if self.bind_port == 0 {
            return Err(crate::error::DafError::ConfigError(
                "bind_port must be > 0".into(),
            ));
        }
        self.transport.validate()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TransportConfig
// ---------------------------------------------------------------------------

/// Configuration for the inter-agent transport layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Transport protocol.
    pub protocol: TransportProtocol,

    /// Address to connect to (for clients) or bind on (for servers).
    pub address: String,

    /// Port number.
    pub port: u16,

    /// Path to the TLS certificate file (PEM). Required when `protocol` is `Tls`.
    pub tls_cert_path: Option<PathBuf>,

    /// Path to the TLS private key file (PEM). Required when `protocol` is `Tls`.
    pub tls_key_path: Option<PathBuf>,

    /// Maximum message size in bytes before rejection.
    pub max_message_size: usize,

    /// TCP keep-alive interval in seconds. `None` disables keep-alive.
    pub keepalive_secs: Option<u64>,

    /// Maximum number of concurrent connections.
    pub max_connections: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            protocol: TransportProtocol::Tcp,
            address: "127.0.0.1".into(),
            port: 9400,
            tls_cert_path: None,
            tls_key_path: None,
            max_message_size: 16 * 1024 * 1024, // 16 MiB
            keepalive_secs: Some(30),
            max_connections: 1024,
        }
    }
}

impl TransportConfig {
    /// Validate TLS configuration consistency.
    pub fn validate(&self) -> crate::error::DafResult<()> {
        if self.protocol == TransportProtocol::Tls {
            if self.tls_cert_path.is_none() {
                return Err(crate::error::DafError::ConfigError(
                    "tls_cert_path required when protocol is TLS".into(),
                ));
            }
            if self.tls_key_path.is_none() {
                return Err(crate::error::DafError::ConfigError(
                    "tls_key_path required when protocol is TLS".into(),
                ));
            }
        }
        Ok(())
    }

    /// Produce the socket address string (`"host:port"`).
    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }
}

/// Wire protocol for inter-agent transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportProtocol {
    /// Plain TCP (development only).
    Tcp,
    /// Unix domain socket (same-host agents).
    Unix,
    /// TLS over TCP (production).
    Tls,
}

impl std::fmt::Display for TransportProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp => write!(f, "tcp"),
            Self::Unix => write!(f, "unix"),
            Self::Tls => write!(f, "tls"),
        }
    }
}

// ---------------------------------------------------------------------------
// LogConfig
// ---------------------------------------------------------------------------

/// Configuration for structured logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Minimum severity level to emit.
    pub level: LogLevel,
    /// Output format.
    pub format: LogFormat,
    /// Where to write logs.
    pub output: LogOutput,
    /// File path when `output` is [`LogOutput::File`].
    pub file_path: Option<PathBuf>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            format: LogFormat::Json,
            output: LogOutput::Stdout,
            file_path: None,
        }
    }
}

/// Log severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Extremely verbose, only for deep debugging.
    Trace,
    /// Diagnostic information for developers.
    Debug,
    /// Normal operational messages.
    Info,
    /// Potential issues that are not yet errors.
    Warn,
    /// Failures that need attention.
    Error,
}

impl LogLevel {
    /// Convert to a `tracing` filter directive string.
    pub fn as_filter_str(&self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_filter_str())
    }
}

/// Output format for structured logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Machine-readable JSON (one object per line).
    Json,
    /// Human-readable colored text.
    Text,
}

/// Destination for log output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogOutput {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// A file on disk (path set in [`LogConfig::file_path`]).
    File,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        let cfg = DafConfig::default();
        cfg.validate().unwrap();
    }

    #[test]
    fn dev_config_validates() {
        let cfg = DafConfig::development();
        cfg.validate().unwrap();
        assert_eq!(cfg.log.level, LogLevel::Debug);
    }

    #[test]
    fn empty_cluster_name_fails_validation() {
        let mut cfg = DafConfig::default();
        cfg.cluster_name = String::new();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_max_agents_fails_validation() {
        let mut cfg = DafConfig::default();
        cfg.max_agents = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn tls_without_cert_fails_validation() {
        let mut cfg = DafConfig::default();
        cfg.transport.protocol = TransportProtocol::Tls;
        cfg.transport.tls_cert_path = None;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn tls_with_cert_validates() {
        let mut cfg = DafConfig::default();
        cfg.transport.protocol = TransportProtocol::Tls;
        cfg.transport.tls_cert_path = Some("/tmp/cert.pem".into());
        cfg.transport.tls_key_path = Some("/tmp/key.pem".into());
        cfg.validate().unwrap();
    }

    #[test]
    fn transport_socket_addr() {
        let t = TransportConfig::default();
        assert_eq!(t.socket_addr(), "127.0.0.1:9400");
    }

    #[test]
    fn config_serde_roundtrip() {
        let cfg = DafConfig::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let back: DafConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cluster_name, cfg.cluster_name);
        assert_eq!(back.bind_port, cfg.bind_port);
    }

    #[test]
    fn log_level_filter_strings() {
        assert_eq!(LogLevel::Trace.as_filter_str(), "trace");
        assert_eq!(LogLevel::Error.as_filter_str(), "error");
    }
}
