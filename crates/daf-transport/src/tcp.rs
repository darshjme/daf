//! TCP transport implementation.
//!
//! Provides [`TcpTransport`] for establishing and managing TCP connections
//! with Nagle disabled, keepalive support, and optional connection pooling.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::connection::{Connection, ConnectionInfo, ConnectionState};
use crate::error::{TransportError, TransportResult};
use crate::listener::{DafTcpListener, Listener, ListenerConfig};
use crate::pool::ConnectionPool;

// ---------------------------------------------------------------------------
// TCP transport config
// ---------------------------------------------------------------------------

/// Configuration for the TCP transport layer.
#[derive(Debug, Clone)]
pub struct TcpTransportConfig {
    /// Bind address for listeners (e.g. `"0.0.0.0:9000"`).
    pub bind_address: String,
    /// Whether to disable Nagle's algorithm (`TCP_NODELAY = true`).
    pub no_delay: bool,
    /// TCP keepalive interval. `None` disables keepalive.
    pub keepalive: Option<Duration>,
    /// Connection pool capacity. `0` disables pooling.
    pub pool_size: usize,
    /// Connect timeout for outbound dials.
    pub connect_timeout: Duration,
}

impl Default for TcpTransportConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:0".into(),
            no_delay: true,
            keepalive: Some(Duration::from_secs(30)),
            pool_size: 64,
            connect_timeout: Duration::from_secs(10),
        }
    }
}

// ---------------------------------------------------------------------------
// TCP connection
// ---------------------------------------------------------------------------

/// A single TCP connection implementing the [`Connection`] trait.
pub struct TcpConnection {
    stream: TcpStream,
    info: ConnectionInfo,
}

impl TcpConnection {
    /// Wrap an already-accepted or already-connected TCP stream.
    pub fn from_stream(stream: TcpStream, peer_addr: String) -> Self {
        let mut info = ConnectionInfo::new(Some(peer_addr));
        info.state = ConnectionState::Connected;
        Self { stream, info }
    }

    /// Dial a remote address and return a connected `TcpConnection`.
    pub async fn connect(addr: &str, config: &TcpTransportConfig) -> TransportResult<Self> {
        let stream = tokio::time::timeout(config.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| TransportError::Timeout {
                operation: format!("TCP connect to {addr}"),
                duration: config.connect_timeout,
            })?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused => TransportError::ConnectionRefused {
                    address: addr.to_string(),
                },
                _ => TransportError::IoError(e),
            })?;

        if config.no_delay {
            stream.set_nodelay(true)?;
        }

        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| addr.to_string());

        let mut info = ConnectionInfo::new(Some(peer));
        info.state = ConnectionState::Connected;

        tracing::debug!(address = %addr, "TCP connection established");
        Ok(Self { stream, info })
    }
}

#[async_trait]
impl Connection for TcpConnection {
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        let n = self.stream.read(buf).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::ConnectionReset {
                self.info.state = ConnectionState::Closed;
                TransportError::ConnectionReset {
                    address: self.info.peer_addr.clone().unwrap_or_default(),
                }
            } else {
                TransportError::IoError(e)
            }
        })?;
        self.info.bytes_received += n as u64;
        if n == 0 {
            self.info.state = ConnectionState::Closed;
        }
        Ok(n)
    }

    async fn write(&mut self, data: &[u8]) -> TransportResult<()> {
        self.stream.write_all(data).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::ConnectionReset
                || e.kind() == std::io::ErrorKind::BrokenPipe
            {
                self.info.state = ConnectionState::Closed;
                TransportError::ConnectionReset {
                    address: self.info.peer_addr.clone().unwrap_or_default(),
                }
            } else {
                TransportError::IoError(e)
            }
        })?;
        self.info.bytes_sent += data.len() as u64;
        Ok(())
    }

    async fn flush(&mut self) -> TransportResult<()> {
        self.stream.flush().await?;
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        self.info.state = ConnectionState::Draining;
        self.stream.shutdown().await.ok();
        self.info.state = ConnectionState::Closed;
        tracing::debug!(id = %self.info.id, "TCP connection closed");
        Ok(())
    }

    fn peer_addr(&self) -> Option<String> {
        self.info.peer_addr.clone()
    }

    fn is_alive(&self) -> bool {
        self.info.state == ConnectionState::Connected
    }

    fn info(&self) -> &ConnectionInfo {
        &self.info
    }
}

// ---------------------------------------------------------------------------
// TCP transport
// ---------------------------------------------------------------------------

/// High-level TCP transport: manages a listener and an outbound connection pool.
pub struct TcpTransport {
    config: TcpTransportConfig,
    listener: Option<DafTcpListener>,
    pool: Option<Arc<ConnectionPool<TcpConnection>>>,
}

impl TcpTransport {
    /// Create a new TCP transport with the given configuration.
    /// Call [`listen`](Self::listen) to start accepting connections.
    pub fn new(config: TcpTransportConfig) -> Self {
        Self {
            config,
            listener: None,
            pool: None,
        }
    }

    /// Bind and start listening for inbound connections.
    pub async fn listen(&mut self) -> TransportResult<String> {
        let cfg = ListenerConfig::new(&self.config.bind_address);
        let listener = DafTcpListener::bind(cfg).await?;
        let addr = listener.local_addr()?;
        self.listener = Some(listener);

        if self.config.pool_size > 0 {
            let pool_cfg = crate::pool::PoolConfig {
                min_connections: 0,
                max_connections: self.config.pool_size,
                idle_timeout: Duration::from_secs(300),
                max_lifetime: Duration::from_secs(3600),
                health_check_interval: Duration::from_secs(30),
            };
            self.pool = Some(Arc::new(ConnectionPool::new(pool_cfg)));
        }

        Ok(addr)
    }

    /// Accept the next inbound connection from the listener.
    pub async fn accept(&self) -> TransportResult<TcpConnection> {
        let listener = self.listener.as_ref().ok_or_else(|| {
            TransportError::InvalidAddress {
                address: self.config.bind_address.clone(),
                reason: "listener not started; call listen() first".into(),
            }
        })?;
        listener.accept().await
    }

    /// Dial a remote address and return a connection.
    pub async fn connect(&self, addr: &str) -> TransportResult<TcpConnection> {
        TcpConnection::connect(addr, &self.config).await
    }

    /// Shut down the listener.
    pub async fn shutdown(&self) -> TransportResult<()> {
        if let Some(listener) = &self.listener {
            listener.shutdown().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tcp_connection_roundtrip() {
        let mut transport = TcpTransport::new(TcpTransportConfig {
            bind_address: "127.0.0.1:0".into(),
            pool_size: 0,
            ..Default::default()
        });

        let addr = transport.listen().await.unwrap();

        let handle = tokio::spawn(async move {
            let mut conn = transport.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            conn.write(&buf[..n]).await.unwrap();
            conn.flush().await.unwrap();
            conn.close().await.unwrap();
            assert!(!conn.is_alive());
        });

        let config = TcpTransportConfig::default();
        let mut client = TcpConnection::connect(&addr, &config).await.unwrap();
        assert!(client.is_alive());
        assert!(client.peer_addr().is_some());

        client.write(b"hello").await.unwrap();
        client.flush().await.unwrap();

        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");

        let info = client.info();
        assert_eq!(info.bytes_sent, 5);
        assert_eq!(info.bytes_received, 5);

        client.close().await.unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_nodelay_default() {
        let config = TcpTransportConfig::default();
        assert!(config.no_delay);
        assert_eq!(config.pool_size, 64);
    }
}
