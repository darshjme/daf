//! Message types for inter-agent communication.
//!
//! All communication between agents — commands, responses, events,
//! heartbeats, and handoffs — flows through the [`Message`] type.
//! Messages are wrapped in an [`Envelope`] for routing through the
//! transport layer.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentId;

// ---------------------------------------------------------------------------
// MessageId
// ---------------------------------------------------------------------------

/// Time-ordered message identifier backed by UUID v7.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(Uuid);

impl MessageId {
    /// Generate a new time-ordered message identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageId({})", &self.0.to_string()[..8])
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for MessageId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

// ---------------------------------------------------------------------------
// MessageKind
// ---------------------------------------------------------------------------

/// Classification of a message's intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// A request expecting a response (RPC-style).
    Request,
    /// A response to a previous request, correlated by `correlation_id`.
    Response,
    /// A fire-and-forget notification of something that happened.
    Event,
    /// An imperative instruction to the target agent.
    Command,
    /// Periodic liveness signal.
    Heartbeat,
    /// Transfer of ownership / context from one agent to another.
    Handoff,
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Event => "event",
            Self::Command => "command",
            Self::Heartbeat => "heartbeat",
            Self::Handoff => "handoff",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Message delivery priority. Higher-priority messages are dequeued first
/// when the transport layer supports priority ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Must be processed immediately; may preempt running work.
    Critical = 0,
    /// Important but not preemptive.
    High = 1,
    /// Default priority for most messages.
    Normal = 2,
    /// Best-effort delivery; may be dropped under load.
    Low = 3,
    /// Batch / background processing.
    Background = 4,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Normal
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
            Self::Background => "background",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// The fundamental unit of inter-agent communication.
///
/// Messages carry an opaque binary payload ([`Bytes`]) so that the core
/// crate is codec-agnostic. Higher-level crates deserialize the payload
/// using the codec indicated by headers or convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique identifier for this message.
    pub id: MessageId,
    /// What kind of message this is.
    pub kind: MessageKind,
    /// Agent that sent the message.
    pub source: AgentId,
    /// Intended recipient. `None` means broadcast.
    pub target: Option<AgentId>,
    /// Delivery priority.
    pub priority: Priority,
    /// Opaque binary payload.
    #[serde(with = "bytes_serde")]
    pub payload: Bytes,
    /// Identifier linking a response back to the originating request.
    pub correlation_id: Option<MessageId>,
    /// Wall-clock time when the message was created.
    pub timestamp: DateTime<Utc>,
    /// Time-to-live. Messages older than this should be discarded.
    pub ttl: Option<Duration>,
    /// Arbitrary headers for extensibility (content-type, encoding, etc.).
    pub headers: HashMap<String, String>,
}

/// Serde bridge for [`Bytes`] — serializes as base64 in JSON, raw in binary.
mod bytes_serde {
    use bytes::Bytes;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v: Vec<u8> = Deserialize::deserialize(deserializer)?;
        Ok(Bytes::from(v))
    }
}

impl Message {
    /// Start building a new message.
    pub fn builder(kind: MessageKind, source: AgentId) -> MessageBuilder {
        MessageBuilder {
            kind,
            source,
            target: None,
            priority: Priority::Normal,
            payload: Bytes::new(),
            correlation_id: None,
            ttl: None,
            headers: HashMap::new(),
        }
    }

    /// Returns `true` if the message has expired according to its TTL.
    pub fn is_expired(&self) -> bool {
        if let Some(ttl) = self.ttl {
            let age = Utc::now() - self.timestamp;
            if let Ok(age_std) = age.to_std() {
                return age_std > ttl;
            }
        }
        false
    }

    /// Shorthand: is this a request expecting a response?
    pub fn expects_reply(&self) -> bool {
        self.kind == MessageKind::Request
    }

    /// Read the payload as a UTF-8 string (best-effort).
    pub fn payload_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.payload)
    }

    /// Convenience: deserialize the payload as JSON.
    pub fn payload_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }

    /// Total size in bytes (approximate, for back-pressure calculations).
    pub fn size_bytes(&self) -> usize {
        self.payload.len()
            + self
                .headers
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
            + 128 // fixed overhead estimate for the struct fields
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{kind}] {id} {src} -> {tgt} (pri={pri}, {size}B)",
            kind = self.kind,
            id = self.id,
            src = self.source,
            tgt = self
                .target
                .map(|t| t.to_string())
                .unwrap_or_else(|| "*".into()),
            pri = self.priority,
            size = self.payload.len(),
        )
    }
}

// ---------------------------------------------------------------------------
// MessageBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for [`Message`].
pub struct MessageBuilder {
    kind: MessageKind,
    source: AgentId,
    target: Option<AgentId>,
    priority: Priority,
    payload: Bytes,
    correlation_id: Option<MessageId>,
    ttl: Option<Duration>,
    headers: HashMap<String, String>,
}

impl MessageBuilder {
    /// Set the target agent.
    pub fn target(mut self, target: AgentId) -> Self {
        self.target = Some(target);
        self
    }

    /// Set delivery priority.
    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Set the raw payload.
    pub fn payload(mut self, payload: impl Into<Bytes>) -> Self {
        self.payload = payload.into();
        self
    }

    /// Set a JSON-serializable payload.
    pub fn payload_json<T: Serialize>(mut self, value: &T) -> Result<Self, serde_json::Error> {
        self.payload = Bytes::from(serde_json::to_vec(value)?);
        self.headers
            .insert("content-type".into(), "application/json".into());
        Ok(self)
    }

    /// Set the correlation ID for request/response linking.
    pub fn correlation_id(mut self, id: MessageId) -> Self {
        self.correlation_id = Some(id);
        self
    }

