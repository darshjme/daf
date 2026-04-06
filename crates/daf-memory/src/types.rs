//! Core memory types and identifiers.
//!
//! Defines the fundamental data structures that represent memories within
//! the DAF agent framework. Every memory has an identity, a tier (how hot
//! it is), a kind (what it represents), and rich metadata for lifecycle
//! management.

use chrono::{DateTime, Duration, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// MemoryId
// ---------------------------------------------------------------------------

/// Globally unique, time-ordered identifier for a single memory entry.
///
/// Uses UUID v7 so that identifiers are sortable by creation time,
/// enabling efficient range scans on time-based queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct MemoryId(pub Uuid);

impl MemoryId {
    /// Create a new time-ordered memory identifier (UUID v7).
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mem:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// MemoryTier
// ---------------------------------------------------------------------------

/// Which tier a memory resides in, inspired by CPU cache hierarchy and
/// Mohini's 3-tier memory architecture.
///
/// Memories are promoted to hotter tiers when frequently accessed and
/// demoted to colder tiers as they age without use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum MemoryTier {
    /// Instant recall, sub-millisecond. Currently active context and
    /// frequently reinforced knowledge. Stored in-memory or sled.
    Hot,
    /// Recent memories, sub-10ms retrieval. Accessed within the last
    /// few sessions but not in the immediate working set.
    Warm,
    /// Archival storage, sub-100ms retrieval. Old memories preserved
    /// for completeness. Stored in RocksDB or on disk.
    Cold,
}

impl MemoryTier {
    /// Target retrieval latency for this tier.
    pub fn target_latency_ms(&self) -> u64 {
        match self {
            Self::Hot => 1,
            Self::Warm => 10,
            Self::Cold => 100,
        }
    }
}

impl fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hot => write!(f, "hot"),
            Self::Warm => write!(f, "warm"),
            Self::Cold => write!(f, "cold"),
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryKind
// ---------------------------------------------------------------------------

/// The semantic category of a memory, mapping to cognitive science concepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryKind {
    /// What happened — records of events, interactions, decisions.
    /// E.g., "User asked me to refactor the auth module on 2026-04-01."
    Episodic,
    /// Facts and knowledge — distilled truths extracted from experience.
    /// E.g., "The auth module uses bcrypt for password hashing."
    Semantic,
    /// How to do things — procedures, workflows, skills.
    /// E.g., "To deploy, run `cargo build --release && docker push`."
    Procedural,
    /// Current context — the agent's active working set.
    /// Transient, not persisted across sessions unless explicitly saved.
    Working,
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episodic => write!(f, "episodic"),
            Self::Semantic => write!(f, "semantic"),
            Self::Procedural => write!(f, "procedural"),
            Self::Working => write!(f, "working"),
        }
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// A single memory entry — the atomic unit of the memory system.
///
/// Memories carry content (as a flexible JSON value), metadata for lifecycle
/// management, and linking information to trace provenance back to agents
/// and conversations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Unique identifier.
    pub id: MemoryId,
    /// Semantic category.
    pub kind: MemoryKind,
    /// Current storage tier.
    pub tier: MemoryTier,
    /// The memory's content — structured as JSON for flexibility.
    pub content: serde_json::Value,
    /// BLAKE3 hash of an embedding vector (for future vector search integration).
    /// `None` if no embedding has been computed yet.
    pub embedding_hash: Option<String>,
    /// When this memory was first created.
    pub created_at: DateTime<Utc>,
    /// When this memory was last accessed (read or recalled).
    pub last_accessed: DateTime<Utc>,
    /// How many times this memory has been accessed.
    pub access_count: u64,
    /// Importance score in `[0.0, 1.0]`. Higher means more likely to be
    /// retained and promoted to hotter tiers. Decays over time unless
    /// reinforced by access.
    pub importance: f64,
    /// Free-form tags for categorization and filtering.
    pub tags: Vec<String>,
    /// Which agent created this memory.
    pub source_agent: Option<AgentId>,
    /// Which conversation produced this memory.
    pub source_conversation: Option<Uuid>,
    /// Time-to-live. If set, the memory expires after this duration from
    /// creation. `None` means the memory lives forever (subject to
    /// compaction and tier demotion, but never auto-deleted).
    pub ttl: Option<Duration>,
}

impl Memory {
    /// Create a new memory with sensible defaults.
    ///
    /// Only `kind` and `content` are required; everything else gets defaults
    /// that can be overridden via the builder methods.
    pub fn new(kind: MemoryKind, content: serde_json::Value) -> Self {
        let now = Utc::now();
        Self {
            id: MemoryId::new(),
            kind,
            tier: MemoryTier::Warm,
            content,
            embedding_hash: None,
            created_at: now,
            last_accessed: now,
            access_count: 0,
            importance: 0.5,
            tags: Vec::new(),
            source_agent: None,
            source_conversation: None,
            ttl: None,
        }
    }

    /// Set the importance score, clamped to `[0.0, 1.0]`.
    pub fn with_importance(mut self, importance: f64) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Set the initial tier.
    pub fn with_tier(mut self, tier: MemoryTier) -> Self {
        self.tier = tier;
        self
    }

    /// Add tags.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Set the source agent.
    pub fn with_agent(mut self, agent: AgentId) -> Self {
        self.source_agent = Some(agent);
        self
    }

