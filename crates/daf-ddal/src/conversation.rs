//! # Conversation Tracking
//!
//! Tracks multi-turn dialogues between agents. A [`Conversation`] is an
//! ordered sequence of [`Turn`]s exchanged by a set of participants, with
//! lifecycle management (pause, resume, archive) and the ability to fork
//! a conversation into a new thread while preserving history.
//!
//! ## Identifiers
//!
//! Every conversation receives a [`ConversationId`] backed by UUID v7, so
//! conversations sort chronologically and are globally unique across nodes.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use daf_core::AgentId;

/// Re-export for public API convenience.
pub use bytes::Bytes;

// ---------------------------------------------------------------------------
// Serde helper for bytes::Bytes
// ---------------------------------------------------------------------------

mod bytes_serde {
    use bytes::Bytes;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Bytes, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytes.as_ref())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Bytes, D::Error> {
        let vec: Vec<u8> = serde::Deserialize::deserialize(de)?;
        Ok(Bytes::from(vec))
    }
}

// ---------------------------------------------------------------------------
// ConversationId
// ---------------------------------------------------------------------------

/// Time-ordered conversation identifier backed by UUID v7.
///
/// Guarantees chronological ordering so that conversation lists can be
/// sorted by creation time without an additional timestamp column.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationId(Uuid);

impl ConversationId {
    /// Generate a new time-ordered conversation identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing [`Uuid`] as a [`ConversationId`].
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the inner [`Uuid`].
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ConversationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConversationId({})", &self.0.to_string()[..8])
    }
}

impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Turn
// ---------------------------------------------------------------------------

/// A single turn in a conversation — one agent speaking.
///
/// Turns are append-only: once recorded, they are never modified. The
/// `turn_number` field provides a monotonic sequence within the conversation,
/// independent of wall-clock timestamps (which may skew across nodes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// The agent that produced this turn.
    pub speaker: AgentId,
    /// Raw content of the turn. Interpretation depends on the conversation's
    /// payload format (text, binary, structured message, etc.).
    #[serde(with = "bytes_serde")]
    pub content: Bytes,
    /// Wall-clock time when the turn was recorded.
    pub timestamp: DateTime<Utc>,
    /// Zero-based monotonic position within the conversation.
    pub turn_number: u64,
    /// Arbitrary key-value metadata attached to this turn.
    ///
    /// Common keys: `"content_type"`, `"model"`, `"tool_call_id"`.
    pub metadata: HashMap<String, String>,
}

impl Turn {
    /// Create a new turn with the given speaker and content.
    ///
    /// Timestamp is set to `Utc::now()` and metadata is empty.
    pub fn new(speaker: AgentId, content: Bytes, turn_number: u64) -> Self {
        Self {
            speaker,
            content,
            timestamp: Utc::now(),
            turn_number,
            metadata: HashMap::new(),
        }
    }

    /// Attach a metadata key-value pair to this turn.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

impl fmt::Display for Turn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Turn(#{} by {} at {}, {} bytes)",
            self.turn_number,
            self.speaker,
            self.timestamp.format("%H:%M:%S"),
            self.content.len(),
        )
    }
}

// ---------------------------------------------------------------------------
// ConversationState
// ---------------------------------------------------------------------------

/// Lifecycle state of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationState {
    /// Conversation is live — turns can be appended.
    Active,
    /// Conversation is temporarily suspended. No new turns are accepted
    /// until it is resumed.
    Paused,
    /// All participants have finished. The conversation is read-only.
    Completed,
    /// Conversation has been archived for long-term storage.
    Archived,
}

impl ConversationState {
    /// Returns `true` if the conversation accepts new turns.
    pub fn accepts_turns(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Returns `true` if the conversation has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Archived)
    }
}

impl fmt::Display for ConversationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Archived => "archived",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Conversation
// ---------------------------------------------------------------------------

