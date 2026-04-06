//! SDK configuration.
//!
//! [`SdkConfig`] centralizes all the knobs an agent developer needs to
//! connect to the DAF runtime: transport addresses, logger settings,
//! memory backend selection, registry discovery, and concurrency limits.
//!
//! Configuration can be loaded from environment variables, a TOML/JSON
//! file, or constructed programmatically with the builder.
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::config::{SdkConfig, Environment};
//!
//! // Development defaults (localhost, in-memory everything).
//! let dev = SdkConfig::development();
//!
//! // Production from environment variables.
//! let prod = SdkConfig::from_env()?;
//!
//! // Custom via builder.
//! let custom = SdkConfig::builder()
//!     .registry_url("https://registry.prod.internal:8443")
//!     .heartbeat_interval_secs(10)
//!     .max_concurrent_tasks(32)
//!     .environment(Environment::Production)
//!     .build()?;
//! ```

use std::path::PathBuf;
use std::time::Duration;

use daf_core::error::{DafError, DafResult};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// Deployment environment, governs default values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// Local development. Relaxed timeouts, verbose logging, in-memory stores.
    Development,
    /// Automated test runs. Fast timeouts, in-memory stores, no network.
    Test,
    /// Staging or pre-production. Production-like but with extra diagnostics.
    Staging,
    /// Production. Strict timeouts, durable stores, TLS required.
    Production,
}

impl Default for Environment {
    fn default() -> Self {
        Self::Development
    }
}

impl std::fmt::Display for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Development => write!(f, "development"),
            Self::Test => write!(f, "test"),
            Self::Staging => write!(f, "staging"),
            Self::Production => write!(f, "production"),
        }
    }
}

// ---------------------------------------------------------------------------
// TransportConfig
// ---------------------------------------------------------------------------

/// Transport layer settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Address to bind or connect to (e.g. `"127.0.0.1:9090"`).
    pub address: String,
    /// Enable TLS for agent-to-agent communication.
    pub tls_enabled: bool,
    /// Path to PEM certificate file (required when `tls_enabled` is true).
    pub tls_cert_path: Option<PathBuf>,
    /// Path to PEM private key file.
    pub tls_key_path: Option<PathBuf>,
    /// Connection timeout.
    pub connect_timeout: Duration,
    /// Maximum number of concurrent connections.
    pub max_connections: u32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:9090".into(),
            tls_enabled: false,
            tls_cert_path: None,
            tls_key_path: None,
            connect_timeout: Duration::from_secs(10),
            max_connections: 128,
        }
    }
}

// ---------------------------------------------------------------------------
// LoggerConfig
// ---------------------------------------------------------------------------

/// Logger backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggerConfig {
    /// Base directory for log files.
    pub log_dir: PathBuf,
    /// Minimum log level to capture.
    pub level: String,
    /// Whether to enable structured JSON logging.
    pub structured: bool,
    /// Maximum log file size in bytes before rotation.
    pub max_file_size_bytes: u64,
    /// Maximum number of rotated log files to retain.
    pub max_retained_files: u32,
}

impl Default for LoggerConfig {
    fn default() -> Self {
        Self {
            log_dir: PathBuf::from("./logs"),
            level: "info".into(),
            structured: true,
            max_file_size_bytes: 100 * 1024 * 1024, // 100 MiB
            max_retained_files: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryConfig
// ---------------------------------------------------------------------------

/// Memory subsystem configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Which backend to use: `"in_memory"`, `"sled"`, or `"rocksdb"`.
    pub backend: String,
    /// Path for persistent stores (`sled` or `rocksdb`).
    pub data_dir: Option<PathBuf>,
    /// Maximum number of memories to keep in the hot tier.
    pub hot_tier_capacity: u64,
    /// Whether to enable automatic tier promotion/demotion.
    pub auto_tiering: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: "in_memory".into(),
            data_dir: None,
            hot_tier_capacity: 10_000,
            auto_tiering: true,
        }
    }
}

// ---------------------------------------------------------------------------
// SdkConfig
// ---------------------------------------------------------------------------

