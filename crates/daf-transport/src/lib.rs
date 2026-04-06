//! # daf-transport
//!
//! The physical transport layer for the DAF agent framework.
//!
//! This crate sits below DDAL and provides raw byte-stream transport over
//! multiple backends:
//!
//! | Backend | Module | Use case |
//! |---------|--------|----------|
//! | TCP | [`tcp`] | Cross-host agent communication |
//! | Unix | [`unix`] | Same-host IPC with lower overhead |
//! | TLS | [`tls`] | Encrypted TCP (rustls, no OpenSSL) |
//! | InProc | [`inproc`] | Same-process agents, zero-copy channels |
//!
//! All transports implement the [`Connection`] trait from the [`connection`]
//! module, providing a uniform interface for reading, writing, and lifecycle
//! management.
//!
//! ## Connection pooling
//!
//! The [`pool`] module provides a generic [`ConnectionPool`](pool::ConnectionPool)
//! with configurable limits, idle eviction, max lifetime, periodic health
//! checks, and round-robin selection.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use daf_transport::tcp::{TcpTransport, TcpTransportConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut transport = TcpTransport::new(TcpTransportConfig {
//!     bind_address: "0.0.0.0:9000".into(),
//!     ..Default::default()
//! });
//! let addr = transport.listen().await?;
//! println!("listening on {addr}");
//! # Ok(())
//! # }
//! ```

pub mod connection;
pub mod error;
pub mod inproc;
pub mod listener;
pub mod pool;
pub mod tcp;
pub mod tls;
pub mod unix;

// ---------------------------------------------------------------------------
// Re-exports for convenience
// ---------------------------------------------------------------------------

pub use connection::{Connection, ConnectionId, ConnectionInfo, ConnectionState};
pub use error::{TransportError, TransportResult};
pub use inproc::{InProcBus, InProcConnection, InProcTransport};
pub use listener::{DafTcpListener, Listener, ListenerConfig};
pub use pool::{ConnectionPool, PoolConfig};
pub use tcp::{TcpConnection, TcpTransport, TcpTransportConfig};
pub use tls::{TlsConfig, TlsConnection, TlsTransport};
pub use unix::{UnixConnection, UnixTransport, UnixTransportConfig};

#[cfg(unix)]
pub use listener::DafUnixListener;

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// High-level transport abstraction.
///
/// A `Transport` can listen for inbound connections and dial outbound ones.
/// Each transport backend (TCP, Unix, TLS, InProc) provides its own config
/// and connection types while adhering to this interface.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    /// The connection type produced by this transport.
    type Conn: Connection;

    /// Start listening and return the bound address.
    async fn listen(&mut self) -> TransportResult<String>;

    /// Accept the next inbound connection.
    async fn accept(&self) -> TransportResult<Self::Conn>;

    /// Dial an outbound connection to `addr`.
    async fn connect(&self, addr: &str) -> TransportResult<Self::Conn>;

    /// Gracefully shut down the transport.
    async fn shutdown(&self) -> TransportResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_are_accessible() {
        // Smoke test: verify that the re-exports compile.
        let _: ConnectionState = ConnectionState::Connected;
        let _ = ConnectionId::next();
        let _ = PoolConfig::default();
        let _ = TcpTransportConfig::default();
    }
}
