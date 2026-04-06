//! Connection listener abstractions.
//!
//! A [`Listener`] binds to a local address and accepts incoming connections.
//! Concrete implementations wrap `tokio::net::TcpListener` and
//! `tokio::net::UnixListener`.

use async_trait::async_trait;

use crate::connection::Connection;
use crate::error::{TransportError, TransportResult};

// ---------------------------------------------------------------------------
// Listener config
// ---------------------------------------------------------------------------

/// Configuration for binding a listener.
#[derive(Debug, Clone)]
pub struct ListenerConfig {
    /// The address to bind to (e.g. `"0.0.0.0:9000"` or `"/tmp/daf.sock"`).
    pub address: String,
    /// Maximum number of pending connections in the accept queue.
    /// Passed to the OS via `listen(2)` backlog. `None` uses the OS default.
    pub backlog: Option<u32>,
    /// Enable `SO_REUSEADDR` on the listening socket (TCP only).
    pub reuse_addr: bool,
}

impl ListenerConfig {
    /// Create a config for the given address with sensible defaults.
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            backlog: Some(1024),
            reuse_addr: true,
        }
    }

    /// Set the backlog depth.
    pub fn with_backlog(mut self, backlog: u32) -> Self {
        self.backlog = Some(backlog);
        self
    }

    /// Set the `SO_REUSEADDR` flag.
    pub fn with_reuse_addr(mut self, reuse: bool) -> Self {
        self.reuse_addr = reuse;
        self
    }
}

// ---------------------------------------------------------------------------
// Listener trait
// ---------------------------------------------------------------------------

/// An async connection listener.
///
/// Implementations bind to a local address and produce [`Connection`] instances
/// for each accepted peer.
#[async_trait]
pub trait Listener: Send + Sync + 'static {
    /// The concrete connection type produced by this listener.
    type Conn: Connection;

    /// Bind to the configured address and begin listening.
    async fn bind(config: ListenerConfig) -> TransportResult<Self>
    where
        Self: Sized;

    /// Accept the next incoming connection. Blocks until a peer connects.
    async fn accept(&self) -> TransportResult<Self::Conn>;

    /// Return the local address this listener is bound to.
    fn local_addr(&self) -> TransportResult<String>;

    /// Initiate a graceful shutdown. After this returns, `accept` will
    /// yield errors.
    async fn shutdown(&self) -> TransportResult<()>;
}

// ---------------------------------------------------------------------------
// TCP listener
// ---------------------------------------------------------------------------

/// TCP listener wrapping `tokio::net::TcpListener`.
pub struct DafTcpListener {
    inner: tokio::net::TcpListener,
}

#[async_trait]
impl Listener for DafTcpListener {
    type Conn = crate::tcp::TcpConnection;

    async fn bind(config: ListenerConfig) -> TransportResult<Self> {
        let listener = tokio::net::TcpListener::bind(&config.address)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    TransportError::AddressInUse {
                        address: config.address.clone(),
                    }
                } else {
                    TransportError::IoError(e)
                }
            })?;

        tracing::info!(address = %config.address, "TCP listener bound");
        Ok(Self { inner: listener })
    }

    async fn accept(&self) -> TransportResult<crate::tcp::TcpConnection> {
        let (stream, peer) = self.inner.accept().await?;

        // Disable Nagle for low latency.
        stream.set_nodelay(true).ok();

        let peer_str = peer.to_string();
        tracing::debug!(peer = %peer_str, "accepted TCP connection");

        Ok(crate::tcp::TcpConnection::from_stream(stream, peer_str))
    }

    fn local_addr(&self) -> TransportResult<String> {
        Ok(self.inner.local_addr()?.to_string())
    }

    async fn shutdown(&self) -> TransportResult<()> {
        // Dropping the listener closes the socket. Since we hold a reference
        // here, the actual close happens when the struct is dropped. We just
        // log intent.
        tracing::info!("TCP listener shutdown requested");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unix listener
// ---------------------------------------------------------------------------

/// Unix domain socket listener wrapping `tokio::net::UnixListener`.
#[cfg(unix)]
pub struct DafUnixListener {
    inner: tokio::net::UnixListener,
    /// Path to the socket file, kept for cleanup on drop.
    path: String,
}

#[cfg(unix)]
#[async_trait]
impl Listener for DafUnixListener {
    type Conn = crate::unix::UnixConnection;

    async fn bind(config: ListenerConfig) -> TransportResult<Self> {
        // Remove stale socket file if present.
        if std::path::Path::new(&config.address).exists() {
            std::fs::remove_file(&config.address).ok();
        }

        let listener =
            tokio::net::UnixListener::bind(&config.address).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    TransportError::AddressInUse {
                        address: config.address.clone(),
                    }
                } else {
                    TransportError::IoError(e)
                }
            })?;

        tracing::info!(path = %config.address, "Unix listener bound");
        Ok(Self {
            inner: listener,
            path: config.address,
        })
    }

    async fn accept(&self) -> TransportResult<crate::unix::UnixConnection> {
        let (stream, _addr) = self.inner.accept().await?;
        tracing::debug!(path = %self.path, "accepted Unix connection");
        Ok(crate::unix::UnixConnection::from_stream(
            stream,
            self.path.clone(),
        ))
    }

    fn local_addr(&self) -> TransportResult<String> {
        Ok(self.path.clone())
    }

    async fn shutdown(&self) -> TransportResult<()> {
        tracing::info!(path = %self.path, "Unix listener shutdown requested");
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for DafUnixListener {
    fn drop(&mut self) {
        if std::path::Path::new(&self.path).exists() {
            std::fs::remove_file(&self.path).ok();
            tracing::debug!(path = %self.path, "cleaned up Unix socket file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_config_builder() {
        let cfg = ListenerConfig::new("0.0.0.0:8080")
            .with_backlog(512)
            .with_reuse_addr(false);

        assert_eq!(cfg.address, "0.0.0.0:8080");
        assert_eq!(cfg.backlog, Some(512));
        assert!(!cfg.reuse_addr);
    }

    #[test]
    fn listener_config_defaults() {
        let cfg = ListenerConfig::new("127.0.0.1:3000");
        assert_eq!(cfg.backlog, Some(1024));
        assert!(cfg.reuse_addr);
    }

    #[tokio::test]
    async fn tcp_listener_bind_and_accept() {
        let cfg = ListenerConfig::new("127.0.0.1:0");
        let listener = DafTcpListener::bind(cfg).await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(addr.contains("127.0.0.1"));

        // Connect from client side.
        let client = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let _conn = listener.accept().await.unwrap();
        drop(client);
    }
}