/// Top-level SDK configuration.
///
/// Aggregates all subsystem configs into a single struct that can be
/// validated, serialized, and passed to [`AgentBuilder`](crate::builder::AgentBuilder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkConfig {
    /// Deployment environment.
    pub environment: Environment,
    /// Transport layer settings.
    pub transport: TransportConfig,
    /// Logger settings.
    pub logger: LoggerConfig,
    /// Memory subsystem settings.
    pub memory: MemoryConfig,
    /// URL of the agent registry service.
    pub registry_url: String,
    /// Interval between heartbeat pings to the registry.
    pub heartbeat_interval: Duration,
    /// Maximum tasks an agent may execute concurrently.
    pub max_concurrent_tasks: u32,
    /// Graceful shutdown timeout.
    pub shutdown_timeout: Duration,
}

impl Default for SdkConfig {
    fn default() -> Self {
        Self::development()
    }
}

impl SdkConfig {
    /// Development defaults: localhost, in-memory stores, relaxed limits.
    pub fn development() -> Self {
        Self {
            environment: Environment::Development,
            transport: TransportConfig::default(),
            logger: LoggerConfig::default(),
            memory: MemoryConfig::default(),
            registry_url: "http://127.0.0.1:8500".into(),
            heartbeat_interval: Duration::from_secs(30),
            max_concurrent_tasks: 8,
            shutdown_timeout: Duration::from_secs(30),
        }
    }

    /// Production defaults: stricter limits, TLS expected, durable stores.
    pub fn production() -> Self {
        Self {
            environment: Environment::Production,
            transport: TransportConfig {
                address: "0.0.0.0:9090".into(),
                tls_enabled: true,
                connect_timeout: Duration::from_secs(5),
                max_connections: 512,
                ..Default::default()
            },
            logger: LoggerConfig {
                level: "warn".into(),
                max_retained_files: 30,
                ..Default::default()
            },
            memory: MemoryConfig {
                backend: "rocksdb".into(),
                data_dir: Some(PathBuf::from("/var/lib/daf/memory")),
                hot_tier_capacity: 100_000,
                auto_tiering: true,
            },
            registry_url: "https://registry.daf.internal:8443".into(),
            heartbeat_interval: Duration::from_secs(10),
            max_concurrent_tasks: 32,
            shutdown_timeout: Duration::from_secs(60),
        }
    }

    /// Test defaults: minimal, in-memory, fast timeouts.
    pub fn test() -> Self {
        Self {
            environment: Environment::Test,
            transport: TransportConfig {
                connect_timeout: Duration::from_secs(1),
                max_connections: 16,
                ..Default::default()
            },
            logger: LoggerConfig {
                level: "trace".into(),
                ..Default::default()
            },
            memory: MemoryConfig {
                backend: "in_memory".into(),
                hot_tier_capacity: 100,
                auto_tiering: false,
                ..Default::default()
            },
            registry_url: "http://127.0.0.1:8500".into(),
            heartbeat_interval: Duration::from_secs(5),
            max_concurrent_tasks: 4,
            shutdown_timeout: Duration::from_secs(5),
        }
    }