    /// Set the time-to-live.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Insert a header.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Consume the builder and produce the [`Message`].
    pub fn build(self) -> Message {
        Message {
            id: MessageId::new(),
            kind: self.kind,
            source: self.source,
            target: self.target,
            priority: self.priority,
            payload: self.payload,
            correlation_id: self.correlation_id,
            timestamp: Utc::now(),
            ttl: self.ttl,
            headers: self.headers,
        }
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A message wrapped with routing metadata for the transport layer.
///
/// The envelope accumulates hop information as the message passes through
/// routers, and carries a distributed trace ID for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// The wrapped message.
    pub message: Message,
    /// Distributed trace identifier for correlating across services.
    pub trace_id: Uuid,
    /// Number of routing hops this message has traversed.
    pub hops: u32,
    /// Maximum hops before the message is discarded (prevents loops).
    pub max_hops: u32,
    /// Ordered list of agent IDs this message has passed through.
    pub route: Vec<AgentId>,
}

impl Envelope {
    /// Wrap a message in a new envelope with a fresh trace ID.
    pub fn new(message: Message) -> Self {
        Self {
            message,
            trace_id: Uuid::now_v7(),
            hops: 0,
            max_hops: 16,
            route: Vec::new(),
        }
    }

    /// Wrap a message with an existing trace ID (for continuation).
    pub fn with_trace(message: Message, trace_id: Uuid) -> Self {
        Self {
            message,
            trace_id,
            hops: 0,
            max_hops: 16,
            route: Vec::new(),
        }
    }

    /// Record a hop through a routing agent. Returns an error if max hops
    /// is exceeded.
    pub fn record_hop(&mut self, agent: AgentId) -> crate::error::DafResult<()> {
        self.hops += 1;
        self.route.push(agent);
        if self.hops > self.max_hops {
            return Err(crate::error::DafError::ProtocolError {
                message: format!(
                    "message {} exceeded max hops ({}) — possible routing loop",
                    self.message.id, self.max_hops
                ),
            });
        }
        Ok(())
    }

    /// Returns `true` if the message has exceeded its hop limit.
    pub fn is_loop_detected(&self) -> bool {
        self.hops > self.max_hops
    }
}

impl fmt::Display for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Envelope(trace={trace}, hops={hops}, msg={msg})",
            trace = &self.trace_id.to_string()[..8],
            hops = self.hops,
            msg = self.message,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_source() -> AgentId {
        AgentId::new()
    }

    #[test]
    fn message_builder_basic() {
        let src = test_source();
        let tgt = AgentId::new();
        let msg = Message::builder(MessageKind::Request, src)
            .target(tgt)
            .priority(Priority::High)
            .payload(Bytes::from_static(b"hello"))
            .build();

        assert_eq!(msg.kind, MessageKind::Request);
        assert_eq!(msg.source, src);
        assert_eq!(msg.target, Some(tgt));
        assert_eq!(msg.priority, Priority::High);
        assert_eq!(msg.payload_str().unwrap(), "hello");
        assert!(msg.expects_reply());
    }

    #[test]
    fn message_builder_json_payload() {
        let src = test_source();
        let data = serde_json::json!({"action": "deploy", "version": 42});
        let msg = Message::builder(MessageKind::Command, src)
            .payload_json(&data)
            .unwrap()
            .build();

        let back: serde_json::Value = msg.payload_json().unwrap();
        assert_eq!(back["action"], "deploy");
        assert_eq!(
            msg.headers.get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn message_ttl_expiry() {
        let src = test_source();
        let msg = Message::builder(MessageKind::Heartbeat, src)
            .ttl(Duration::from_millis(0))
            .build();
        // With a zero TTL the message is immediately expired.
        assert!(msg.is_expired());
    }

    #[test]
    fn message_display() {
        let src = test_source();
        let msg = Message::builder(MessageKind::Event, src).build();
        let display = msg.to_string();
        assert!(display.contains("[event]"));
        assert!(display.contains("-> *")); // no target
    }

    #[test]
    fn message_serde_roundtrip() {
        let src = test_source();
        let msg = Message::builder(MessageKind::Request, src)
            .payload(Bytes::from_static(b"test"))
            .header("x-custom", "value")
            .build();

        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id);
        assert_eq!(back.payload, msg.payload);
        assert_eq!(back.headers.get("x-custom").unwrap(), "value");
    }

    #[test]
    fn envelope_hop_tracking() {
        let src = test_source();
        let msg = Message::builder(MessageKind::Request, src).build();
        let mut env = Envelope::new(msg);

        assert_eq!(env.hops, 0);
        env.record_hop(AgentId::new()).unwrap();
        assert_eq!(env.hops, 1);
        assert_eq!(env.route.len(), 1);
    }

    #[test]
    fn envelope_loop_detection() {
        let src = test_source();
        let msg = Message::builder(MessageKind::Request, src).build();
        let mut env = Envelope::new(msg);
        env.max_hops = 2;

        env.record_hop(AgentId::new()).unwrap();
        env.record_hop(AgentId::new()).unwrap();
        let result = env.record_hop(AgentId::new());
        assert!(result.is_err());
        assert!(env.is_loop_detected());
    }

    #[test]
    fn priority_ordering() {
        assert!(Priority::Critical < Priority::High);
        assert!(Priority::High < Priority::Normal);
        assert!(Priority::Normal < Priority::Low);
        assert!(Priority::Low < Priority::Background);
    }

    #[test]
    fn message_size_bytes() {
        let src = test_source();
        let msg = Message::builder(MessageKind::Event, src)
            .payload(Bytes::from(vec![0u8; 1000]))
            .build();
        assert!(msg.size_bytes() >= 1000);
    }
}
