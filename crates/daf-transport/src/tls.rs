//! TLS transport layer wrapping TCP with rustls.
//!
//! Provides [`TlsTransport`] for establishing TLS-encrypted connections.
//! Uses `rustls` — no OpenSSL dependency. Supports mutual TLS (mTLS)
//! when `verify_peer` is enabled and a CA certificate is provided.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::connection::{Connection, ConnectionInfo, ConnectionState};
use crate::error::{TransportError, TransportResult};

// ---------------------------------------------------------------------------
// TLS config
// ---------------------------------------------------------------------------

/// TLS configuration for certificate-based encryption.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to the PEM-encoded certificate chain.
    pub cert_path: String,
    /// Path to the PEM-encoded private key.
    pub key_path: String,
    /// Optional path to the CA certificate for peer verification.
    pub ca_path: Option<String>,
    /// Whether to require and verify the peer's certificate.
    pub verify_peer: bool,
    /// Server name for SNI (client-side only).
    pub server_name: Option<String>,
    /// Connect timeout for TLS handshake.
    pub handshake_timeout: Duration,
}

impl TlsConfig {
    /// Create a minimal TLS config with cert and key paths.
    pub fn new(cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        Self {
            cert_path: cert_path.into(),
            key_path: key_path.into(),
            ca_path: None,
            verify_peer: false,
            server_name: None,
            handshake_timeout: Duration::from_secs(10),
        }
    }

    /// Enable mutual TLS with a CA certificate.
    pub fn with_ca(mut self, ca_path: impl Into<String>) -> Self {
        self.ca_path = Some(ca_path.into());
        self.verify_peer = true;
        self
    }

    /// Set the server name for SNI.
    pub fn with_server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Certificate loading
// ---------------------------------------------------------------------------

/// Load PEM-encoded certificates from a file.
fn load_certs(path: &str) -> TransportResult<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path).map_err(|e| TransportError::TlsError {
        message: format!("cannot open cert file {path}: {e}"),
    })?;
    let mut reader = std::io::BufReader::new(file);

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TransportError::TlsError {
            message: format!("failed to parse certs from {path}: {e}"),
        })?;

    if certs.is_empty() {
        return Err(TransportError::TlsError {
            message: format!("no certificates found in {path}"),
        });
    }

    Ok(certs)
}

/// Load a PEM-encoded private key from a file.
fn load_key(path: &str) -> TransportResult<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path).map_err(|e| TransportError::TlsError {
        message: format!("cannot open key file {path}: {e}"),
    })?;
    let mut reader = std::io::BufReader::new(file);

    let key = rustls_pemfile::private_key(&mut reader)
        .map_err(|e| TransportError::TlsError {
            message: format!("failed to parse key from {path}: {e}"),
        })?
        .ok_or_else(|| TransportError::TlsError {
            message: format!("no private key found in {path}"),
        })?;

    Ok(key)
}

// ---------------------------------------------------------------------------
// TLS connection
// ---------------------------------------------------------------------------

/// A TLS-encrypted connection wrapping a TCP stream.
///
/// This is generic over the TLS stream direction (client vs server).
pub struct TlsConnection {
    /// We store the stream as an enum to handle both client and server sides.
    stream: TlsStream,
    info: ConnectionInfo,
}

enum TlsStream {
    Client(tokio_rustls::client::TlsStream<tokio::net::TcpStream>),
    Server(tokio_rustls::server::TlsStream<tokio::net::TcpStream>),
}

