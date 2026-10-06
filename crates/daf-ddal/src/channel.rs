//! # Logical Communication Channels
//!
//! A **channel** is a bidirectional logical stream multiplexed over a single
//! TCP connection. Each channel has its own stream ID, state machine, and
//! bounded message queue so that slow consumers on one channel cannot starve
//! others sharing the same socket.
//!
//! ## Multiplexing model
//!
//! ```text
//!  TCP connection
//!  ┌──────────────────────────────────────────────┐
//!  │  Channel 0  (control)    ◄──► stream_id = 0  │
//!  │  Channel 1  (agent A→B)  ◄──► stream_id = 1  │
//!  │  Channel 2  (agent A→C)  ◄──► stream_id = 2  │
//!  │  ...                                         │
//!  └──────────────────────────────────────────────┘
//! ```
//!
//! The [`ChannelPool`] manages the lifecycle of all channels on a connection.
//! It assigns stream IDs, enforces concurrency limits, and provides
//! send/receive handles with built-in back-pressure via bounded
//! [`tokio::sync::mpsc`] channels.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument, warn};

use daf_core::AgentId;

use crate::protocol::Frame;

// ---------------------------------------------------------------------------
// ChannelState
// ---------------------------------------------------------------------------

/// State machine for a logical channel's lifecycle.
///
/// ```text
///  Opening ──▶ Open ──▶ Closing ──▶ Closed
///                │                     ▲
///                └─────────────────────┘  (error path)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelState {
    /// Handshake in progress — the channel exists but is not yet usable.
    Opening,
    /// Fully established — frames can flow in both directions.
    Open,
    /// A close has been initiated but not yet acknowledged by the peer.
    Closing,
    /// Fully torn down — no further frames will be sent or received.
    Closed,
}

impl ChannelState {
    /// Returns `true` when the channel can carry application data.
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Open)
    }

    /// Returns `true` when the channel has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

impl fmt::Display for ChannelState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Opening => "opening",
            Self::Open => "open",
            Self::Closing => "closing",
            Self::Closed => "closed",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Channel
// ---------------------------------------------------------------------------

/// A single logical channel within a multiplexed connection.
///
/// Each channel tracks the two communicating agents, its stream ID (used to
/// tag frames on the wire), and a bounded send/receive pair for back-pressure.
#[derive(Debug)]
pub struct Channel {
    /// Unique channel identifier (scoped to this connection).
    pub id: u32,
    /// The agent on the local side of this channel.
    pub source_agent: AgentId,
    /// The agent on the remote side of this channel.
    pub target_agent: AgentId,
    /// Wire-level stream identifier — maps 1:1 with `id` today, but kept
    /// separate so we can remap during connection migration.
    pub stream_id: u32,
    /// Current lifecycle state.
    pub state: ChannelState,
    /// When the channel was created.
    pub created_at: DateTime<Utc>,
    /// Sender half of the bounded frame queue. The codec task reads from the
    /// matching receiver and writes frames to the socket.
    queues: Arc<ChannelQueues>,
    /// Receiver half for inbound frames delivered by the codec task.
    inbound_rx: mpsc::Receiver<Frame>,
}

impl Channel {
    /// Send a data frame through this channel.
    ///
    /// This is the primary API for application code. The method applies
    /// back-pressure: if the outbound queue is full, the future will suspend
    /// until space becomes available.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the channel is not open or the send queue has been
    /// dropped (connection lost).
    pub async fn send(&self, payload: Bytes) -> Result<(), ChannelError> {
        if !self.state.is_usable() {
            return Err(ChannelError::NotOpen {
                channel_id: self.id,
                state: self.state,
            });
        }

        let frame = Frame::data(self.stream_id, payload);
        self.queues
            .send(&self.queues.outbound, frame, self.id)
            .await
    }

    /// Send a pre-built frame through this channel.
    ///
    /// Used internally for control frames (Ping, Close, etc.) that don't
    /// carry application payloads.
    pub async fn send_frame(&self, frame: Frame) -> Result<(), ChannelError> {
        if self.state.is_terminal() {
            return Err(ChannelError::NotOpen {
                channel_id: self.id,
                state: self.state,
            });
        }
        self.queues
            .send(&self.queues.outbound, frame, self.id)
            .await
    }

    /// Receive the next inbound frame.
    ///
    /// Returns `None` when the channel has been closed and all buffered
    /// frames have been consumed.
    pub async fn recv(&mut self) -> Option<Frame> {
        self.inbound_rx.recv().await
    }

