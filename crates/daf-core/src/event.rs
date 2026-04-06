//! Event system for observability and reactive control flow.
//!
//! The event system decouples producers from consumers. Agents and the
//! runtime emit [`Event`]s through an [`EventBus`], and interested parties
//! subscribe with [`EventFilter`]s to receive only the events they care
//! about. This is the foundation for monitoring, auditing, and
//! trigger-based orchestration.

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::AgentId;
use crate::error::DafResult;

// ---------------------------------------------------------------------------
// EventKind
// ---------------------------------------------------------------------------

/// Classification of events emitted by the DAF runtime and agents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A new agent was spawned and initialized.
    AgentSpawned,
    /// An agent was shut down (graceful or forced).
    AgentTerminated,
    /// A task began executing on an agent.
    TaskStarted,
    /// A task completed successfully.
    TaskCompleted,
    /// A task failed (final failure, after retries exhausted).
    TaskFailed,
    /// A message was routed through the transport layer.
    MessageRouted,
    /// A resource pool is fully consumed.
    ResourceExhausted,
    /// An agent's health check returned an error.
    HealthCheckFailed,
    /// Extension point for application-specific events.
    Custom(String),
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentSpawned => write!(f, "agent_spawned"),
            Self::AgentTerminated => write!(f, "agent_terminated"),
            Self::TaskStarted => write!(f, "task_started"),
            Self::TaskCompleted => write!(f, "task_completed"),
            Self::TaskFailed => write!(f, "task_failed"),
            Self::MessageRouted => write!(f, "message_routed"),
            Self::ResourceExhausted => write!(f, "resource_exhausted"),
            Self::HealthCheckFailed => write!(f, "health_check_failed"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// A discrete occurrence in the DAF runtime.
///
/// Events are immutable once created. They carry structured data for
/// machine consumption and human-readable tags for filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event identifier (UUID v7 for time-ordering).
    pub id: Uuid,
    /// What happened.
    pub kind: EventKind,
    /// The agent or component that emitted this event.
    pub source: AgentId,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Structured event data (schema depends on [`EventKind`]).
    pub data: serde_json::Value,
    /// Free-form tags for filtering and grouping.
    pub tags: Vec<String>,
}

impl Event {
    /// Create a new event.
    pub fn new(kind: EventKind, source: AgentId) -> Self {
        Self {
            id: Uuid::now_v7(),
            kind,
            source,
            timestamp: Utc::now(),
            data: serde_json::Value::Null,
            tags: Vec::new(),
        }
    }

    /// Attach structured data to the event.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Add multiple tags.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    /// Check if the event has a specific tag.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// Age of the event relative to now.
    pub fn age(&self) -> std::time::Duration {
        (Utc::now() - self.timestamp)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO)
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{ts}] {kind} from {src}",
            ts = self.timestamp.format("%H:%M:%S%.3f"),
            kind = self.kind,
            src = self.source,
        )?;
        if !self.tags.is_empty() {
            write!(f, " tags=[{}]", self.tags.join(", "))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// EventFilter
// ---------------------------------------------------------------------------

/// Predicate for selecting events from the event bus.
///
/// A filter matches an event if ALL of the following are true:
/// - `kinds` is empty OR the event's kind is in `kinds`
/// - `sources` is empty OR the event's source is in `sources`
/// - `tags` is empty OR the event has at least one matching tag
///
/// An empty filter matches everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilter {
    /// Match only these event kinds (empty = all kinds).
    pub kinds: HashSet<EventKind>,
    /// Match only events from these sources (empty = all sources).
    pub sources: HashSet<AgentId>,
    /// Match events that have at least one of these tags (empty = all tags).
    pub tags: HashSet<String>,
}

impl EventFilter {
    /// Create an empty filter that matches all events.
    pub fn all() -> Self {
        Self::default()
    }

    /// Filter for a single event kind.
    pub fn kind(kind: EventKind) -> Self {
        let mut f = Self::default();
        f.kinds.insert(kind);
        f
    }

    /// Add an event kind to match.
    pub fn with_kind(mut self, kind: EventKind) -> Self {
        self.kinds.insert(kind);
        self
    }