#[async_trait]
impl Connection for TlsConnection {
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        let n = match &mut self.stream {
            TlsStream::Client(s) => s.read(buf).await,
            TlsStream::Server(s) => s.read(buf).await,
        }
        .map_err(|e| {
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
        match &mut self.stream {
            TlsStream::Client(s) => s.write_all(data).await,
            TlsStream::Server(s) => s.write_all(data).await,
        }
        .map_err(|e| {
            self.info.state = ConnectionState::Closed;
            TransportError::IoError(e)
        })?;
        self.info.bytes_sent += data.len() as u64;
        Ok(())
    }

    async fn flush(&mut self) -> TransportResult<()> {
        match &mut self.stream {
            TlsStream::Client(s) => s.flush().await?,
            TlsStream::Server(s) => s.flush().await?,
        }
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        self.info.state = ConnectionState::Draining;
        match &mut self.stream {
            TlsStream::Client(s) => s.shutdown().await.ok(),
            TlsStream::Server(s) => s.shutdown().await.ok(),
        };
        self.info.state = ConnectionState::Closed;
        tracing::debug!(id = %self.info.id, "TLS connection closed");
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
// TLS transport
// ---------------------------------------------------------------------------

/// TLS transport providing encrypted connections over TCP.
///
/// Wraps `rustls` for both server-side acceptance and client-side connection.
pub struct TlsTransport {
    config: TlsConfig,
    acceptor: Option<TlsAcceptor>,
    connector: Option<TlsConnector>,
}

impl TlsTransport {
    /// Build a TLS transport from the given configuration.
    ///
    /// Loads certificates and keys eagerly so that misconfiguration is
    /// caught at construction time rather than at the first handshake.
    pub fn new(config: TlsConfig) -> TransportResult<Self> {
        Ok(Self {
            config,
            acceptor: None,
            connector: None,
        })
    }

    /// Initialize the server-side TLS acceptor.
    ///
    /// Must be called before [`accept_tls`](Self::accept_tls).
    pub fn init_server(&mut self) -> TransportResult<()> {
        let certs = load_certs(&self.config.cert_path)?;
        let key = load_key(&self.config.key_path)?;

        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| TransportError::TlsError {
                message: format!("server config error: {e}"),
            })?;

        self.acceptor = Some(TlsAcceptor::from(Arc::new(server_config)));
        tracing::info!("TLS server acceptor initialized");
        Ok(())
    }

    /// Initialize the client-side TLS connector.
    ///
    /// Must be called before [`connect_tls`](Self::connect_tls).
    pub fn init_client(&mut self) -> TransportResult<()> {
        let mut root_store = rustls::RootCertStore::empty();

        if let Some(ca_path) = &self.config.ca_path {
            let ca_certs = load_certs(ca_path)?;
            for cert in ca_certs {
                root_store.add(cert).map_err(|e| TransportError::TlsError {
                    message: format!("failed to add CA cert: {e}"),
                })?;
            }
        }

        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        self.connector = Some(TlsConnector::from(Arc::new(client_config)));
        tracing::info!("TLS client connector initialized");
        Ok(())
    }

    /// Perform a TLS handshake on an already-accepted TCP stream (server side).
    pub async fn accept_tls(
        &self,
        tcp_stream: tokio::net::TcpStream,
    ) -> TransportResult<TlsConnection> {
        let acceptor = self.acceptor.as_ref().ok_or_else(|| TransportError::TlsError {
            message: "server acceptor not initialized; call init_server() first".into(),
        })?;

        let peer = tcp_stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".into());

        let tls_stream = tokio::time::timeout(
            self.config.handshake_timeout,
            acceptor.accept(tcp_stream),
        )
        .await
        .map_err(|_| TransportError::Timeout {
            operation: "TLS server handshake".into(),
            duration: self.config.handshake_timeout,
        })?
        .map_err(|e| TransportError::TlsError {
            message: format!("server handshake failed: {e}"),
        })?;

        let mut info = ConnectionInfo::new(Some(peer));
        info.state = ConnectionState::Connected;

        tracing::debug!(id = %info.id, "TLS server handshake complete");
        Ok(TlsConnection {
            stream: TlsStream::Server(tls_stream),
            info,
        })
    }

    /// Connect to a remote TLS server over TCP (client side).
    pub async fn connect_tls(&self, addr: &str) -> TransportResult<TlsConnection> {
        let connector = self.connector.as_ref().ok_or_else(|| TransportError::TlsError {
            message: "client connector not initialized; call init_client() first".into(),
        })?;

        let tcp_stream = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused => TransportError::ConnectionRefused {
                    address: addr.to_string(),
                },
                _ => TransportError::IoError(e),
            })?;

        tcp_stream.set_nodelay(true)?;

        let server_name = self
            .config
            .server_name
            .as_deref()
            .unwrap_or("localhost");

        let domain = ServerName::try_from(server_name.to_string()).map_err(|e| {
            TransportError::TlsError {
                message: format!("invalid server name '{server_name}': {e}"),
            }
        })?;

        let tls_stream = tokio::time::timeout(
            self.config.handshake_timeout,
            connector.connect(domain, tcp_stream),
        )
        .await
        .map_err(|_| TransportError::Timeout {
            operation: format!("TLS client handshake to {addr}"),
            duration: self.config.handshake_timeout,
        })?
        .map_err(|e| TransportError::TlsError {
            message: format!("client handshake to {addr} failed: {e}"),
        })?;

        let mut info = ConnectionInfo::new(Some(addr.to_string()));
        info.state = ConnectionState::Connected;

        tracing::debug!(id = %info.id, address = %addr, "TLS client handshake complete");
        Ok(TlsConnection {
            stream: TlsStream::Client(tls_stream),
            info,
        })
    }

    /// Return the underlying TLS configuration.
    pub fn config(&self) -> &TlsConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_config_builder() {
        let cfg = TlsConfig::new("/path/to/cert.pem", "/path/to/key.pem")
            .with_ca("/path/to/ca.pem")
            .with_server_name("example.com");

        assert_eq!(cfg.cert_path, "/path/to/cert.pem");
        assert_eq!(cfg.key_path, "/path/to/key.pem");
        assert_eq!(cfg.ca_path.as_deref(), Some("/path/to/ca.pem"));
        assert!(cfg.verify_peer);
        assert_eq!(cfg.server_name.as_deref(), Some("example.com"));
    }

    #[test]
    fn load_certs_missing_file() {
        let result = load_certs("/nonexistent/cert.pem");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, TransportError::TlsError { .. }));
    }

    #[test]
    fn load_key_missing_file() {
        let result = load_key("/nonexistent/key.pem");
        assert!(result.is_err());
    }

    #[test]
    fn tls_transport_lazy_init() {
        let cfg = TlsConfig::new("/dev/null", "/dev/null");
        let transport = TlsTransport::new(cfg);
        assert!(transport.is_ok());
    }
}
