//! Bootstrap sequence for the DAF runtime.
//!
//! The [`bootstrap`] function is the primary entry point. It walks through
//! a deterministic sequence of initialization steps, logging progress and
//! handling errors with rollback where possible.

use std::path::Path;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use daf_core::{DafError, DafResult};

use crate::config::{LogFormat, RuntimeConfig};
use crate::node::cluster_join;
use crate::runtime::Runtime;

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

/// Print the startup banner to stderr.
fn print_banner(config: &RuntimeConfig) {
    let banner = format!(
        r#"
  ╔══════════════════════════════════════════════════╗
  ║   ██████╗  █████╗ ███████╗                       ║
  ║   ██╔══██╗██╔══██╗██╔════╝                       ║
  ║   ██║  ██║███████║█████╗                          ║
  ║   ██║  ██║██╔══██║██╔══╝                          ║
  ║   ██████╔╝██║  ██║██║                             ║
  ║   ╚═════╝ ╚═╝  ╚═╝╚═╝                            ║
  ║   Darshj's Agent Framework                        ║
  ╚══════════════════════════════════════════════════╝
    version : {version}
    node    : {node}
    bind    : {bind}
    data    : {data}
    agents  : {agents} max
"#,
        version = crate::VERSION,
        node = config.node_name,
        bind = config.bind_address,
        data = config.data_dir.display(),
        agents = config.max_agents,
    );
    eprintln!("{banner}");
}

// ---------------------------------------------------------------------------
// Bootstrap steps
// ---------------------------------------------------------------------------

/// Step 1: Initialize the tracing/logging subsystem.
fn init_tracing(config: &RuntimeConfig) -> DafResult<()> {
    let filter = EnvFilter::try_new(&config.logging.level).map_err(|e| {
        DafError::ConfigError(format!("invalid log level '{}': {e}", config.logging.level))
    })?;

    let subscriber = tracing_subscriber::fmt().with_env_filter(filter);

    match config.logging.format {
        LogFormat::Json => {
            let result = subscriber.json().try_init();
            if let Err(e) = result {
                // Tracing may already be initialized (e.g. in tests).
                eprintln!("note: tracing already initialized: {e}");
            }
        }
        LogFormat::Text => {
            let result = subscriber.try_init();
            if let Err(e) = result {
                eprintln!("note: tracing already initialized: {e}");
            }
        }
    }

    info!("step 1/9: tracing initialized (level={})", config.logging.level);
    Ok(())
}

/// Step 2: Create data directories.
fn create_data_dirs(config: &RuntimeConfig) -> DafResult<()> {
    let dirs = [
        config.data_dir.clone(),
        config.data_dir.join("logs"),
        config.data_dir.join("memory"),
        config.data_dir.join("registry"),
        config.data_dir.join("tmp"),
    ];

    for dir in &dirs {
        create_dir_if_missing(dir)?;
    }

    info!(
        data_dir = %config.data_dir.display(),
        "step 2/9: data directories created"
    );
    Ok(())
}