    /// Load configuration from environment variables.
    ///
    /// Supported variables (all optional, fallback to development defaults):
    /// - `DAF_ENV` — `development`, `test`, `staging`, `production`
    /// - `DAF_TRANSPORT_ADDR` — bind/connect address
    /// - `DAF_REGISTRY_URL` — registry service URL
    /// - `DAF_HEARTBEAT_SECS` — heartbeat interval in seconds
    /// - `DAF_MAX_TASKS` — max concurrent tasks
    /// - `DAF_LOG_LEVEL` — log level filter
    /// - `DAF_MEMORY_BACKEND` — `in_memory`, `sled`, `rocksdb`
    /// - `DAF_MEMORY_DIR` — data directory for persistent memory backends
    pub fn from_env() -> DafResult<Self> {
        let base = match std::env::var("DAF_ENV").as_deref() {
            Ok("production") => Self::production(),
            Ok("staging") => Self {
                environment: Environment::Staging,
                ..Self::production()
            },
            Ok("test") => Self::test(),
            _ => Self::development(),
        };

        let mut config = base;

        if let Ok(addr) = std::env::var("DAF_TRANSPORT_ADDR") {
            config.transport.address = addr;
        }
        if let Ok(url) = std::env::var("DAF_REGISTRY_URL") {
            config.registry_url = url;
        }
        if let Ok(secs) = std::env::var("DAF_HEARTBEAT_SECS") {
            let s: u64 = secs.parse().map_err(|e| {
                DafError::ConfigError(format!("invalid DAF_HEARTBEAT_SECS: {e}"))
            })?;
            config.heartbeat_interval = Duration::from_secs(s);
        }
        if let Ok(max) = std::env::var("DAF_MAX_TASKS") {
            let m: u32 = max.parse().map_err(|e| {
                DafError::ConfigError(format!("invalid DAF_MAX_TASKS: {e}"))
            })?;
            config.max_concurrent_tasks = m;
        }
        if let Ok(level) = std::env::var("DAF_LOG_LEVEL") {
            config.logger.level = level;
        }
        if let Ok(backend) = std::env::var("DAF_MEMORY_BACKEND") {
            config.memory.backend = backend;
        }
        if let Ok(dir) = std::env::var("DAF_MEMORY_DIR") {
            config.memory.data_dir = Some(PathBuf::from(dir));
        }

        config.validate()?;
        Ok(config)
    }

    /// Start building a config with the builder API.
    pub fn builder() -> SdkConfigBuilder {
        SdkConfigBuilder::new()
    }

    /// Validate the configuration, returning an error with a helpful message
    /// if anything is misconfigured.
    pub fn validate(&self) -> DafResult<()> {
        if self.registry_url.is_empty() {
            return Err(DafError::ConfigError(
                "registry_url must not be empty".into(),
            ));
        }

        if self.max_concurrent_tasks == 0 {
            return Err(DafError::ConfigError(
                "max_concurrent_tasks must be at least 1".into(),
            ));
        }

        if self.heartbeat_interval.is_zero() {
            return Err(DafError::ConfigError(
                "heartbeat_interval must be greater than zero".into(),
            ));
        }

        if self.transport.tls_enabled && self.transport.tls_cert_path.is_none() {
            return Err(DafError::ConfigError(
                "tls_cert_path is required when TLS is enabled".into(),
            ));
        }

        if self.transport.tls_enabled && self.transport.tls_key_path.is_none() {
            return Err(DafError::ConfigError(
                "tls_key_path is required when TLS is enabled".into(),
            ));
        }

        let valid_backends = ["in_memory", "sled", "rocksdb"];
        if !valid_backends.contains(&self.memory.backend.as_str()) {
            return Err(DafError::ConfigError(format!(
                "unknown memory backend '{}', expected one of: {}",
                self.memory.backend,
                valid_backends.join(", "),
            )));
        }

        if (self.memory.backend == "sled" || self.memory.backend == "rocksdb")
            && self.memory.data_dir.is_none()
        {
            return Err(DafError::ConfigError(format!(
                "data_dir is required for the '{}' memory backend",
                self.memory.backend,
            )));
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SdkConfigBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for [`SdkConfig`].
#[derive(Debug, Clone)]
pub struct SdkConfigBuilder {
    config: SdkConfig,
}

impl SdkConfigBuilder {
    /// Start with development defaults.
    pub fn new() -> Self {
        Self {
            config: SdkConfig::development(),
        }
    }

    /// Set the deployment environment.
    pub fn environment(mut self, env: Environment) -> Self {
        self.config.environment = env;
        self
    }

    /// Set the transport bind/connect address.
    pub fn transport_address(mut self, addr: impl Into<String>) -> Self {
        self.config.transport.address = addr.into();
        self
    }

    /// Enable or disable TLS.
    pub fn tls_enabled(mut self, enabled: bool) -> Self {
        self.config.transport.tls_enabled = enabled;
        self
    }

    /// Set TLS certificate and key paths.
    pub fn tls_paths(mut self, cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.config.transport.tls_cert_path = Some(cert.into());
        self.config.transport.tls_key_path = Some(key.into());
        self
    }

    /// Set the registry service URL.
    pub fn registry_url(mut self, url: impl Into<String>) -> Self {
        self.config.registry_url = url.into();
        self
    }

    /// Set the heartbeat interval in seconds.
    pub fn heartbeat_interval_secs(mut self, secs: u64) -> Self {
        self.config.heartbeat_interval = Duration::from_secs(secs);
        self
    }

    /// Set the maximum concurrent tasks.
    pub fn max_concurrent_tasks(mut self, max: u32) -> Self {
        self.config.max_concurrent_tasks = max;
        self
    }

    /// Set the log level.
    pub fn log_level(mut self, level: impl Into<String>) -> Self {
        self.config.logger.level = level.into();
        self
    }

    /// Set the memory backend.
    pub fn memory_backend(mut self, backend: impl Into<String>) -> Self {
        self.config.memory.backend = backend.into();
        self
    }

    /// Set the memory data directory.
    pub fn memory_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.memory.data_dir = Some(dir.into());
        self
    }

    /// Set the shutdown timeout.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout = timeout;
        self
    }

    /// Replace the entire transport config.
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.config.transport = config;
        self
    }

