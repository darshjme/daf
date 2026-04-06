//! In-process channel transport for co-located agents.
//!
//! When two agents live in the same process, routing traffic through TCP
//! is wasteful. [`InProcTransport`] uses tokio mpsc channels for
//! zero-serialization, near-zero-latency message passing. The [`InProcBus`]
//! manages named channels between agent pairs.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::connection::{Connection, ConnectionInfo, ConnectionState};
use crate::error::{TransportError, TransportResult};

// ---------------------------------------------------------------------------
// Channel capacity
// ---------------------------------------------------------------------------

/// Default channel buffer size (messages, not bytes).
const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

// ---------------------------------------------------------------------------
// InProc connection
// ---------------------------------------------------------------------------

/// One side of an in-process channel connection.
///
/// Data is passed as `Bytes` — when the sender already holds a `Bytes` buffer,
/// this is zero-copy.
pub struct InProcConnection {
    tx: mpsc::Sender<Bytes>,
    rx: mpsc::Receiver<Bytes>,
    /// Buffered partial read from a previous `read` call.
    pending: Vec<u8>,
    info: ConnectionInfo,
}

impl InProcConnection {
    /// Create a matched pair of in-process connections.
    ///
    /// Returns `(a, b)` where data written to `a` can be read from `b`
    /// and vice versa.
    pub fn pair(capacity: usize) -> (Self, Self) {
        let (tx_a, rx_b) = mpsc::channel(capacity);
        let (tx_b, rx_a) = mpsc::channel(capacity);

        let a = Self {
            tx: tx_a,
            rx: rx_a,
            pending: Vec::new(),
            info: ConnectionInfo::new(Some("inproc:a".into())),
        };

        let b = Self {
            tx: tx_b,
            rx: rx_b,
            pending: Vec::new(),
            info: ConnectionInfo::new(Some("inproc:b".into())),
        };

        (
            Self { info: { let mut i = a.info; i.state = ConnectionState::Connected; i }, ..a },
            Self { info: { let mut i = b.info; i.state = ConnectionState::Connected; i }, ..b },
        )
    }
}

#[async_trait]
impl Connection for InProcConnection {
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        // Drain any leftover bytes from a previous oversized message.
        if !self.pending.is_empty() {
            let n = std::cmp::min(buf.len(), self.pending.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            self.info.bytes_received += n as u64;
            return Ok(n);
        }

        match self.rx.recv().await {
            Some(data) => {
                let n = std::cmp::min(buf.len(), data.len());
                buf[..n].copy_from_slice(&data[..n]);
                if data.len() > n {
                    self.pending.extend_from_slice(&data[n..]);
                }
                self.info.bytes_received += n as u64;
                Ok(n)
            }
            None => {
                self.info.state = ConnectionState::Closed;
                Ok(0)
            }
        }
    }

    async fn write(&mut self, data: &[u8]) -> TransportResult<()> {
        let bytes = Bytes::copy_from_slice(data);
        self.tx
            .send(bytes)
            .await
            .map_err(|_| TransportError::ChannelError("receiver dropped".into()))?;
        self.info.bytes_sent += data.len() as u64;
        Ok(())
    }

    async fn write_bytes(&mut self, data: Bytes) -> TransportResult<()> {
        let len = data.len();
        self.tx
            .send(data)
            .await
            .map_err(|_| TransportError::ChannelError("receiver dropped".into()))?;
        self.info.bytes_sent += len as u64;
        Ok(())
    }

    async fn flush(&mut self) -> TransportResult<()> {
        // Channels don't buffer; flush is a no-op.
        Ok(())
    }

    async fn close(&mut self) -> TransportResult<()> {
        self.info.state = ConnectionState::Closed;
        self.rx.close();
        // Dropping tx will signal the peer.
        tracing::debug!(id = %self.info.id, "InProc connection closed");
        Ok(())
    }

    fn peer_addr(&self) -> Option<String> {
        self.info.peer_addr.clone()
    }

    fn is_alive(&self) -> bool {
        self.info.state == ConnectionState::Connected && !self.tx.is_closed()
    }

    fn info(&self) -> &ConnectionInfo {
        &self.info
    }
}

// ---------------------------------------------------------------------------
// InProc bus
// ---------------------------------------------------------------------------

/// Key for a channel between two named agents.
///
/// The key is canonicalized so that `(a, b)` and `(b, a)` map to the same
/// channel pair.
fn channel_key(a: &str, b: &str) -> String {
    let mut parts = [a, b];
    parts.sort();
    format!("{}:{}", parts[0], parts[1])
}

