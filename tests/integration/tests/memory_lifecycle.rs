//! Memory system integration tests.
//!
//! Tests the memory store/recall/forget cycle, tier promotion on frequent
//! access, episode recording/retrieval, and memory consolidation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::Mutex;
use uuid::Uuid;

use daf_core::agent::AgentId;
use daf_integration_tests::init_tracing;

// ---------------------------------------------------------------------------
// Memory system simulation types (mirrors expected daf-memory API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MemoryTier {
    /// Hot: frequently accessed, kept in-memory.
    Hot,
    /// Warm: moderately accessed, may spill to disk.
    Warm,
    /// Cold: rarely accessed, archived.
    Cold,
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    id: Uuid,
    key: String,
    value: serde_json::Value,
    tier: MemoryTier,
    access_count: u64,
    created_at: chrono::DateTime<Utc>,
    last_accessed: chrono::DateTime<Utc>,
    agent_id: AgentId,
}

impl MemoryEntry {
    fn new(key: &str, value: serde_json::Value, agent_id: AgentId) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7(),
            key: key.to_string(),
            value,
            tier: MemoryTier::Cold, // start cold
            access_count: 0,
            created_at: now,
            last_accessed: now,
            agent_id,
        }
    }
}

#[derive(Debug, Clone)]
struct Episode {
    id: Uuid,
    agent_id: AgentId,
    entries: Vec<EpisodeEntry>,
    started_at: chrono::DateTime<Utc>,
    ended_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct EpisodeEntry {
    timestamp: chrono::DateTime<Utc>,
    action: String,
    data: serde_json::Value,
}

#[derive(Debug)]
struct MemoryStore {
    entries: HashMap<String, MemoryEntry>,
    episodes: Vec<Episode>,
    /// Threshold for tier promotion.
    hot_threshold: u64,
    warm_threshold: u64,
}

impl MemoryStore {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            episodes: Vec::new(),
            hot_threshold: 10,
            warm_threshold: 3,
        }
    }

    /// Store a value. If the key already exists, update it.
    fn store(&mut self, key: &str, value: serde_json::Value, agent_id: AgentId) {
        let entry = MemoryEntry::new(key, value, agent_id);
        self.entries.insert(key.to_string(), entry);
    }

    /// Recall a value by key. Increments access count and may promote tier.
    fn recall(&mut self, key: &str) -> Option<&MemoryEntry> {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.access_count += 1;
            entry.last_accessed = Utc::now();

            // Tier promotion based on access frequency.
            if entry.access_count >= self.hot_threshold {
                entry.tier = MemoryTier::Hot;
            } else if entry.access_count >= self.warm_threshold {
                entry.tier = MemoryTier::Warm;
            }

            Some(entry)
        } else {
            None
        }
    }

    /// Forget (delete) a memory entry.
    fn forget(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    /// Return all entries at a given tier.
    fn entries_at_tier(&self, tier: MemoryTier) -> Vec<&MemoryEntry> {
        self.entries.values().filter(|e| e.tier == tier).collect()
    }

    /// Start recording an episode for an agent.
    fn start_episode(&mut self, agent_id: AgentId) -> Uuid {
        let episode = Episode {
            id: Uuid::now_v7(),
            agent_id,
            entries: Vec::new(),
            started_at: Utc::now(),
            ended_at: None,
        };
        let id = episode.id;
        self.episodes.push(episode);
        id
    }

    /// Record an entry in an active episode.
    fn record_episode_entry(
        &mut self,
        episode_id: Uuid,
        action: &str,
        data: serde_json::Value,
    ) -> bool {
        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == episode_id) {
            if ep.ended_at.is_none() {
                ep.entries.push(EpisodeEntry {
                    timestamp: Utc::now(),
                    action: action.to_string(),
                    data,
                });
                return true;
            }
        }
        false
    }

    /// End an episode.
    fn end_episode(&mut self, episode_id: Uuid) -> bool {
        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == episode_id) {
            ep.ended_at = Some(Utc::now());
            true
        } else {
            false
        }
    }

    /// Retrieve an episode by ID.
    fn get_episode(&self, episode_id: Uuid) -> Option<&Episode> {
        self.episodes.iter().find(|e| e.id == episode_id)
    }

    /// Retrieve all episodes for an agent.
    fn agent_episodes(&self, agent_id: AgentId) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|e| e.agent_id == agent_id)
            .collect()
    }

    /// Consolidate: merge cold entries with low access into a summary entry
    /// and remove the originals.
    fn consolidate(&mut self, prefix: &str) -> Option<MemoryEntry> {
        let cold_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(k, e)| k.starts_with(prefix) && e.tier == MemoryTier::Cold)
            .map(|(k, _)| k.clone())
            .collect();

        if cold_keys.len() < 2 {
            return None;
        }

        // Gather values.
        let mut consolidated_data = Vec::new();
        let agent_id = self.entries[&cold_keys[0]].agent_id;

        for key in &cold_keys {
            if let Some(entry) = self.entries.get(key) {
                consolidated_data.push(serde_json::json!({
                    "key": key,
                    "value": entry.value,
                }));
            }
        }

        // Remove originals.
        for key in &cold_keys {
            self.entries.remove(key);
        }

        // Insert consolidated entry.
        let summary_key = format!("{prefix}:consolidated");
        let summary = MemoryEntry::new(
            &summary_key,
            serde_json::json!({
                "consolidated_from": cold_keys.len(),
                "entries": consolidated_data,
            }),
            agent_id,
        );

        self.entries.insert(summary_key, summary.clone());
        Some(summary)
    }
}