    /// Add a source to match.
    pub fn with_source(mut self, source: AgentId) -> Self {
        self.sources.insert(source);
        self
    }

    /// Add a tag to match.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.insert(tag.into());
        self
    }

    /// Test whether an event matches this filter.
    pub fn matches(&self, event: &Event) -> bool {
        let kind_ok = self.kinds.is_empty() || self.kinds.contains(&event.kind);
        let source_ok = self.sources.is_empty() || self.sources.contains(&event.source);
        let tag_ok = self.tags.is_empty()
            || self.tags.iter().any(|t| event.tags.contains(t));
        kind_ok && source_ok && tag_ok
    }
}

// ---------------------------------------------------------------------------
// SubscriptionId
// ---------------------------------------------------------------------------

/// Opaque handle returned by [`EventBus::subscribe`] for unsubscribing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(Uuid);

impl SubscriptionId {
    /// Generate a new subscription ID.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for SubscriptionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sub-{}", &self.0.to_string()[..8])
    }
}

// ---------------------------------------------------------------------------
// EventBus trait
// ---------------------------------------------------------------------------

/// Publish/subscribe interface for the event system.
///
/// Implementations may be in-process (using channels) or distributed
/// (using the transport layer). The trait is object-safe so it can be
/// used behind `dyn EventBus`.
#[async_trait::async_trait]
pub trait EventBus: Send + Sync {
    /// Publish an event to all matching subscribers.
    async fn publish(&self, event: Event) -> DafResult<()>;

    /// Subscribe to events matching the filter. Returns a subscription
    /// handle and a receiver channel.
    async fn subscribe(
        &self,
        filter: EventFilter,
    ) -> DafResult<(SubscriptionId, tokio::sync::mpsc::Receiver<Event>)>;

    /// Remove a subscription. Future events will no longer be delivered.
    async fn unsubscribe(&self, id: SubscriptionId) -> DafResult<()>;
}

// ---------------------------------------------------------------------------
// InMemoryEventBus
// ---------------------------------------------------------------------------

/// Simple in-process event bus backed by `tokio::sync::mpsc` channels.
///
/// Suitable for single-node deployments and testing. For production
/// clusters, use a distributed implementation.
pub struct InMemoryEventBus {
    subscribers: parking_lot::Mutex<
        std::collections::HashMap<
            SubscriptionId,
            (EventFilter, tokio::sync::mpsc::Sender<Event>),
        >,
    >,
}

impl InMemoryEventBus {
    /// Create a new in-memory event bus.
    pub fn new() -> Self {
        Self {
            subscribers: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Number of active subscriptions.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().len()
    }
}

impl Default for InMemoryEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for InMemoryEventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryEventBus")
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

#[async_trait::async_trait]
impl EventBus for InMemoryEventBus {
    async fn publish(&self, event: Event) -> DafResult<()> {
        let subscribers = self.subscribers.lock();
        for (filter, sender) in subscribers.values() {
            if filter.matches(&event) {
                // Best-effort delivery: if the receiver is full or dropped,
                // we skip it rather than blocking the publisher.
                let _ = sender.try_send(event.clone());
            }
        }
        Ok(())
    }

    async fn subscribe(
        &self,
        filter: EventFilter,
    ) -> DafResult<(SubscriptionId, tokio::sync::mpsc::Receiver<Event>)> {
        let id = SubscriptionId::new();
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        self.subscribers.lock().insert(id, (filter, tx));
        Ok((id, rx))
    }

