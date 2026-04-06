//! Agent-to-agent handoff — seamless context transfer between specialists.
//!
//! When one agent transfers ownership of a task or context to another,
//! a [`Handoff`] record captures everything the receiving agent needs:
//! the structured context, conversation history, relevant memories, and
//! task state. The [`HandoffManager`] coordinates the transfer and
//! maintains a complete audit trail.
//!
//! Handoffs are essential for the specialist routing model — when a Planner
//! finishes decomposing a goal, it hands off to Builders; when a Builder
//! finishes, it hands off to a Tester. Each handoff preserves continuity.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

use daf_core::AgentId;

// ---------------------------------------------------------------------------
// HandoffId
// ---------------------------------------------------------------------------

/// Unique identifier for a handoff event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HandoffId(pub Uuid);

impl HandoffId {
    /// Generate a new time-ordered handoff identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for HandoffId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for HandoffId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "handoff:{}", &self.0.to_string()[..8])
    }
}

// ---------------------------------------------------------------------------
// HandoffState
// ---------------------------------------------------------------------------

/// Lifecycle state of a handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    /// Handoff has been initiated but not yet accepted by the receiver.
    Initiated,
    /// Receiver has accepted and is loading the context.
    Accepted,
    /// Handoff completed successfully — receiver has full context.
    Completed,
    /// Handoff was rejected by the receiver (e.g., incompatible capabilities).
    Rejected,
    /// Handoff failed due to an error during transfer.
    Failed,
}

impl fmt::Display for HandoffState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Initiated => "initiated",
            Self::Accepted => "accepted",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// ConversationEntry
// ---------------------------------------------------------------------------

/// A single entry in the conversation history transferred during handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// Who said it (agent ID or "user").
    pub speaker: String,
    /// The content of the message.
    pub content: String,
    /// When it was said.
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Handoff
// ---------------------------------------------------------------------------

/// A context handoff from one agent to another.
///
/// Contains everything the receiving agent needs to seamlessly continue
/// work that the source agent was doing — structured context, conversation
/// history, memory references, and arbitrary metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    /// Unique handoff identifier.
    pub id: HandoffId,
    /// Agent transferring context.
    pub from_agent: AgentId,
    /// Agent receiving context.
    pub to_agent: AgentId,
    /// Structured context being transferred (task state, intermediate results).
    pub context: serde_json::Value,
    /// Current state of the task being handed off (serialized).
    pub task_state: Option<serde_json::Value>,
    /// Conversation history relevant to this task, in chronological order.
    pub conversation_history: Vec<ConversationEntry>,
    /// Memory IDs that the receiving agent should load for context.
    pub memory_references: Vec<Uuid>,
    /// Current lifecycle state of the handoff.
    pub state: HandoffState,
    /// When the handoff was initiated.
    pub initiated_at: DateTime<Utc>,
    /// When the handoff completed (accepted or rejected).
    pub completed_at: Option<DateTime<Utc>>,
    /// Reason for rejection or failure, if applicable.
    pub error: Option<String>,
    /// Arbitrary metadata (reason for handoff, priority, etc.).
    pub metadata: HashMap<String, String>,
}

impl Handoff {
    /// Create a new handoff from one agent to another with the given context.
    pub fn new(
        from: AgentId,
        to: AgentId,
        context: serde_json::Value,
    ) -> Self {
        Self {
            id: HandoffId::new(),
            from_agent: from,
            to_agent: to,
            context,
            task_state: None,
            conversation_history: Vec::new(),
            memory_references: Vec::new(),
            state: HandoffState::Initiated,
            initiated_at: Utc::now(),
            completed_at: None,
            error: None,
            metadata: HashMap::new(),
        }
    }

    /// Attach task state to the handoff.
    pub fn with_task_state(mut self, state: serde_json::Value) -> Self {
        self.task_state = Some(state);
        self
    }

    /// Attach conversation history.
    pub fn with_conversation(mut self, history: Vec<ConversationEntry>) -> Self {
        self.conversation_history = history;
        self
    }

    /// Add a single conversation entry.
    pub fn add_conversation_entry(&mut self, speaker: impl Into<String>, content: impl Into<String>) {
        self.conversation_history.push(ConversationEntry {
            speaker: speaker.into(),
            content: content.into(),
            timestamp: Utc::now(),
        });
    }