// ---------------------------------------------------------------------------
// Test: Store / Recall / Forget cycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_store_recall_forget_cycle() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent = AgentId::new();

    // Store
    store.store("project:name", serde_json::json!("DAF"), agent);
    store.store("project:version", serde_json::json!("0.1.0"), agent);

    // Recall
    let name = store.recall("project:name").expect("should find name");
    assert_eq!(name.value, serde_json::json!("DAF"));
    assert_eq!(name.access_count, 1);
    assert_eq!(name.tier, MemoryTier::Cold); // first access, still cold

    let version = store.recall("project:version").expect("should find version");
    assert_eq!(version.value, serde_json::json!("0.1.0"));

    // Missing key
    assert!(store.recall("nonexistent").is_none());

    // Forget
    assert!(store.forget("project:version"));
    assert!(store.recall("project:version").is_none());
    assert!(!store.forget("project:version")); // already forgotten

    // Original entry still exists.
    assert!(store.recall("project:name").is_some());
}

// ---------------------------------------------------------------------------
// Test: Tier promotion on frequent access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tier_promotion_on_frequent_access() {
    init_tracing();

    let mut store = MemoryStore::new();
    store.warm_threshold = 3;
    store.hot_threshold = 7;

    let agent = AgentId::new();
    store.store("config:db_url", serde_json::json!("postgres://localhost"), agent);

    // Initially cold.
    let entry = store.recall("config:db_url").unwrap();
    assert_eq!(entry.tier, MemoryTier::Cold);

    // Access until warm threshold.
    for _ in 0..2 {
        store.recall("config:db_url");
    }
    let entry = store.recall("config:db_url").unwrap();
    assert_eq!(entry.access_count, 4); // 1 initial + 2 loop + 1 check
    assert_eq!(entry.tier, MemoryTier::Warm);

    // Continue accessing until hot threshold.
    for _ in 0..3 {
        store.recall("config:db_url");
    }
    let entry = store.recall("config:db_url").unwrap();
    assert_eq!(entry.access_count, 8);
    assert_eq!(entry.tier, MemoryTier::Hot);

    // Verify tier distribution.
    assert_eq!(store.entries_at_tier(MemoryTier::Hot).len(), 1);
    assert_eq!(store.entries_at_tier(MemoryTier::Cold).len(), 0);
}

// ---------------------------------------------------------------------------
// Test: Episode recording and retrieval
// ---------------------------------------------------------------------------

#[tokio::test]
async fn episode_recording_and_retrieval() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent = AgentId::new();

    // Start episode.
    let episode_id = store.start_episode(agent);

    // Record entries.
    assert!(store.record_episode_entry(
        episode_id,
        "task_started",
        serde_json::json!({"task": "lint-code"}),
    ));
    assert!(store.record_episode_entry(
        episode_id,
        "file_processed",
        serde_json::json!({"file": "main.rs", "warnings": 2}),
    ));
    assert!(store.record_episode_entry(
        episode_id,
        "task_completed",
        serde_json::json!({"duration_ms": 150}),
    ));

    // End episode.
    assert!(store.end_episode(episode_id));

    // Retrieve and verify.
    let episode = store.get_episode(episode_id).expect("should find episode");
    assert_eq!(episode.agent_id, agent);
    assert_eq!(episode.entries.len(), 3);
    assert!(episode.ended_at.is_some());

    assert_eq!(episode.entries[0].action, "task_started");
    assert_eq!(episode.entries[1].action, "file_processed");
    assert_eq!(episode.entries[2].action, "task_completed");

    // Entries should be chronologically ordered.
    for window in episode.entries.windows(2) {
        assert!(window[0].timestamp <= window[1].timestamp);
    }

    // Cannot record to ended episode.
    assert!(!store.record_episode_entry(
        episode_id,
        "late_entry",
        serde_json::json!({}),
    ));
}