    /// Try to receive without blocking.
    pub fn try_recv(&mut self) -> Result<Frame, mpsc::error::TryRecvError> {
        self.inbound_rx.try_recv()
    }

    /// Approximate number of slots available in the outbound queue.
    pub fn outbound_capacity(&self) -> usize {
        self.queues
            .outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, mpsc::Sender::capacity)
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Channel(id={}, stream={}, {} -> {}, state={})",
            self.id, self.stream_id, self.source_agent, self.target_agent, self.state,
        )
    }
}

// ---------------------------------------------------------------------------
// ChannelHandle — the writer half given to the pool owner
// ---------------------------------------------------------------------------

/// Inbound writer handle held by the codec/transport layer.
///
/// When the codec decodes a frame from the socket it looks up the channel by
/// `stream_id` and pushes the frame through this handle.
#[derive(Debug, Clone)]
pub struct ChannelInboundWriter {
    /// The channel this writer belongs to.
    pub channel_id: u32,
    /// Sender into the channel's inbound queue.
    queues: Arc<ChannelQueues>,
}

impl ChannelInboundWriter {
    /// Deliver a frame to the channel's inbound queue.
    pub async fn deliver(&self, frame: Frame) -> Result<(), ChannelError> {
        self.queues
            .send(&self.queues.inbound, frame, self.channel_id)
            .await
    }
}

/// Shared queue ownership lets pool teardown invalidate all previously cloned
/// writers and disconnect a claimed outbound receiver after queued frames drain.
#[derive(Debug)]
struct ChannelQueues {
    outbound: Mutex<Option<mpsc::Sender<Frame>>>,
    inbound: Mutex<Option<mpsc::Sender<Frame>>>,
    closed: CancellationToken,
}

impl ChannelQueues {
    async fn send(
        &self,
        queue: &Mutex<Option<mpsc::Sender<Frame>>>,
        frame: Frame,
        channel_id: u32,
    ) -> Result<(), ChannelError> {
        let failed = || ChannelError::SendFailed { channel_id };
        let sender = queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
            .ok_or_else(failed)?;
        tokio::select! {
            biased;
            _ = self.closed.cancelled() => Err(failed()),
            permit = sender.reserve() => {
                let permit = permit.map_err(|_| failed())?;
                // Serialize enqueue against close, after waiting for capacity.
                // No synchronous lock is held across an await.
                let guard = queue.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_none() || self.closed.is_cancelled() {
                    return Err(failed());
                }
                permit.send(frame);
                Ok(())
            }
        }
    }