    /// Add memory references that the receiver should load.
    pub fn with_memories(mut self, memory_ids: Vec<Uuid>) -> Self {
        self.memory_references = memory_ids;
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Mark the handoff as accepted.
    pub fn accept(&mut self) {
        self.state = HandoffState::Accepted;
    }

    /// Mark the handoff as completed.
    pub fn complete(&mut self) {
        self.state = HandoffState::Completed;
        self.completed_at = Some(Utc::now());
    }

    /// Mark the handoff as rejected.
    pub fn reject(&mut self, reason: impl Into<String>) {
        self.state = HandoffState::Rejected;
        self.completed_at = Some(Utc::now());
        self.error = Some(reason.into());
    }

    /// Mark the handoff as failed.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.state = HandoffState::Failed;
        self.completed_at = Some(Utc::now());
        self.error = Some(reason.into());
    }

    /// Duration of the handoff (from initiation to completion).
    pub fn duration(&self) -> Option<std::time::Duration> {
        let end = self.completed_at?;
        (end - self.initiated_at).to_std().ok()
    }
}

impl fmt::Display for Handoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Handoff({id} {from} -> {to}, {state})",
            id = self.id,
            from = self.from_agent,
            to = self.to_agent,
            state = self.state,
        )
    }
}

/// Alias for backward compatibility and clarity in audit contexts.
pub type HandoffRecord = Handoff;

// ---------------------------------------------------------------------------
// HandoffManager
// ---------------------------------------------------------------------------

/// Manages handoff lifecycle, coordination, and audit logging.
///
/// The manager is the central point for initiating, tracking, and querying
/// handoffs. It maintains a complete history of all handoffs for
/// observability, debugging, and retrospective analysis.
#[derive(Debug, Clone)]
pub struct HandoffManager {
    /// All recorded handoffs indexed by their ID.
    records: Arc<DashMap<HandoffId, HandoffRecord>>,
    /// Index: agent ID -> handoffs where this agent was the source.
    outbound_index: Arc<DashMap<AgentId, Vec<HandoffId>>>,
    /// Index: agent ID -> handoffs where this agent was the target.
    inbound_index: Arc<DashMap<AgentId, Vec<HandoffId>>>,
}

impl HandoffManager {
    /// Create a new handoff manager.
    pub fn new() -> Self {
        Self {
            records: Arc::new(DashMap::new()),
            outbound_index: Arc::new(DashMap::new()),
            inbound_index: Arc::new(DashMap::new()),
        }
    }

    /// Record and initiate a handoff.
    ///
    /// The handoff is stored and indexed for both the source and target
    /// agents. Returns the handoff ID for tracking.
    pub fn record(&self, handoff: Handoff) -> HandoffId {
        let id = handoff.id;
        let from = handoff.from_agent;
        let to = handoff.to_agent;

        info!(
            handoff = %id,
            from = %from,
            to = %to,
            "recording handoff"
        );

        self.outbound_index.entry(from).or_default().push(id);
        self.inbound_index.entry(to).or_default().push(id);
        self.records.insert(id, handoff);

        id
    }

    /// Look up a handoff by ID.
    pub fn get(&self, id: &HandoffId) -> Option<HandoffRecord> {
        self.records.get(id).map(|r| r.clone())
    }

    /// Update the state of a handoff (accept, complete, reject, fail).
    pub fn update(&self, id: &HandoffId, f: impl FnOnce(&mut Handoff)) -> bool {
        if let Some(mut entry) = self.records.get_mut(id) {
            f(entry.value_mut());
            true
        } else {
            false
        }
    }

