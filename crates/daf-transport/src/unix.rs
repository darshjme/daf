//! Unix domain socket transport.
//!
//! Provides [`UnixTransport`] and [`UnixConnection`] for same-host IPC
//! over Unix domain sockets. The transport manages socket file creation
//! and cleanup automatically.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::connection::{Connection, ConnectionInfo, ConnectionState};
use crate::error::{TransportError, TransportResult};
use crate::listener::{DafUnixListener, Listener, ListenerConfig};

// ---------------------------------------------------------------------------
// Unix transport config
// ---------------------------------------------------------------------------

/// Configuration for the Unix domain socket transport.
#[derive(Debug, Clone)]
pub struct UnixTransportConfig {
    /// Path to the socket file (e.g. `"/tmp/daf-agent.sock"`).
    pub socket_path: String,
    /// File permissions for the socket (octal, e.g. `0o660`).
    pub permissions: u32,
    /// Connect timeout for outbound dials.
    pub connect_timeout: Duration,
}

impl Default for UnixTransportConfig {
    fn default() -> Self {
        Self {
            socket_path: "/tmp/daf.sock".into(),
            permissions: 0o660,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

// ---------------------------------------------------------------------------
// Unix connection
// ---------------------------------------------------------------------------

/// A single Unix domain socket connection.
pub struct UnixConnection {
    stream: UnixStream,
    info: ConnectionInfo,
}

impl UnixConnection {
    /// Wrap an already-accepted Unix stream.
    pub fn from_stream(stream: UnixStream, path: String) -> Self {
        let mut info = ConnectionInfo::new(Some(path));
        info.state = ConnectionState::Connected;
        Self { stream, info }
    }

    /// Dial a Unix socket at `path`.
    pub async fn connect(path: &str, config: &UnixTransportConfig) -> TransportResult<Self> {
        let stream = tokio::time::timeout(config.connect_timeout, UnixStream::connect(path))
            .await
            .map_err(|_| TransportError::Timeout {
                operation: format!("Unix connect to {path}"),
                duration: config.connect_timeout,
            })?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused => TransportError::ConnectionRefused {
                    address: path.to_string(),
                },
                std::io::ErrorKind::NotFound => TransportError::InvalidAddress {
                    address: path.to_string(),
                    reason: "socket file does not exist".into(),
                },
                _ => TransportError::IoError(e),
            })?;

        let mut info = ConnectionInfo::new(Some(path.to_string()));
        info.state = ConnectionState::Connected;

        tracing::debug!(path = %path, "Unix connection established");
        Ok(Self { stream, info })
    }
}

#[async_trait]
impl Connection for UnixConnection {
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        let n = self.stream.read(buf).await.map_err(|e| {
            self.info.state = ConnectionState::Closed;
            TransportError::IoError(e)
        })?;
        self.info.bytes_received += n as u64;
        if n == 0 {
            self.info.state = ConnectionState::Closed;
        }
        Ok(n)
    }

    async fn write(&mut self, data: &[u8]) -> TransportResult<()> {
        self.stream.write_all(data).await.map_err(|e| {
            self.info.state = ConnectionState::Closed;
            TransportError::IoError(e)
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
        tracing::debug!(id = %self.info.id, "Unix connection closed");
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
// Unix transport
// ---------------------------------------------------------------------------

/// High-level Unix domain socket transport.
///
/// Manages the listener, socket file lifecycle (creation and cleanup),
/// and file permissions.
pub struct UnixTransport {
    config: UnixTransportConfig,
    listener: Option<DafUnixListener>,
}

impl UnixTransport {
    /// Create a new Unix transport. Call [`listen`](Self::listen) to start.
    pub fn new(config: UnixTransportConfig) -> Self {
        Self {
            config,
            listener: None,
        }
    }

    /// Bind and start listening on the configured socket path.
    pub async fn listen(&mut self) -> TransportResult<String> {
        let cfg = ListenerConfig::new(&self.config.socket_path);
        let listener = DafUnixListener::bind(cfg).await?;

        // Set socket file permissions.
        #[cfg(unix)]
        {
            let perms = std::fs::Permissions::from_mode(self.config.permissions);
            std::fs::set_permissions(&self.config.socket_path, perms).ok();
        }

        let addr = listener.local_addr()?;
        self.listener = Some(listener);
        Ok(addr)
    }

    /// Accept the next inbound connection.
    pub async fn accept(&self) -> TransportResult<UnixConnection> {
        let listener = self.listener.as_ref().ok_or_else(|| {
            TransportError::InvalidAddress {
                address: self.config.socket_path.clone(),
                reason: "listener not started; call listen() first".into(),
            }
        })?;
        listener.accept().await
    }

    /// Dial the configured socket path.
    pub async fn connect(&self) -> TransportResult<UnixConnection> {
        UnixConnection::connect(&self.config.socket_path, &self.config).await
    }

    /// Dial an arbitrary socket path.
    pub async fn connect_to(&self, path: &str) -> TransportResult<UnixConnection> {
        UnixConnection::connect(path, &self.config).await
    }

    /// Shut down the listener and remove the socket file.
    pub async fn shutdown(&mut self) -> TransportResult<()> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().await?;
            // Drop triggers socket file cleanup via DafUnixListener::drop.
        }
        Ok(())
    }

    /// Remove a stale socket file without binding.
    pub fn cleanup_socket(path: &str) {
        if Path::new(path).exists() {
            std::fs::remove_file(path).ok();
            tracing::debug!(path = %path, "removed stale Unix socket");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unix_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let sock_path = sock.to_str().unwrap().to_string();

        let mut transport = UnixTransport::new(UnixTransportConfig {
            socket_path: sock_path.clone(),
            ..Default::default()
        });

        let addr = transport.listen().await.unwrap();
        assert_eq!(addr, sock_path);

        let handle = tokio::spawn(async move {
            let mut conn = transport.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            conn.write(&buf[..n]).await.unwrap();
            conn.flush().await.unwrap();
        });

        let config = UnixTransportConfig::default();
        let mut client = UnixConnection::connect(&sock_path, &config).await.unwrap();
        client.write(b"uds-test").await.unwrap();
        client.flush().await.unwrap();

        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"uds-test");

        let info = client.info();
        assert_eq!(info.bytes_sent, 8);
        assert_eq!(info.bytes_received, 8);

        handle.await.unwrap();
    }

    #[test]
    fn cleanup_nonexistent_is_noop() {
        UnixTransport::cleanup_socket("/tmp/daf_nonexistent_test_12345.sock");
    }
}
