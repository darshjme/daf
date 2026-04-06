//! Log entry types for structured conversation logging.
//!
//! Every agent-to-agent message, internal event, or system observation is
//! captured as a [`LogEntry`]. A sequence of entries sharing a conversation ID
//! forms a [`ConversationLog`] — the canonical record of what agents discussed.

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// LogLevel
// ---------------------------------------------------------------------------

/// Severity / verbosity level for a log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trace => write!(f, "trace"),
            Self::Debug => write!(f, "debug"),
            Self::Info => write!(f, "info"),
            Self::Warn => write!(f, "warn"),
            Self::Error => write!(f, "error"),
        }
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

// ---------------------------------------------------------------------------
// ContentType
// ---------------------------------------------------------------------------

/// Discriminator for the `content` field of a [`LogEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    /// UTF-8 text (chat messages, prompts, completions).
    Text,
    /// Opaque binary blob (base64-encoded when serialized to JSON).
    Binary,
    /// Arbitrary structured JSON value.
    Structured,
}

impl Default for ContentType {
    fn default() -> Self {
        Self::Text
    }
}

// ---------------------------------------------------------------------------
// LogEntry
// ---------------------------------------------------------------------------

/// A single log entry — the atomic unit of conversation logging.
///
/// Every message, tool call, observation, or system event produces one entry.
/// Entries are immutable once written; the `id` is assigned at creation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unique identifier for this entry (UUID v7, time-ordered).
    pub id: Uuid,
    /// When the entry was created.
    pub timestamp: DateTime<Utc>,
    /// Agent that produced this entry.
    pub source_agent: AgentId,
    /// Agent this entry is addressed to (`None` for broadcasts / system events).
    pub target_agent: Option<AgentId>,
    /// Conversation this entry belongs to.
    pub conversation_id: Uuid,
    /// Monotonically increasing turn number within the conversation.
    pub turn_number: u64,
    /// What kind of content the `content` field holds.
    pub content_type: ContentType,
    /// The actual payload — text, binary (base64), or structured JSON.
    pub content: Value,
    /// Free-form metadata attached by the agent or the framework.
    pub metadata: Value,
    /// Searchable tags for filtering and aggregation.
    pub tags: Vec<String>,
    /// Severity level.
    pub level: LogLevel,
    /// BLAKE3 hash of the content field for integrity verification.
    pub content_hash: String,
}

impl LogEntry {
    /// Create a new log entry with the given fields. Computes `content_hash`
    /// automatically from the serialized content.
    pub fn new(
        source_agent: AgentId,
        target_agent: Option<AgentId>,
        conversation_id: Uuid,
        turn_number: u64,
        content_type: ContentType,
        content: Value,
        level: LogLevel,
    ) -> Self {
        let content_bytes = serde_json::to_vec(&content).unwrap_or_default();
        let hash = blake3::hash(&content_bytes);

        Self {
            id: Uuid::now_v7(),
            timestamp: Utc::now(),
            source_agent,
            target_agent,
            conversation_id,
            turn_number,
            content_type,
            content,
            metadata: Value::Object(serde_json::Map::new()),
            tags: Vec::new(),
            level,
            content_hash: hash.to_hex().to_string(),
        }
    }

    /// Convenience constructor for a simple text message between two agents.
    pub fn text(
        source: AgentId,
        target: AgentId,
        conversation_id: Uuid,
        turn: u64,
        text: &str,
    ) -> Self {
        Self::new(
            source,
            Some(target),
            conversation_id,
            turn,
            ContentType::Text,
            Value::String(text.to_owned()),
            LogLevel::Info,
        )
    }

    /// Add a tag and return self (builder pattern).
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set metadata and return self.
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Verify that `content_hash` matches the current `content`.
    pub fn verify_integrity(&self) -> bool {
        let content_bytes = serde_json::to_vec(&self.content).unwrap_or_default();
        let hash = blake3::hash(&content_bytes);
        hash.to_hex().to_string() == self.content_hash
    }
}

// ---------------------------------------------------------------------------
// ConversationLog
// ---------------------------------------------------------------------------

/// A complete conversation log — all entries from a single conversation,
/// plus metadata about participants and timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationLog {
    /// The conversation identifier shared by all entries.
    pub conversation_id: Uuid,
    /// Agents that participated in this conversation.
    pub participants: Vec<AgentId>,
    /// Ordered sequence of log entries (by turn number).
    pub entries: Vec<LogEntry>,
    /// When the conversation started.
    pub started_at: DateTime<Utc>,
    /// When the conversation ended (`None` if still active).
    pub ended_at: Option<DateTime<Utc>>,
    /// Optional human-readable summary of the conversation.
    pub summary: Option<String>,
}