    async fn unsubscribe(&self, id: SubscriptionId) -> DafResult<()> {
        self.subscribers.lock().remove(&id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent() -> AgentId {
        AgentId::new()
    }

    #[test]
    fn event_creation() {
        let agent = test_agent();
        let event = Event::new(EventKind::AgentSpawned, agent)
            .with_data(serde_json::json!({"name": "worker-1"}))
            .with_tag("critical")
            .with_tag("team:platform");

        assert_eq!(event.kind, EventKind::AgentSpawned);
        assert_eq!(event.source, agent);
        assert!(event.has_tag("critical"));
        assert!(event.has_tag("team:platform"));
        assert!(!event.has_tag("missing"));
    }

    #[test]
    fn event_display() {
        let event = Event::new(EventKind::TaskFailed, test_agent())
            .with_tag("retry");
        let display = event.to_string();
        assert!(display.contains("task_failed"));
        assert!(display.contains("tags=[retry]"));
    }

    #[test]
    fn event_serde_roundtrip() {
        let event = Event::new(EventKind::Custom("deploy".into()), test_agent())
            .with_data(serde_json::json!({"version": 42}));
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, EventKind::Custom("deploy".into()));
        assert_eq!(back.data["version"], 42);
    }

    #[test]
    fn filter_all_matches_everything() {
        let filter = EventFilter::all();
        let event = Event::new(EventKind::AgentSpawned, test_agent());
        assert!(filter.matches(&event));
    }

    #[test]
    fn filter_by_kind() {
        let filter = EventFilter::kind(EventKind::TaskCompleted);

        let match_event = Event::new(EventKind::TaskCompleted, test_agent());
        let miss_event = Event::new(EventKind::TaskFailed, test_agent());

        assert!(filter.matches(&match_event));
        assert!(!filter.matches(&miss_event));
    }

    #[test]
    fn filter_by_source() {
        let target = test_agent();
        let filter = EventFilter::all().with_source(target);

        let match_event = Event::new(EventKind::AgentSpawned, target);
        let miss_event = Event::new(EventKind::AgentSpawned, test_agent());

        assert!(filter.matches(&match_event));
        assert!(!filter.matches(&miss_event));
    }

    #[test]
    fn filter_by_tag() {
        let filter = EventFilter::all().with_tag("critical");

        let match_event = Event::new(EventKind::TaskFailed, test_agent())
            .with_tag("critical");
        let miss_event = Event::new(EventKind::TaskFailed, test_agent())
            .with_tag("info");

        assert!(filter.matches(&match_event));
        assert!(!filter.matches(&miss_event));
    }

    #[test]
    fn filter_combined() {
        let agent = test_agent();
        let filter = EventFilter::kind(EventKind::TaskFailed)
            .with_source(agent)
            .with_tag("retry");

        // Must match all three criteria.
        let event = Event::new(EventKind::TaskFailed, agent).with_tag("retry");
        assert!(filter.matches(&event));

        // Wrong kind.
        let event2 = Event::new(EventKind::TaskCompleted, agent).with_tag("retry");
        assert!(!filter.matches(&event2));

        // Wrong source.
        let event3 = Event::new(EventKind::TaskFailed, test_agent()).with_tag("retry");
        assert!(!filter.matches(&event3));
    }

    #[tokio::test]
    async fn in_memory_bus_publish_subscribe() {
        let bus = InMemoryEventBus::new();
        let agent = test_agent();

        let (sub_id, mut rx) = bus
            .subscribe(EventFilter::kind(EventKind::TaskCompleted))
            .await
            .unwrap();

        assert_eq!(bus.subscriber_count(), 1);

        // Publish a matching event.
        let event = Event::new(EventKind::TaskCompleted, agent);
        bus.publish(event.clone()).await.unwrap();

        let received = rx.try_recv().unwrap();
        assert_eq!(received.id, event.id);

        // Publish a non-matching event.
        let miss = Event::new(EventKind::AgentSpawned, agent);
        bus.publish(miss).await.unwrap();
        assert!(rx.try_recv().is_err());

        // Unsubscribe.
        bus.unsubscribe(sub_id).await.unwrap();
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn in_memory_bus_multiple_subscribers() {
        let bus = InMemoryEventBus::new();
        let agent = test_agent();

        let (_s1, mut rx1) = bus.subscribe(EventFilter::all()).await.unwrap();
        let (_s2, mut rx2) = bus
            .subscribe(EventFilter::kind(EventKind::TaskFailed))
            .await
            .unwrap();

        let event = Event::new(EventKind::TaskFailed, agent);
        bus.publish(event).await.unwrap();

        // Both should receive it.
        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[test]
    fn event_kind_display() {
        assert_eq!(EventKind::AgentSpawned.to_string(), "agent_spawned");
        assert_eq!(
            EventKind::Custom("x".into()).to_string(),
            "custom:x"
        );
    }

    #[test]
    fn subscription_id_display() {
        let id = SubscriptionId::new();
        assert!(id.to_string().starts_with("sub-"));
    }
}