    /// Set the source conversation.
    pub fn with_conversation(mut self, conv: Uuid) -> Self {
        self.source_conversation = Some(conv);
        self
    }

    /// Set time-to-live.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Set the embedding hash.
    pub fn with_embedding_hash(mut self, hash: String) -> Self {
        self.embedding_hash = Some(hash);
        self
    }

    /// Record an access, bumping the count and updating `last_accessed`.
    pub fn record_access(&mut self) {
        self.access_count += 1;
        self.last_accessed = Utc::now();
    }

    /// Check if this memory has expired based on its TTL.
    pub fn is_expired(&self) -> bool {
        if let Some(ttl) = self.ttl {
            Utc::now() > self.created_at + ttl
        } else {
            false
        }
    }

    /// Age of this memory since creation.
    pub fn age(&self) -> Duration {
        Utc::now() - self.created_at
    }

    /// Time since last access.
    pub fn time_since_access(&self) -> Duration {
        Utc::now() - self.last_accessed
    }

    /// Compute a composite relevance score combining recency, frequency, and importance.
    ///
    /// This is the core ranking function used by recall queries.
    /// - `recency_weight`: how much to weight recency (0.0-1.0)
    /// - `frequency_weight`: how much to weight access frequency (0.0-1.0)
    /// - `importance_weight`: how much to weight raw importance (0.0-1.0)
    pub fn relevance_score(
        &self,
        recency_weight: f64,
        frequency_weight: f64,
        importance_weight: f64,
    ) -> f64 {
        let total_weight = recency_weight + frequency_weight + importance_weight;
        if total_weight == 0.0 {
            return 0.0;
        }

        // Recency: exponential decay over hours since last access.
        let hours_since_access = self
            .time_since_access()
            .num_seconds()
            .max(0) as f64
            / 3600.0;
        let recency = (-hours_since_access / 168.0).exp(); // half-life ~1 week

        // Frequency: logarithmic scaling of access count.
        let frequency = (1.0 + self.access_count as f64).ln() / 10.0_f64.ln();
        let frequency = frequency.min(1.0);

        // Weighted combination, normalized.
        (recency * recency_weight + frequency * frequency_weight + self.importance * importance_weight)
            / total_weight
    }
}

// ---------------------------------------------------------------------------
// MemoryMetrics
// ---------------------------------------------------------------------------

/// Aggregate statistics about the memory store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryMetrics {
    /// Total number of memories across all tiers.
    pub total_memories: u64,
    /// Count per tier.
    pub by_tier: HashMap<String, u64>,
    /// Count per kind.
    pub by_kind: HashMap<String, u64>,
    /// Approximate storage bytes used.
    pub storage_bytes: u64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn memory_id_is_time_ordered() {
        let a = MemoryId::new();
        let b = MemoryId::new();
        assert!(b.0 >= a.0);
    }

    #[test]
    fn memory_default_tier_is_warm() {
        let m = Memory::new(MemoryKind::Episodic, json!({"event": "test"}));
        assert_eq!(m.tier, MemoryTier::Warm);
    }

    #[test]
    fn importance_clamped() {
        let m = Memory::new(MemoryKind::Semantic, json!("fact"))
            .with_importance(1.5);
        assert!((m.importance - 1.0).abs() < f64::EPSILON);

        let m = Memory::new(MemoryKind::Semantic, json!("fact"))
            .with_importance(-0.5);
        assert!(m.importance.abs() < f64::EPSILON);
    }

    #[test]
    fn record_access_increments() {
        let mut m = Memory::new(MemoryKind::Working, json!(null));
        assert_eq!(m.access_count, 0);
        m.record_access();
        m.record_access();
        assert_eq!(m.access_count, 2);
    }

    #[test]
    fn ttl_expiry() {
        let mut m = Memory::new(MemoryKind::Episodic, json!("temp"));
        m.ttl = Some(Duration::seconds(-1)); // already expired
        m.created_at = Utc::now() - Duration::seconds(10);
        assert!(m.is_expired());

        let m2 = Memory::new(MemoryKind::Episodic, json!("persistent"));
        assert!(!m2.is_expired());
    }

    #[test]
    fn relevance_score_within_bounds() {
        let m = Memory::new(MemoryKind::Semantic, json!("fact"))
            .with_importance(0.8);
        let score = m.relevance_score(1.0, 1.0, 1.0);
        assert!(score >= 0.0 && score <= 1.0);
    }

    #[test]
    fn display_formats() {
        assert_eq!(MemoryTier::Hot.to_string(), "hot");
        assert_eq!(MemoryKind::Procedural.to_string(), "procedural");
        let id = MemoryId::new();
        assert!(id.to_string().starts_with("mem:"));
    }

    #[test]
    fn serialization_roundtrip() {
        let m = Memory::new(MemoryKind::Semantic, json!({"key": "value"}))
            .with_importance(0.9)
            .with_tags(vec!["rust".into(), "agent".into()])
            .with_tier(MemoryTier::Hot);

        let json = serde_json::to_string(&m).unwrap();
        let m2: Memory = serde_json::from_str(&json).unwrap();
        assert_eq!(m.id, m2.id);
        assert_eq!(m.kind, m2.kind);
        assert_eq!(m.tier, m2.tier);
        assert_eq!(m.importance, m2.importance);
        assert_eq!(m.tags, m2.tags);
    }
}