/// A tracked multi-turn dialogue between agents.
///
/// Conversations are the primary unit of structured interaction in DDAL.
/// They maintain an ordered turn log, participant list, and lifecycle state.
///
/// # Forking
///
/// A conversation can be [`fork`](Conversation::fork)ed to create a new
/// conversation that inherits all turns up to the fork point. The original
/// conversation is unaffected — this enables branching exploration patterns
/// common in agent orchestration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique identifier for this conversation.
    pub id: ConversationId,
    /// Agents participating in this conversation.
    pub participants: Vec<AgentId>,
    /// Ordered sequence of turns.
    pub turns: Vec<Turn>,
    /// When the conversation was started.
    pub started_at: DateTime<Utc>,
    /// When the conversation ended, if it has.
    pub ended_at: Option<DateTime<Utc>>,
    /// Current lifecycle state.
    pub state: ConversationState,
    /// Human-readable topic or purpose of the conversation.
    pub topic: String,
}

impl Conversation {
    /// Start a new conversation with the given participants and topic.
    pub fn new(participants: Vec<AgentId>, topic: impl Into<String>) -> Self {
        Self {
            id: ConversationId::new(),
            participants,
            turns: Vec::new(),
            started_at: Utc::now(),
            ended_at: None,
            state: ConversationState::Active,
            topic: topic.into(),
        }
    }

    /// Append a turn to the conversation.
    ///
    /// The turn number is assigned automatically based on the current turn
    /// count. Returns the assigned turn number.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the conversation is not in the [`Active`](ConversationState::Active)
    /// state.
    pub fn add_turn(
        &mut self,
        speaker: AgentId,
        content: Bytes,
    ) -> Result<u64, ConversationError> {
        if !self.state.accepts_turns() {
            return Err(ConversationError::NotActive {
                conversation_id: self.id,
                state: self.state,
            });
        }

        let turn_number = self.turns.len() as u64;
        let turn = Turn::new(speaker, content, turn_number);
        self.turns.push(turn);
        Ok(turn_number)
    }

    /// Append a turn with metadata.
    ///
    /// Convenience method that combines [`add_turn`](Conversation::add_turn)
    /// with metadata attachment in a single call.
    pub fn add_turn_with_metadata(
        &mut self,
        speaker: AgentId,
        content: Bytes,
        metadata: HashMap<String, String>,
    ) -> Result<u64, ConversationError> {
        if !self.state.accepts_turns() {
            return Err(ConversationError::NotActive {
                conversation_id: self.id,
                state: self.state,
            });
        }

        let turn_number = self.turns.len() as u64;
        let mut turn = Turn::new(speaker, content, turn_number);
        turn.metadata = metadata;
        self.turns.push(turn);
        Ok(turn_number)
    }

    /// Build a plain-text transcript of all turns.
    ///
    /// Each turn is rendered as `"[speaker]: content"` on its own line.
    /// Binary content is lossy-converted to UTF-8.
    pub fn get_transcript(&self) -> String {
        let mut transcript = String::new();
        for turn in &self.turns {
            let text = String::from_utf8_lossy(&turn.content);
            transcript.push_str(&format!("[{}]: {}\n", turn.speaker, text));
        }
        transcript
    }

    /// Fork this conversation into a new one, copying all turns up to
    /// (but not including) the current length.
    ///
    /// The forked conversation gets a new [`ConversationId`] and starts in
    /// [`Active`](ConversationState::Active) state. The original conversation
    /// is unmodified.
    pub fn fork(&self, new_topic: impl Into<String>) -> Self {
        Self {
            id: ConversationId::new(),
            participants: self.participants.clone(),
            turns: self.turns.clone(),
            started_at: Utc::now(),
            ended_at: None,
            state: ConversationState::Active,
            topic: new_topic.into(),
        }
    }

    /// Number of agents participating in this conversation.
    pub fn participants_count(&self) -> usize {
        self.participants.len()
    }