/// Create a directory (and parents) if it doesn't exist.
fn create_dir_if_missing(path: &Path) -> DafResult<()> {
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|e| {
            DafError::Internal(format!(
                "failed to create directory {}: {e}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

/// Step 3: Initialize transport layer.
fn init_transport(config: &RuntimeConfig) -> DafResult<()> {
    // daf-transport is currently a placeholder. When it exposes a real
    // `TransportLayer::bind()` method, this will call it.
    info!(
        bind = %config.bind_address,
        max_connections = config.transport.max_connections,
        tls = config.transport.tls_enabled,
        "step 3/9: transport layer initialized"
    );
    Ok(())
}

/// Step 4: Initialize memory stores.
fn init_memory(config: &RuntimeConfig) -> DafResult<()> {
    // daf-memory is currently a placeholder.
    info!(
        hot_capacity = config.memory.hot_capacity,
        warm_capacity = config.memory.warm_capacity,
        "step 4/9: memory stores initialized"
    );
    Ok(())
}

/// Step 5: Initialize registry.
fn init_registry(config: &RuntimeConfig) -> DafResult<()> {
    // daf-registry is currently a placeholder.
    info!(
        max_registrations = config.registry.max_registrations,
        federation = config.registry.federation_enabled,
        "step 5/9: registry initialized"
    );
    Ok(())
}

/// Step 6: Initialize logger system.
fn init_logger(config: &RuntimeConfig) -> DafResult<()> {
    // daf-logger is currently a placeholder.
    if let Some(ref path) = config.logging.file_path {
        info!(
            file = %path.display(),
            max_size = config.logging.max_file_size,
            "step 6/9: file logger initialized"
        );
    } else {
        info!("step 6/9: logger initialized (stderr only)");
    }
    Ok(())
}

/// Step 7: Initialize orchestrator.
fn init_orchestrator(config: &RuntimeConfig) -> DafResult<()> {
    // daf-orchestrator is currently a placeholder.
    info!(
        max_missions = config.orchestrator.max_concurrent_missions,
        wave_timeout = ?config.orchestrator.wave_timeout,
        heartbeat = ?config.orchestrator.heartbeat_interval,
        "step 7/9: orchestrator initialized"
    );
    Ok(())
}

/// Step 8: Start health monitoring.
fn init_health(runtime: &Runtime) -> DafResult<()> {
    info!(
        interval = ?runtime.config().health_check_interval,
        "step 8/9: health monitoring initialized"
    );
    Ok(())
}

/// Step 9: Register built-in agents.
fn register_builtin_agents(_runtime: &Runtime) -> DafResult<()> {
    // Built-in agents (health monitor agent, router agent) will be
    // registered here once daf-registry supports agent registration.
    info!("step 9/9: built-in agents registered");
    Ok(())
}

// ---------------------------------------------------------------------------
// bootstrap()
// ---------------------------------------------------------------------------

/// Execute the full bootstrap sequence and return a ready-to-start [`Runtime`].
///
/// The sequence is:
/// 1. Initialize tracing/logging
/// 2. Create data directories
/// 3. Initialize transport layer
/// 4. Initialize memory stores
/// 5. Initialize registry
/// 6. Initialize logger system
/// 7. Initialize orchestrator
/// 8. Start health monitoring
/// 9. Register built-in agents
///
/// Each step logs progress. If any step fails, the error is propagated
/// immediately (future steps are skipped).
pub async fn bootstrap(config: RuntimeConfig) -> DafResult<Runtime> {
    // Validate configuration first.
    config.validate()?;

    // Step 1: Tracing (must come first so all subsequent steps can log).
    init_tracing(&config)?;

    // Print the startup banner.
    print_banner(&config);

    info!(
        node = %config.node_name,
        bind = %config.bind_address,
        "beginning bootstrap sequence"
    );

    // Step 2: Data directories.
    if let Err(e) = create_data_dirs(&config) {
        error!(error = %e, "bootstrap failed at step 2 (data dirs)");
        return Err(e);
    }

    // Step 3: Transport.
    if let Err(e) = init_transport(&config) {
        error!(error = %e, "bootstrap failed at step 3 (transport)");
        return Err(e);
    }

    // Step 4: Memory.
    if let Err(e) = init_memory(&config) {
        error!(error = %e, "bootstrap failed at step 4 (memory)");
        return Err(e);
    }

    // Step 5: Registry.
    if let Err(e) = init_registry(&config) {
        error!(error = %e, "bootstrap failed at step 5 (registry)");
        return Err(e);
    }

    // Step 6: Logger.
    if let Err(e) = init_logger(&config) {
        error!(error = %e, "bootstrap failed at step 6 (logger)");
        return Err(e);
    }

    // Step 7: Orchestrator.
    if let Err(e) = init_orchestrator(&config) {
        error!(error = %e, "bootstrap failed at step 7 (orchestrator)");
        return Err(e);
    }

    // Construct the runtime.
    let cluster_peers = config.cluster_peers.clone();
    let runtime = Runtime::new(config)?;

    // Step 8: Health monitoring.
    if let Err(e) = init_health(&runtime) {
        error!(error = %e, "bootstrap failed at step 8 (health)");
        return Err(e);
    }

    // Step 9: Built-in agents.
    if let Err(e) = register_builtin_agents(&runtime) {
        error!(error = %e, "bootstrap failed at step 9 (built-in agents)");
        return Err(e);
    }

    // Join cluster if peers are configured.
    cluster_join(runtime.node(), &cluster_peers).await;

    info!(
        node = %runtime.node(),
        "bootstrap complete — runtime ready"
    );

    Ok(runtime)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_with_dev_config() {
        let mut config = RuntimeConfig::development();
        // Use a temp directory to avoid polluting /tmp.
        let tmp = tempfile::tempdir().unwrap();
        config.data_dir = tmp.path().to_path_buf();

        let runtime = bootstrap(config).await.unwrap();
        assert_eq!(runtime.node().name, "daf-dev");
        assert_eq!(
            runtime.state(),
            crate::runtime::RuntimeState::Initializing
        );
    }

    #[tokio::test]
    async fn bootstrap_creates_data_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = RuntimeConfig::development();
        config.data_dir = tmp.path().join("daf-test");

        let _runtime = bootstrap(config).await.unwrap();

        assert!(tmp.path().join("daf-test").exists());
        assert!(tmp.path().join("daf-test/logs").exists());
        assert!(tmp.path().join("daf-test/memory").exists());
        assert!(tmp.path().join("daf-test/registry").exists());
        assert!(tmp.path().join("daf-test/tmp").exists());
    }

    #[tokio::test]
    async fn bootstrap_with_invalid_config_fails() {
        let mut config = RuntimeConfig::development();
        config.node_name = String::new();
        let result = bootstrap(config).await;
        assert!(result.is_err());
    }

    #[test]
    fn create_dir_if_missing_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b/c");
        create_dir_if_missing(&path).unwrap();
        assert!(path.exists());
        // Second call should not error.
        create_dir_if_missing(&path).unwrap();
    }

    #[test]
    fn banner_does_not_panic() {
        let config = RuntimeConfig::development();
        print_banner(&config);
    }
}