    /// Replace the entire logger config.
    pub fn logger_config(mut self, config: LoggerConfig) -> Self {
        self.config.logger = config;
        self
    }

    /// Replace the entire memory config.
    pub fn memory_config(mut self, config: MemoryConfig) -> Self {
        self.config.memory = config;
        self
    }

    /// Validate and build the configuration.
    pub fn build(self) -> DafResult<SdkConfig> {
        self.config.validate()?;
        Ok(self.config)
    }
}

impl Default for SdkConfigBuilder {
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
    fn development_config_validates() {
        let config = SdkConfig::development();
        config.validate().unwrap();
    }

    #[test]
    fn test_config_validates() {
        let config = SdkConfig::test();
        config.validate().unwrap();
    }

    #[test]
    fn production_config_needs_tls_paths() {
        let config = SdkConfig::production();
        // Production enables TLS but doesn't set cert paths — should fail.
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("tls_cert_path"));
    }

    #[test]
    fn production_config_with_tls_paths_validates() {
        let mut config = SdkConfig::production();
        config.transport.tls_cert_path = Some("/etc/daf/cert.pem".into());
        config.transport.tls_key_path = Some("/etc/daf/key.pem".into());
        config.validate().unwrap();
    }

    #[test]
    fn builder_produces_valid_config() {
        let config = SdkConfig::builder()
            .registry_url("http://localhost:8500")
            .max_concurrent_tasks(16)
            .heartbeat_interval_secs(15)
            .log_level("debug")
            .build()
            .unwrap();

        assert_eq!(config.max_concurrent_tasks, 16);
        assert_eq!(config.heartbeat_interval, Duration::from_secs(15));
        assert_eq!(config.logger.level, "debug");
    }

    #[test]
    fn builder_rejects_zero_tasks() {
        let result = SdkConfig::builder()
            .max_concurrent_tasks(0)
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_concurrent_tasks"));
    }

    #[test]
    fn builder_rejects_empty_registry() {
        let result = SdkConfig::builder()
            .registry_url("")
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_rejects_unknown_memory_backend() {
        let result = SdkConfig::builder()
            .memory_backend("postgres")
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("postgres"));
    }

    #[test]
    fn sled_backend_requires_data_dir() {
        let result = SdkConfig::builder()
            .memory_backend("sled")
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("data_dir"));
    }

    #[test]
    fn sled_backend_with_data_dir_validates() {
        let config = SdkConfig::builder()
            .memory_backend("sled")
            .memory_data_dir("/tmp/daf-test")
            .build()
            .unwrap();
        assert_eq!(config.memory.backend, "sled");
    }

    #[test]
    fn config_serialization_roundtrip() {
        let config = SdkConfig::development();
        let json = serde_json::to_string(&config).unwrap();
        let back: SdkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.registry_url, config.registry_url);
        assert_eq!(back.max_concurrent_tasks, config.max_concurrent_tasks);
    }

    #[test]
    fn environment_display() {
        assert_eq!(Environment::Production.to_string(), "production");
        assert_eq!(Environment::Development.to_string(), "development");
        assert_eq!(Environment::Test.to_string(), "test");
        assert_eq!(Environment::Staging.to_string(), "staging");
    }
}