    /// Elapsed duration since the conversation started.
    ///
    /// If the conversation has ended, returns the duration from start to end.
    /// Otherwise, returns the duration from start to now.
    pub fn duration(&self) -> chrono::Duration {
        let end = self.ended_at.unwrap_or_else(Utc::now);
        end - self.started_at
    }

    /// Return the most recent turn, if any.
    pub fn latest_turn(&self) -> Option<&Turn> {
        self.turns.last()
    }

    /// Number of turns in this conversation.
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Transition the conversation to [`Paused`](ConversationState::Paused).
    pub fn pause(&mut self) {
        self.state = ConversationState::Paused;
    }

    /// Resume a paused conversation back to [`Active`](ConversationState::Active).
    pub fn resume(&mut self) {
        if self.state == ConversationState::Paused {
            self.state = ConversationState::Active;
        }
    }

    /// Mark the conversation as completed. Sets `ended_at` to now.
    pub fn complete(&mut self) {
        self.state = ConversationState::Completed;
        self.ended_at = Some(Utc::now());
    }

    /// Archive the conversation.
    pub fn archive(&mut self) {
        self.state = ConversationState::Archived;
        if self.ended_at.is_none() {
            self.ended_at = Some(Utc::now());
        }
    }

    /// Merge turns from another conversation into this one.
    ///
    /// Incoming turns are re-numbered to continue from this conversation's
    /// current turn count. Participants from the other conversation are
    /// added if not already present.
    ///
    /// Returns the number of turns merged.
    ///
    /// # Errors
    ///
    /// Returns `Err` if this conversation is not [`Active`](ConversationState::Active).
    pub fn merge(&mut self, other: &Conversation) -> Result<u64, ConversationError> {
        if !self.state.accepts_turns() {
            return Err(ConversationError::NotActive {
                conversation_id: self.id,
                state: self.state,
            });
        }

        let base = self.turns.len() as u64;
        let mut merged = 0u64;

        for turn in &other.turns {
            let mut new_turn = turn.clone();
            new_turn.turn_number = base + merged;
            self.turns.push(new_turn);
            merged += 1;
        }

        // Add participants from the other conversation.
        for participant in &other.participants {
            if !self.participants.contains(participant) {
                self.participants.push(*participant);
            }
        }

        Ok(merged)
    }

    /// Total content bytes across all turns.
    pub fn total_content_bytes(&self) -> usize {
        self.turns.iter().map(|t| t.content.len()).sum()
    }

    /// Get all turns from a specific speaker.
    pub fn turns_by_speaker(&self, speaker: &AgentId) -> Vec<&Turn> {
        self.turns.iter().filter(|t| &t.speaker == speaker).collect()
    }

    /// Get a specific turn by number.
    pub fn get_turn(&self, turn_number: u64) -> Option<&Turn> {
        self.turns.get(turn_number as usize)
    }
}