impl ConversationLog {
    /// Start a new empty conversation log.
    pub fn new(conversation_id: Uuid, participants: Vec<AgentId>) -> Self {
        Self {
            conversation_id,
            participants,
            entries: Vec::new(),
            started_at: Utc::now(),
            ended_at: None,
            summary: None,
        }
    }

    /// Append an entry to the conversation.
    pub fn push(&mut self, entry: LogEntry) {
        // Track new participants.
        if !self.participants.contains(&entry.source_agent) {
            self.participants.push(entry.source_agent);
        }
        if let Some(target) = entry.target_agent {
            if !self.participants.contains(&target) {
                self.participants.push(target);
            }
        }
        self.entries.push(entry);
    }

    /// Mark the conversation as finished.
    pub fn close(&mut self, summary: Option<String>) {
        self.ended_at = Some(Utc::now());
        self.summary = summary;
    }

    /// Total number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Duration of the conversation (from first to last entry).
    pub fn duration(&self) -> Option<chrono::Duration> {
        if self.entries.len() < 2 {
            return None;
        }
        let first = self.entries.first().map(|e| e.timestamp)?;
        let last = self.entries.last().map(|e| e.timestamp)?;
        Some(last - first)
    }

    /// Filter entries by agent.
    pub fn entries_by_agent(&self, agent: &AgentId) -> Vec<&LogEntry> {
        self.entries
            .iter()
            .filter(|e| &e.source_agent == agent)
            .collect()
    }

    /// Filter entries by level (returns entries at or above the given level).
    pub fn entries_at_level(&self, min_level: LogLevel) -> Vec<&LogEntry> {
        self.entries
            .iter()
            .filter(|e| e.level >= min_level)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_pair() -> (AgentId, AgentId) {
        (AgentId::new(), AgentId::new())
    }

    #[test]
    fn entry_creation_and_integrity() {
        let (src, tgt) = agent_pair();
        let conv = Uuid::now_v7();
        let entry = LogEntry::text(src, tgt, conv, 1, "hello world");

        assert_eq!(entry.conversation_id, conv);
        assert_eq!(entry.turn_number, 1);
        assert_eq!(entry.level, LogLevel::Info);
        assert!(entry.verify_integrity());
    }

    #[test]
    fn entry_integrity_detects_tampering() {
        let (src, tgt) = agent_pair();
        let conv = Uuid::now_v7();
        let mut entry = LogEntry::text(src, tgt, conv, 1, "original");
        assert!(entry.verify_integrity());

        // Tamper with content.
        entry.content = Value::String("tampered".to_string());
        assert!(!entry.verify_integrity());
    }

    #[test]
    fn conversation_log_tracks_participants() {
        let (a, b) = agent_pair();
        let c = AgentId::new();
        let conv_id = Uuid::now_v7();

        let mut log = ConversationLog::new(conv_id, vec![a, b]);
        assert_eq!(log.participants.len(), 2);

        // Push an entry from a new agent — should auto-add.
        let entry = LogEntry::text(c, a, conv_id, 1, "joining");
        log.push(entry);
        assert_eq!(log.participants.len(), 3);
        assert!(log.participants.contains(&c));
    }

    #[test]
    fn conversation_close() {
        let conv_id = Uuid::now_v7();
        let mut log = ConversationLog::new(conv_id, vec![]);
        assert!(log.ended_at.is_none());

        log.close(Some("All done.".into()));
        assert!(log.ended_at.is_some());
        assert_eq!(log.summary.as_deref(), Some("All done."));
    }

    #[test]
    fn log_level_ordering() {
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn entry_tags_builder() {
        let (src, tgt) = agent_pair();
        let entry = LogEntry::text(src, tgt, Uuid::now_v7(), 0, "msg")
            .with_tag("important")
            .with_tag("review");
        assert_eq!(entry.tags, vec!["important", "review"]);
    }

    #[test]
    fn roundtrip_serialization() {
        let (src, tgt) = agent_pair();
        let entry = LogEntry::text(src, tgt, Uuid::now_v7(), 1, "serialize me");
        let json = serde_json::to_string(&entry).unwrap();
        let back: LogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, entry.id);
        assert_eq!(back.content, entry.content);
        assert!(back.verify_integrity());
    }
}