    /// Get all handoffs where the given agent was the source.
    pub fn outbound_handoffs(&self, agent_id: &AgentId) -> Vec<HandoffRecord> {
        self.outbound_index
            .get(agent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all handoffs where the given agent was the target.
    pub fn inbound_handoffs(&self, agent_id: &AgentId) -> Vec<HandoffRecord> {
        self.inbound_index
            .get(agent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all handoffs in a given state.
    pub fn by_state(&self, state: HandoffState) -> Vec<HandoffRecord> {
        self.records
            .iter()
            .filter(|r| r.state == state)
            .map(|r| r.clone())
            .collect()
    }

    /// Total number of recorded handoffs.
    pub fn count(&self) -> usize {
        self.records.len()
    }

    /// Number of handoffs currently in-flight (initiated or accepted).
    pub fn in_flight(&self) -> usize {
        self.records
            .iter()
            .filter(|r| matches!(r.state, HandoffState::Initiated | HandoffState::Accepted))
            .count()
    }

    /// Export the full audit trail as a JSON-serializable vector.
    pub fn audit_trail(&self) -> Vec<HandoffRecord> {
        let mut records: Vec<_> = self.records.iter().map(|r| r.clone()).collect();
        records.sort_by_key(|r| r.initiated_at);
        records
    }
}

impl Default for HandoffManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_lifecycle() {
        let from = AgentId::new();
        let to = AgentId::new();

        let mut handoff = Handoff::new(from, to, serde_json::json!({"task": "build"}));
        assert_eq!(handoff.state, HandoffState::Initiated);

        handoff.accept();
        assert_eq!(handoff.state, HandoffState::Accepted);

        handoff.complete();
        assert_eq!(handoff.state, HandoffState::Completed);
        assert!(handoff.completed_at.is_some());
    }

    #[test]
    fn handoff_rejection() {
        let mut handoff = Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!(null));
        handoff.reject("incompatible capabilities");
        assert_eq!(handoff.state, HandoffState::Rejected);
        assert_eq!(handoff.error.as_deref(), Some("incompatible capabilities"));
    }

    #[test]
    fn handoff_with_conversation() {
        let mut handoff = Handoff::new(
            AgentId::new(),
            AgentId::new(),
            serde_json::json!({"module": "auth"}),
        );

        handoff.add_conversation_entry("planner", "Decomposed auth into 3 tasks");
        handoff.add_conversation_entry("builder", "Starting implementation");

        assert_eq!(handoff.conversation_history.len(), 2);
        assert_eq!(handoff.conversation_history[0].speaker, "planner");
    }

    #[test]
    fn handoff_builder_chain() {
        let handoff = Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("ctx"))
            .with_task_state(serde_json::json!({"progress": 50}))
            .with_memories(vec![Uuid::now_v7(), Uuid::now_v7()])
            .with_metadata("reason", "specialist routing");

        assert!(handoff.task_state.is_some());
        assert_eq!(handoff.memory_references.len(), 2);
        assert_eq!(handoff.metadata.get("reason").unwrap(), "specialist routing");
    }

    #[test]
    fn manager_record_and_lookup() {
        let mgr = HandoffManager::new();
        let handoff = Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("data"));
        let id = mgr.record(handoff);

        assert_eq!(mgr.count(), 1);
        let retrieved = mgr.get(&id).unwrap();
        assert_eq!(retrieved.id, id);
    }

    #[test]
    fn manager_outbound_inbound_index() {
        let mgr = HandoffManager::new();
        let from = AgentId::new();
        let to = AgentId::new();

        mgr.record(Handoff::new(from, to, serde_json::json!("h1")));
        mgr.record(Handoff::new(from, AgentId::new(), serde_json::json!("h2")));

        assert_eq!(mgr.outbound_handoffs(&from).len(), 2);
        assert_eq!(mgr.inbound_handoffs(&to).len(), 1);
    }

    #[test]
    fn manager_update_state() {
        let mgr = HandoffManager::new();
        let handoff = Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("x"));
        let id = mgr.record(handoff);

        mgr.update(&id, |h| h.complete());

        let updated = mgr.get(&id).unwrap();
        assert_eq!(updated.state, HandoffState::Completed);
    }

    #[test]
    fn manager_by_state() {
        let mgr = HandoffManager::new();
        let mut h1 = Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("a"));
        h1.complete();
        mgr.record(h1);
        mgr.record(Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("b")));

        assert_eq!(mgr.by_state(HandoffState::Completed).len(), 1);
        assert_eq!(mgr.by_state(HandoffState::Initiated).len(), 1);
    }

    #[test]
    fn manager_in_flight() {
        let mgr = HandoffManager::new();
        mgr.record(Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("a")));
        let id = mgr.record(Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("b")));
        mgr.update(&id, |h| h.complete());

        assert_eq!(mgr.in_flight(), 1);
    }

    #[test]
    fn audit_trail_sorted() {
        let mgr = HandoffManager::new();
        mgr.record(Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("first")));
        std::thread::sleep(std::time::Duration::from_millis(2));
        mgr.record(Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("second")));

        let trail = mgr.audit_trail();
        assert_eq!(trail.len(), 2);
        assert!(trail[0].initiated_at <= trail[1].initiated_at);
    }

    #[test]
    fn handoff_display() {
        let handoff = Handoff::new(AgentId::new(), AgentId::new(), serde_json::json!("x"));
        let display = handoff.to_string();
        assert!(display.contains("Handoff("));
        assert!(display.contains("->"));
        assert!(display.contains("initiated"));
    }

    #[test]
    fn handoff_id_display() {
        let id = HandoffId::new();
        let display = id.to_string();
        assert!(display.starts_with("handoff:"));
    }
}