    fn close(&self) {
        self.closed.cancel();
        self.outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

// ---------------------------------------------------------------------------
// ChannelPool
// ---------------------------------------------------------------------------

/// Default capacity for the bounded frame queues (per-channel, per-direction).
const DEFAULT_CHANNEL_BUFFER: usize = 256;

/// Maximum number of channels allowed per connection.
const MAX_CHANNELS_PER_CONNECTION: u32 = 4096;

/// Manages all logical channels on a single connection.
///
/// Thread-safe via [`DashMap`] — multiple tasks can open/close channels and
/// dispatch frames concurrently.
pub struct ChannelPool {
    /// Map from channel ID to the inbound writer handle.
    ///
    /// The `Channel` struct itself is owned by the application task that
    /// created it. The pool only holds the inbound writer so the codec can
    /// push frames to the right channel.
    inbound_writers: DashMap<u32, ChannelInboundWriter>,
    outbound_receivers: DashMap<u32, mpsc::Receiver<Frame>>,
    lifecycle_lock: std::sync::Mutex<()>,

    /// Map from stream_id to channel_id (for frame dispatch).
    stream_to_channel: DashMap<u32, u32>,

    /// Monotonically increasing channel ID allocator.
    next_channel_id: AtomicU32,

    /// Capacity of bounded queues created for new channels.
    channel_buffer_size: usize,

    /// Maximum channels this pool will allow.
    max_channels: u32,
}

impl ChannelPool {
    /// Create a new channel pool with default settings.
    pub fn new() -> Self {
        Self {
            inbound_writers: DashMap::new(),
            outbound_receivers: DashMap::new(),
            lifecycle_lock: std::sync::Mutex::new(()),
            stream_to_channel: DashMap::new(),
            next_channel_id: AtomicU32::new(1), // 0 reserved for control
            channel_buffer_size: DEFAULT_CHANNEL_BUFFER,
            max_channels: MAX_CHANNELS_PER_CONNECTION,
        }
    }

    /// Create a pool with custom buffer size and channel limit.
    pub fn with_limits(channel_buffer_size: usize, max_channels: u32) -> Self {
        Self {
            inbound_writers: DashMap::new(),
            outbound_receivers: DashMap::new(),
            lifecycle_lock: std::sync::Mutex::new(()),
            stream_to_channel: DashMap::new(),
            next_channel_id: AtomicU32::new(1),
            channel_buffer_size: channel_buffer_size.max(1),
            max_channels,
        }
    }

    /// Open a new channel between two agents.
    ///
    /// Returns the [`Channel`] (owned by the caller) and registers the
    /// inbound writer so the codec can deliver frames to it.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the pool has reached its channel limit.
    #[instrument(skip(self), fields(source = %source, target = %target))]
    pub fn open(&self, source: AgentId, target: AgentId) -> Result<Channel, ChannelError> {
        let _guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.active_count() >= self.max_channels as usize {
            return Err(ChannelError::PoolExhausted {
                max: self.max_channels,
            });
        }

        let id = self
            .next_channel_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| ChannelError::PoolExhausted {
                max: self.max_channels,
            })?;

        let stream_id = id; // 1:1 mapping for now

        // Outbound: application → codec → socket
        let (outbound_tx, outbound_rx) = mpsc::channel::<Frame>(self.channel_buffer_size);
        self.outbound_receivers.insert(id, outbound_rx);

        // Inbound: socket → codec → application
        let (inbound_tx, inbound_rx) = mpsc::channel::<Frame>(self.channel_buffer_size);
        let queues = Arc::new(ChannelQueues {
            outbound: Mutex::new(Some(outbound_tx)),
            inbound: Mutex::new(Some(inbound_tx)),
            closed: CancellationToken::new(),
        });

        let channel = Channel {
            id,
            source_agent: source,
            target_agent: target,
            stream_id,
            state: ChannelState::Opening,
            created_at: Utc::now(),
            queues: queues.clone(),
            inbound_rx,
        };

        let writer = ChannelInboundWriter {
            channel_id: id,
            queues,
        };

        self.inbound_writers.insert(id, writer);
        self.stream_to_channel.insert(stream_id, id);

        debug!(channel_id = id, stream_id, "channel opened");
        Ok(channel)
    }

    /// Look up the inbound writer for a given stream ID.
    ///
    /// Called by the codec task to dispatch a decoded frame to the correct
    /// channel.
    pub fn writer_for_stream(&self, stream_id: u32) -> Option<ChannelInboundWriter> {
        let channel_id = self.stream_to_channel.get(&stream_id)?;
        let writer = self.inbound_writers.get(&*channel_id)?;
        Some(writer.clone())
    }

    /// Claim the outbound queue exactly once for the connection's writer task.
    pub fn take_outbound_receiver(&self, channel_id: u32) -> Option<mpsc::Receiver<Frame>> {
        self.outbound_receivers
            .remove(&channel_id)
            .map(|(_, rx)| rx)
    }

    /// Remove a channel from the pool and invalidate all existing send handles.
    /// Already queued frames remain available until each receiver drains them.
    #[instrument(skip(self))]
    pub fn close(&self, channel_id: u32) {
        let _guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((_, writer)) = self.inbound_writers.remove(&channel_id) {
            writer.queues.close();
            self.outbound_receivers.remove(&channel_id);
            // Find and remove the stream mapping.
            self.stream_to_channel.retain(|_, cid| *cid != channel_id);
            debug!(channel_id, "channel removed from pool");
        } else {
            warn!(channel_id, "attempted to close unknown channel");
        }
    }

    /// Number of active channels in the pool.
    pub fn active_count(&self) -> usize {
        self.inbound_writers.len()
    }

    /// Returns `true` if the pool has no channels.
    pub fn is_empty(&self) -> bool {
        self.inbound_writers.is_empty()
    }

    /// Iterate over all channel IDs currently in the pool.
    pub fn channel_ids(&self) -> Vec<u32> {
        self.inbound_writers
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    /// Maximum number of channels allowed.
    pub fn max_channels(&self) -> u32 {
        self.max_channels
    }
}

impl Default for ChannelPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ChannelPool {
    fn drop(&mut self) {
        for writer in self.inbound_writers.iter() {
            writer.queues.close();
        }
    }
}

impl fmt::Debug for ChannelPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelPool")
            .field("active_channels", &self.active_count())
            .field("max_channels", &self.max_channels)
            .field("buffer_size", &self.channel_buffer_size)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors specific to channel operations.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("channel {channel_id} is not open (state: {state})")]
    NotOpen {
        channel_id: u32,
        state: ChannelState,
    },