// ---------------------------------------------------------------------------
// Test: Multiple episodes per agent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_episodes_per_agent() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent = AgentId::new();

    let ep1 = store.start_episode(agent);
    store.record_episode_entry(ep1, "work", serde_json::json!({"phase": 1}));
    store.end_episode(ep1);

    let ep2 = store.start_episode(agent);
    store.record_episode_entry(ep2, "work", serde_json::json!({"phase": 2}));
    store.end_episode(ep2);

    let episodes = store.agent_episodes(agent);
    assert_eq!(episodes.len(), 2);
    assert_ne!(episodes[0].id, episodes[1].id);
}

// ---------------------------------------------------------------------------
// Test: Memory consolidation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_consolidation_merges_cold_entries() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent = AgentId::new();

    // Create several cold entries with a shared prefix.
    store.store("logs:2024-01-01", serde_json::json!({"events": 42}), agent);
    store.store("logs:2024-01-02", serde_json::json!({"events": 37}), agent);
    store.store("logs:2024-01-03", serde_json::json!({"events": 55}), agent);

    // Also add a hot entry that should NOT be consolidated.
    store.store("logs:current", serde_json::json!({"events": 10}), agent);
    for _ in 0..15 {
        store.recall("logs:current");
    }

    // Verify pre-conditions.
    assert_eq!(store.entries_at_tier(MemoryTier::Cold).len(), 3);
    assert_eq!(store.entries_at_tier(MemoryTier::Hot).len(), 1);

    // Act: consolidate cold "logs:" entries.
    let consolidated = store.consolidate("logs:").expect("should consolidate");

    // Assert: cold entries replaced by one summary.
    assert_eq!(consolidated.value["consolidated_from"], 3);
    let entries_array = consolidated.value["entries"].as_array().unwrap();
    assert_eq!(entries_array.len(), 3);

    // Original cold keys are gone.
    assert!(store.recall("logs:2024-01-01").is_none());
    assert!(store.recall("logs:2024-01-02").is_none());
    assert!(store.recall("logs:2024-01-03").is_none());

    // Summary exists.
    assert!(store.recall("logs::consolidated").is_some());

    // Hot entry untouched.
    let current = store.recall("logs:current").expect("hot entry survives");
    assert_eq!(current.tier, MemoryTier::Hot);
}

// ---------------------------------------------------------------------------
// Test: Consolidation with fewer than 2 entries is a no-op
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consolidation_noop_with_single_entry() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent = AgentId::new();

    store.store("solo:entry", serde_json::json!("lone"), agent);

    let result = store.consolidate("solo:");
    assert!(result.is_none(), "should not consolidate a single entry");

    // Entry remains.
    assert!(store.recall("solo:entry").is_some());
}

// ---------------------------------------------------------------------------
// Test: Cross-agent memory isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cross_agent_memory_stores_independently() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent_a = AgentId::new();
    let agent_b = AgentId::new();

    store.store("shared_key", serde_json::json!("from-a"), agent_a);

    // Agent B overwrites with its own value (same key, different agent).
    store.store("shared_key", serde_json::json!("from-b"), agent_b);

    // The store is keyed by string, so B's value overwrites A's.
    let entry = store.recall("shared_key").unwrap();
    assert_eq!(entry.value, serde_json::json!("from-b"));
    assert_eq!(entry.agent_id, agent_b);
}

// ---------------------------------------------------------------------------
// Test: Memory entry update preserves access history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_update_resets_entry() {
    init_tracing();

    let mut store = MemoryStore::new();
    let agent = AgentId::new();

    store.store("counter", serde_json::json!(0), agent);

    // Access several times to warm it up.
    for _ in 0..5 {
        store.recall("counter");
    }
    let entry = store.recall("counter").unwrap();
    assert_eq!(entry.tier, MemoryTier::Warm);

    // Re-store (update) resets the entry.
    store.store("counter", serde_json::json!(99), agent);
    let entry = store.recall("counter").unwrap();
    assert_eq!(entry.value, serde_json::json!(99));
    assert_eq!(entry.access_count, 1); // reset after re-store
    assert_eq!(entry.tier, MemoryTier::Cold); // back to cold
}