/// A registry of named in-process channels between agent pairs.
///
/// Agents request a connection to a peer by name. The bus creates the
/// channel pair on first contact and returns the correct end to each caller.
pub struct InProcBus {
    /// Maps `channel_key` to the unclaimed second-end of the channel pair.
    /// The first caller gets one end immediately; the second caller gets the
    /// stored end.
    pending: Arc<DashMap<String, InProcConnection>>,
    /// Channel buffer size.
    capacity: usize,
}

impl InProcBus {
    /// Create a new bus with the default channel capacity.
    pub fn new() -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
            capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    /// Create a bus with a custom channel capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
            capacity,
        }
    }

    /// Get or create a connection between two named agents.
    ///
    /// The first caller for a given pair creates the underlying channel and
    /// gets one end. The second caller receives the other end. Subsequent
    /// calls after both ends are claimed will create a new channel pair.
    pub fn connect(&self, from: &str, to: &str) -> InProcConnection {
        let key = channel_key(from, to);

        // Try to take the pending end.
        if let Some((_, conn)) = self.pending.remove(&key) {
            tracing::debug!(from = %from, to = %to, "InProcBus: matched existing channel");
            return conn;
        }

        // No pending end — create a new pair, return one, store the other.
        let (a, b) = InProcConnection::pair(self.capacity);
        self.pending.insert(key, b);
        tracing::debug!(from = %from, to = %to, "InProcBus: created new channel pair");
        a
    }

    /// Return the number of pending (unmatched) channel ends.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

impl Default for InProcBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// InProc transport (convenience wrapper)
// ---------------------------------------------------------------------------

/// Convenience transport using the shared [`InProcBus`].
pub struct InProcTransport {
    bus: Arc<InProcBus>,
    /// The name of this agent endpoint.
    name: String,
}

impl InProcTransport {
    /// Create a named in-process transport endpoint backed by the given bus.
    pub fn new(name: impl Into<String>, bus: Arc<InProcBus>) -> Self {
        Self {
            bus,
            name: name.into(),
        }
    }

    /// Open a connection to the named peer.
    pub fn connect(&self, peer: &str) -> InProcConnection {
        self.bus.connect(&self.name, peer)
    }

    /// The name of this endpoint.
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inproc_pair_roundtrip() {
        let (mut a, mut b) = InProcConnection::pair(16);

        a.write(b"hello from a").await.unwrap();
        let mut buf = [0u8; 64];
        let n = b.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from a");

        b.write(b"hello from b").await.unwrap();
        let n = a.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from b");
    }

    #[tokio::test]
    async fn inproc_zero_copy_bytes() {
        let (mut a, mut b) = InProcConnection::pair(16);
        let data = Bytes::from_static(b"zero-copy");
        a.write_bytes(data).await.unwrap();

        let mut buf = [0u8; 64];
        let n = b.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"zero-copy");

        assert_eq!(a.info().bytes_sent, 9);
        assert_eq!(b.info().bytes_received, 9);
    }

    #[tokio::test]
    async fn inproc_partial_read() {
        let (mut a, mut b) = InProcConnection::pair(16);
        a.write(b"abcdefghij").await.unwrap(); // 10 bytes

        let mut small = [0u8; 4];
        let n = b.read(&mut small).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(&small, b"abcd");

        let n = b.read(&mut small).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(&small, b"efgh");

        let n = b.read(&mut small).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(&small[..n], b"ij");
    }

    #[tokio::test]
    async fn inproc_close_signals_eof() {
        let (mut a, mut b) = InProcConnection::pair(16);
        a.close().await.unwrap();
        assert!(!a.is_alive());

        // b should eventually get EOF.
        drop(a);
        let mut buf = [0u8; 8];
        let n = b.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn inproc_bus_connect() {
        let bus = InProcBus::new();

        let mut conn_a = bus.connect("agent-1", "agent-2");
        assert_eq!(bus.pending_count(), 1);

        let mut conn_b = bus.connect("agent-2", "agent-1"); // reversed order
        assert_eq!(bus.pending_count(), 0);

        conn_a.write(b"from-1").await.unwrap();
        let mut buf = [0u8; 32];
        let n = conn_b.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"from-1");
    }

    #[test]
    fn channel_key_is_canonical() {
        assert_eq!(channel_key("a", "b"), channel_key("b", "a"));
        assert_eq!(channel_key("x", "y"), "x:y");
    }

    #[test]
    fn inproc_transport_name() {
        let bus = Arc::new(InProcBus::new());
        let t = InProcTransport::new("my-agent", bus);
        assert_eq!(t.name(), "my-agent");
    }
}