    #[error("failed to send on channel {channel_id}: receiver dropped")]
    SendFailed { channel_id: u32 },

    #[error("channel pool exhausted: maximum {max} channels reached")]
    PoolExhausted { max: u32 },

    #[error("channel {channel_id} not found")]
    NotFound { channel_id: u32 },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> (AgentId, AgentId) {
        (AgentId::new(), AgentId::new())
    }

    #[tokio::test]
    async fn outbound_queue_is_claimable_and_preserves_backpressure() {
        let pool = ChannelPool::with_limits(1, 1);
        let mut channel = pool.open(AgentId::new(), AgentId::new()).unwrap();
        channel.state = ChannelState::Open;
        let mut outbound = pool.take_outbound_receiver(channel.id).unwrap();
        assert!(pool.take_outbound_receiver(channel.id).is_none());
        channel.send(Bytes::from_static(b"first")).await.unwrap();
        let send = channel.send(Bytes::from_static(b"second"));
        tokio::pin!(send);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut send)
                .await
                .is_err()
        );
        assert_eq!(
            outbound.recv().await.unwrap().payload,
            Bytes::from_static(b"first")
        );
        send.await.unwrap();
        assert_eq!(
            outbound.recv().await.unwrap().payload,
            Bytes::from_static(b"second")
        );
        pool.close(channel.id);
        assert!(
            pool.open(AgentId::new(), AgentId::new()).is_ok(),
            "closed channels must release capacity"
        );
    }

    #[tokio::test]
    async fn close_invalidates_stale_handles_and_preserves_queued_frames() {
        let pool = ChannelPool::with_limits(1, 1);
        let mut channel = pool.open(AgentId::new(), AgentId::new()).unwrap();
        channel.state = ChannelState::Open;
        let writer = pool.writer_for_stream(channel.stream_id).unwrap();
        let mut outbound = pool.take_outbound_receiver(channel.id).unwrap();
        channel
            .send(Bytes::from_static(b"queued outbound"))
            .await
            .unwrap();
        writer
            .deliver(Frame::data(
                channel.stream_id,
                Bytes::from_static(b"queued inbound"),
            ))
            .await
            .unwrap();

        {
            let send = channel.send(Bytes::from_static(b"blocked outbound"));
            let deliver = writer.deliver(Frame::data(
                channel.stream_id,
                Bytes::from_static(b"blocked inbound"),
            ));
            tokio::pin!(send, deliver);
            let wait = std::time::Duration::from_millis(10);
            assert!(tokio::time::timeout(wait, &mut send).await.is_err());
            assert!(tokio::time::timeout(wait, &mut deliver).await.is_err());
            pool.close(channel.id);
            let deadline = std::time::Duration::from_secs(1);
            assert!(tokio::time::timeout(deadline, send).await.unwrap().is_err());
            assert!(
                tokio::time::timeout(deadline, deliver)
                    .await
                    .unwrap()
                    .is_err()
            );
        }

        assert_eq!(
            outbound.recv().await.unwrap().payload,
            Bytes::from_static(b"queued outbound")
        );
        assert_eq!(
            channel.recv().await.unwrap().payload,
            Bytes::from_static(b"queued inbound")
        );
        let deadline = std::time::Duration::from_secs(1);
        assert!(
            tokio::time::timeout(deadline, outbound.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            tokio::time::timeout(deadline, channel.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(channel.send(Bytes::from_static(b"late")).await.is_err());
        assert!(
            channel
                .send_frame(Frame::control(
                    crate::protocol::FrameType::Close,
                    channel.stream_id
                ))
                .await
                .is_err()
        );
        assert!(
            writer
                .deliver(Frame::data(channel.stream_id, Bytes::new()))
                .await
                .is_err()
        );
        assert_eq!(channel.outbound_capacity(), 0);
        assert!(pool.writer_for_stream(channel.stream_id).is_none());
        assert!(pool.open(AgentId::new(), AgentId::new()).is_ok());
    }

    #[tokio::test]
    async fn dropping_pool_disconnects_claimed_receiver_and_cloned_writer() {
        let pool = ChannelPool::new();
        let mut channel = pool.open(AgentId::new(), AgentId::new()).unwrap();
        channel.state = ChannelState::Open;
        let writer = pool.writer_for_stream(channel.stream_id).unwrap();
        let mut outbound = pool.take_outbound_receiver(channel.id).unwrap();
        drop(pool);
        assert!(channel.send(Bytes::new()).await.is_err());
        assert!(
            writer
                .deliver(Frame::data(channel.stream_id, Bytes::new()))
                .await
                .is_err()
        );
        let deadline = std::time::Duration::from_secs(1);
        assert!(
            tokio::time::timeout(deadline, outbound.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            tokio::time::timeout(deadline, channel.recv())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn channel_state_predicates() {
        assert!(!ChannelState::Opening.is_usable());
        assert!(ChannelState::Open.is_usable());
        assert!(!ChannelState::Closing.is_usable());
        assert!(!ChannelState::Closed.is_usable());

        assert!(!ChannelState::Open.is_terminal());
        assert!(ChannelState::Closed.is_terminal());
    }

    #[test]
    fn pool_opens_channels() {
        let pool = ChannelPool::new();
        let (a, b) = agents();

        let ch = pool.open(a, b).unwrap();
        assert_eq!(ch.source_agent, a);
        assert_eq!(ch.target_agent, b);
        assert_eq!(ch.state, ChannelState::Opening);
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn pool_assigns_unique_ids() {
        let pool = ChannelPool::new();
        let (a, b) = agents();

        let ch1 = pool.open(a, b).unwrap();
        let ch2 = pool.open(b, a).unwrap();
        assert_ne!(ch1.id, ch2.id);
        assert_ne!(ch1.stream_id, ch2.stream_id);
    }

    #[test]
    fn pool_close_removes_channel() {
        let pool = ChannelPool::new();
        let (a, b) = agents();

        let ch = pool.open(a, b).unwrap();
        let id = ch.id;
        assert_eq!(pool.active_count(), 1);

        pool.close(id);
        assert_eq!(pool.active_count(), 0);
        assert!(pool.is_empty());
    }

    #[test]
    fn pool_respects_max_channels() {
        let pool = ChannelPool::with_limits(16, 2);
        let (a, b) = agents();

        let _ch1 = pool.open(a, b).unwrap();
        let _ch2 = pool.open(a, b).unwrap();
        let result = pool.open(a, b);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ChannelError::PoolExhausted { .. }
        ));
    }

    #[test]
    fn writer_for_stream_lookup() {
        let pool = ChannelPool::new();
        let (a, b) = agents();

        let ch = pool.open(a, b).unwrap();
        let writer = pool.writer_for_stream(ch.stream_id);
        assert!(writer.is_some());
        assert_eq!(writer.unwrap().channel_id, ch.id);
    }

    #[test]
    fn writer_for_unknown_stream_returns_none() {
        let pool = ChannelPool::new();
        assert!(pool.writer_for_stream(9999).is_none());
    }

    #[test]
    fn channel_ids_returns_all() {
        let pool = ChannelPool::new();
        let (a, b) = agents();

        let ch1 = pool.open(a, b).unwrap();
        let ch2 = pool.open(b, a).unwrap();

        let mut ids = pool.channel_ids();
        ids.sort();
        let mut expected = vec![ch1.id, ch2.id];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn inbound_delivery() {
        let pool = ChannelPool::new();
        let (a, b) = agents();

        let mut ch = pool.open(a, b).unwrap();
        let writer = pool.writer_for_stream(ch.stream_id).unwrap();

        let frame = Frame::data(ch.stream_id, Bytes::from_static(b"hello"));
        writer.deliver(frame).await.unwrap();

        let received = ch.recv().await.unwrap();
        assert_eq!(received.payload, Bytes::from_static(b"hello"));
    }

    #[test]
    fn channel_display() {
        let pool = ChannelPool::new();
        let (a, b) = agents();
        let ch = pool.open(a, b).unwrap();
        let display = format!("{ch}");
        assert!(display.contains("Channel("));
        assert!(display.contains("opening"));
    }

    #[test]
    fn pool_debug() {
        let pool = ChannelPool::new();
        let debug = format!("{pool:?}");
        assert!(debug.contains("ChannelPool"));
        assert!(debug.contains("active_channels"));
    }
}
