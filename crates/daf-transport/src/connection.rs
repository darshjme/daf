//! Connection abstraction and metadata types.
//!
//! Every transport (TCP, Unix, TLS, in-process) provides connections that
//! implement the [`Connection`] trait. This module defines that trait plus
//! the supporting types for connection identity, state tracking, and
//! lifecycle statistics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::TransportResult;

// ---------------------------------------------------------------------------
// Connection ID
// ---------------------------------------------------------------------------

/// Monotonically increasing connection identifier.
///
/// Each new connection receives a unique ID within the process lifetime.
/// IDs are never reused.
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque, process-unique connection identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u64);

impl ConnectionId {
    /// Allocate the next globally unique connection ID.
    pub fn next() -> Self {
        Self(NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Return the raw numeric value.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Connection state
// ---------------------------------------------------------------------------

/// Lifecycle state of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Handshake or dial in progress.
    Connecting,
    /// Fully established and ready for I/O.
    Connected,
    /// Graceful shutdown initiated; no new writes, existing reads may complete.
    Draining,
    /// Fully closed. No further I/O is possible.
    Closed,
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => write!(f, "connecting"),
            Self::Connected => write!(f, "connected"),
            Self::Draining => write!(f, "draining"),
            Self::Closed => write!(f, "closed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Connection info
// ---------------------------------------------------------------------------

/// Metadata and cumulative statistics for a single connection.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// Unique identifier for this connection.
    pub id: ConnectionId,
    /// Address of the remote peer, if applicable.
    pub peer_addr: Option<String>,
    /// Timestamp when the connection was established.
    pub connected_at: Instant,
    /// Total bytes sent over this connection.
    pub bytes_sent: u64,
    /// Total bytes received over this connection.
    pub bytes_received: u64,
    /// Current lifecycle state.
    pub state: ConnectionState,
}

impl ConnectionInfo {
    /// Create a new `ConnectionInfo` in the [`ConnectionState::Connecting`] state.
    pub fn new(peer_addr: Option<String>) -> Self {
        Self {
            id: ConnectionId::next(),
            peer_addr,
            connected_at: Instant::now(),
            bytes_sent: 0,
            bytes_received: 0,
            state: ConnectionState::Connecting,
        }
    }

    /// Duration since the connection was established.
    pub fn age(&self) -> std::time::Duration {
        self.connected_at.elapsed()
    }
}

// ---------------------------------------------------------------------------
// Connection trait
// ---------------------------------------------------------------------------

/// A bidirectional byte-stream connection.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks. All operations are cancellation-safe.
#[async_trait]
pub trait Connection: Send + Sync + 'static {
    /// Read up to `buf.len()` bytes into `buf`, returning the number of
    /// bytes actually read. Returns `0` when the peer has closed.
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize>;

    /// Write the entire contents of `data` to the connection.
    async fn write(&mut self, data: &[u8]) -> TransportResult<()>;

    /// Write a `Bytes` buffer. Default implementation delegates to [`write`](Self::write).
    async fn write_bytes(&mut self, data: Bytes) -> TransportResult<()> {
        self.write(&data).await
    }

    /// Flush any buffered output to the underlying transport.
    async fn flush(&mut self) -> TransportResult<()>;

    /// Initiate a graceful close. After this returns, the connection is
    /// in [`ConnectionState::Closed`].
    async fn close(&mut self) -> TransportResult<()>;

    /// Return the peer address as a human-readable string, if known.
    fn peer_addr(&self) -> Option<String>;

    /// Return `true` if the connection is believed to be alive.
    fn is_alive(&self) -> bool;

    /// Return a snapshot of this connection's metadata.
    fn info(&self) -> &ConnectionInfo;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_id_is_unique() {
        let a = ConnectionId::next();
        let b = ConnectionId::next();
        assert_ne!(a, b);
        assert!(b.as_u64() > a.as_u64());
    }

    #[test]
    fn connection_id_display() {
        let id = ConnectionId::next();
        let s = id.to_string();
        assert!(s.starts_with("conn-"));
    }

    #[test]
    fn connection_state_display() {
        assert_eq!(ConnectionState::Connected.to_string(), "connected");
        assert_eq!(ConnectionState::Draining.to_string(), "draining");
    }

    #[test]
    fn connection_info_defaults() {
        let info = ConnectionInfo::new(Some("127.0.0.1:9000".into()));
        assert_eq!(info.bytes_sent, 0);
        assert_eq!(info.bytes_received, 0);
        assert_eq!(info.state, ConnectionState::Connecting);
        assert!(info.peer_addr.is_some());
    }
}