impl fmt::Display for Conversation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Conversation({}, topic={:?}, turns={}, state={}, participants={})",
            self.id,
            self.topic,
            self.turns.len(),
            self.state,
            self.participants.len(),
        )
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors specific to conversation operations.
#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("conversation {conversation_id} is not active (state: {state})")]
    NotActive {
        conversation_id: ConversationId,
        state: ConversationState,
    },
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

    #[test]
    fn conversation_id_is_unique() {
        let id1 = ConversationId::new();
        let id2 = ConversationId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn conversation_id_display() {
        let id = ConversationId::new();
        let s = id.to_string();
        // UUID v7 string representation is 36 chars.
        assert_eq!(s.len(), 36);
    }

    #[test]
    fn new_conversation() {
        let (a, b) = agents();
        let conv = Conversation::new(vec![a, b], "code review");

        assert_eq!(conv.participants_count(), 2);
        assert_eq!(conv.topic, "code review");
        assert_eq!(conv.state, ConversationState::Active);
        assert_eq!(conv.turn_count(), 0);
        assert!(conv.ended_at.is_none());
    }

    #[test]
    fn add_turn_assigns_sequential_numbers() {
        let (a, b) = agents();
        let mut conv = Conversation::new(vec![a, b], "test");

        let n0 = conv.add_turn(a, Bytes::from_static(b"hello")).unwrap();
        let n1 = conv.add_turn(b, Bytes::from_static(b"hi")).unwrap();

        assert_eq!(n0, 0);
        assert_eq!(n1, 1);
        assert_eq!(conv.turn_count(), 2);
    }

    #[test]
    fn add_turn_fails_when_not_active() {
        let (a, _b) = agents();
        let mut conv = Conversation::new(vec![a], "test");
        conv.complete();

        let result = conv.add_turn(a, Bytes::from_static(b"late"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConversationError::NotActive { .. }
        ));
    }

    #[test]
    fn get_transcript() {
        let (a, b) = agents();
        let mut conv = Conversation::new(vec![a, b], "test");
        conv.add_turn(a, Bytes::from_static(b"hello")).unwrap();
        conv.add_turn(b, Bytes::from_static(b"world")).unwrap();

        let transcript = conv.get_transcript();
        assert!(transcript.contains("hello"));
        assert!(transcript.contains("world"));
        assert_eq!(transcript.lines().count(), 2);
    }

    #[test]
    fn fork_preserves_turns() {
        let (a, b) = agents();
        let mut conv = Conversation::new(vec![a, b], "original");
        conv.add_turn(a, Bytes::from_static(b"first")).unwrap();
        conv.add_turn(b, Bytes::from_static(b"second")).unwrap();

        let forked = conv.fork("forked topic");

        assert_ne!(forked.id, conv.id);
        assert_eq!(forked.topic, "forked topic");
        assert_eq!(forked.turn_count(), 2);
        assert_eq!(forked.state, ConversationState::Active);
        assert!(forked.ended_at.is_none());
        // Original is unmodified.
        assert_eq!(conv.topic, "original");
    }

    #[test]
    fn latest_turn() {
        let (a, _b) = agents();
        let mut conv = Conversation::new(vec![a], "test");

        assert!(conv.latest_turn().is_none());

        conv.add_turn(a, Bytes::from_static(b"one")).unwrap();
        conv.add_turn(a, Bytes::from_static(b"two")).unwrap();

        let latest = conv.latest_turn().unwrap();
        assert_eq!(latest.turn_number, 1);
        assert_eq!(latest.content, Bytes::from_static(b"two"));
    }

    #[test]
    fn duration_is_non_negative() {
        let (a, _b) = agents();
        let conv = Conversation::new(vec![a], "test");
        let dur = conv.duration();
        assert!(dur.num_milliseconds() >= 0);
    }

    #[test]
    fn lifecycle_transitions() {
        let (a, _b) = agents();
        let mut conv = Conversation::new(vec![a], "test");

        assert!(conv.state.accepts_turns());
        assert!(!conv.state.is_terminal());

        conv.pause();
        assert_eq!(conv.state, ConversationState::Paused);
        assert!(!conv.state.accepts_turns());

        conv.resume();
        assert_eq!(conv.state, ConversationState::Active);
        assert!(conv.state.accepts_turns());

        conv.complete();
        assert_eq!(conv.state, ConversationState::Completed);
        assert!(conv.state.is_terminal());
        assert!(conv.ended_at.is_some());
    }

    #[test]
    fn archive_sets_ended_at() {
        let (a, _b) = agents();
        let mut conv = Conversation::new(vec![a], "test");
        conv.archive();
        assert_eq!(conv.state, ConversationState::Archived);
        assert!(conv.ended_at.is_some());
    }

    #[test]
    fn turn_with_metadata() {
        let a = AgentId::new();
        let turn = Turn::new(a, Bytes::from_static(b"content"), 0)
            .with_metadata("content_type", "text/plain")
            .with_metadata("model", "claude-4");

        assert_eq!(turn.metadata.len(), 2);
        assert_eq!(turn.metadata.get("model").unwrap(), "claude-4");
    }

    #[test]
    fn conversation_display() {
        let a = AgentId::new();
        let conv = Conversation::new(vec![a], "test topic");
        let display = format!("{conv}");
        assert!(display.contains("Conversation("));
        assert!(display.contains("test topic"));
    }

    #[test]
    fn conversation_state_display() {
        assert_eq!(ConversationState::Active.to_string(), "active");
        assert_eq!(ConversationState::Paused.to_string(), "paused");
        assert_eq!(ConversationState::Completed.to_string(), "completed");
        assert_eq!(ConversationState::Archived.to_string(), "archived");
    }

    #[test]
    fn turn_display() {
        let a = AgentId::new();
        let turn = Turn::new(a, Bytes::from_static(b"hello world"), 3);
        let display = format!("{turn}");
        assert!(display.contains("#3"));
        assert!(display.contains("11 bytes"));
    }

    #[test]
    fn serde_round_trip_conversation_state() {
        let state = ConversationState::Active;
        let json = serde_json::to_string(&state).unwrap();
        let back: ConversationState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn serde_round_trip_conversation_id() {
        let id = ConversationId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: ConversationId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn merge_appends_turns() {
        let (a, _b) = agents();
        let mut main = Conversation::new(vec![a], "main");
        main.add_turn(a, Bytes::from_static(b"main-1")).unwrap();

        let c = AgentId::new();
        let mut branch = Conversation::new(vec![c], "branch");
        branch.add_turn(c, Bytes::from_static(b"branch-1")).unwrap();
        branch.add_turn(c, Bytes::from_static(b"branch-2")).unwrap();

        let merged_count = main.merge(&branch).unwrap();
        assert_eq!(merged_count, 2);
        assert_eq!(main.turn_count(), 3);
        // New participant should be added.
        assert!(main.participants.contains(&c));
        // Turn numbers should be sequential.
        assert_eq!(main.turns[1].turn_number, 1);
        assert_eq!(main.turns[2].turn_number, 2);
    }

    #[test]
    fn merge_fails_when_not_active() {
        let (a, _b) = agents();
        let mut main = Conversation::new(vec![a], "main");
        main.complete();

        let other = Conversation::new(vec![a], "other");
        let result = main.merge(&other);
        assert!(result.is_err());
    }

    #[test]
    fn total_content_bytes() {
        let (a, b) = agents();
        let mut conv = Conversation::new(vec![a, b], "test");
        conv.add_turn(a, Bytes::from_static(b"hello")).unwrap(); // 5
        conv.add_turn(b, Bytes::from_static(b"world!")).unwrap(); // 6
        assert_eq!(conv.total_content_bytes(), 11);
    }

    #[test]
    fn turns_by_speaker() {
        let (a, b) = agents();
        let mut conv = Conversation::new(vec![a, b], "test");
        conv.add_turn(a, Bytes::from_static(b"one")).unwrap();
        conv.add_turn(b, Bytes::from_static(b"two")).unwrap();
        conv.add_turn(a, Bytes::from_static(b"three")).unwrap();

        let a_turns = conv.turns_by_speaker(&a);
        assert_eq!(a_turns.len(), 2);
    }

    #[test]
    fn get_turn_by_number() {
        let (a, _b) = agents();
        let mut conv = Conversation::new(vec![a], "test");
        conv.add_turn(a, Bytes::from_static(b"zero")).unwrap();
        conv.add_turn(a, Bytes::from_static(b"one")).unwrap();

        assert!(conv.get_turn(0).is_some());
        assert!(conv.get_turn(1).is_some());
        assert!(conv.get_turn(99).is_none());
    }
}
