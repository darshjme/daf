//! Memory lifecycle manager — orchestrating the 3-tier memory system.
//!
//! The [`MemoryManager`] is the primary interface for agents interacting with
//! memory. It handles tier assignment, promotion/demotion, TTL-based expiry,
//! and periodic compaction as a background task.

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, info, instrument, warn};

use crate::error::MemoryResult;
use crate::recall::{execute_recall, RecallQuery, RecallResult};
use crate::store::MemoryStore;
use crate::types::{Memory, MemoryId, MemoryKind, MemoryMetrics, MemoryTier};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the memory manager.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Access count threshold to promote from Warm to Hot.
    pub promote_to_hot_threshold: u64,
    /// Access count threshold to promote from Cold to Warm.
    pub promote_to_warm_threshold: u64,
    /// Hours since last access before demoting from Hot to Warm.
    pub demote_from_hot_hours: f64,
    /// Hours since last access before demoting from Warm to Cold.
    pub demote_from_warm_hours: f64,
    /// Importance threshold for initial Hot tier assignment.
    pub hot_importance_threshold: f64,
    /// Half-life in hours for importance decay.
    pub decay_half_life_hours: f64,
    /// Interval in seconds between background compaction runs.
    pub compaction_interval_secs: u64,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            promote_to_hot_threshold: 10,
            promote_to_warm_threshold: 3,
            demote_from_hot_hours: 24.0,
            demote_from_warm_hours: 168.0, // 1 week
            hot_importance_threshold: 0.8,
            decay_half_life_hours: 168.0,
            compaction_interval_secs: 300, // 5 minutes
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryManager
// ---------------------------------------------------------------------------

/// Orchestrates the 3-tier memory system.
///
/// Wraps a [`MemoryStore`] and adds lifecycle management: tier assignment,
/// automatic promotion/demotion, TTL-based expiry, and recall with access
/// tracking.
///
/// The manager is cheaply cloneable (all state is behind `Arc`).
#[derive(Clone)]
pub struct MemoryManager {
    store: Arc<dyn MemoryStore>,
    config: Arc<ManagerConfig>,
    /// Tracks whether the background task is running.
    bg_running: Arc<RwLock<bool>>,
}

impl std::fmt::Debug for MemoryManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryManager")
            .field("config", &self.config)
            .finish()
    }
}

impl MemoryManager {
    /// Create a new memory manager with the given store and default config.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            config: Arc::new(ManagerConfig::default()),
            bg_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Create a new memory manager with custom configuration.
    pub fn with_config(store: Arc<dyn MemoryStore>, config: ManagerConfig) -> Self {
        Self {
            store,
            config: Arc::new(config),
            bg_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Get a reference to the underlying store.
    pub fn store(&self) -> &dyn MemoryStore {
        self.store.as_ref()
    }

    /// Get the current configuration.
    pub fn config(&self) -> &ManagerConfig {
        &self.config
    }

    // ----- Core operations -----

    /// Store a new memory, assigning an initial tier based on importance.
    ///
    /// - Importance >= `hot_importance_threshold` -> Hot
    /// - Working kind -> Hot (always in the active set)
    /// - Everything else -> Warm
    #[instrument(skip(self, memory), fields(id = %memory.id, kind = %memory.kind))]
    pub async fn store_memory(&self, mut memory: Memory) -> MemoryResult<MemoryId> {
        // Assign initial tier based on importance and kind.
        memory.tier = self.assign_initial_tier(&memory);
        let id = memory.id;
        self.store.store(&memory).await?;
        debug!(tier = %memory.tier, importance = memory.importance, "stored memory");
        Ok(id)
    }

    /// Recall memories matching a query.
    ///
    /// This is the primary read path. It:
    /// 1. Executes the query against the store.
    /// 2. Boosts `access_count` and `last_accessed` on each returned memory.
    /// 3. Promotes memories that have been accessed frequently enough.
    #[instrument(skip(self, query))]
    pub async fn recall(&self, query: &RecallQuery) -> MemoryResult<RecallResult> {
        let mut result = execute_recall(self.store.as_ref(), query).await?;

        // Boost access counts and check for promotion.
        for memory in &mut result.memories {
            memory.record_access();

            // Check for tier promotion.
            let new_tier = self.check_promotion(memory);
            if new_tier != memory.tier {
                info!(
                    id = %memory.id,
                    from = %memory.tier,
                    to = %new_tier,
                    "promoting memory"
                );
                memory.tier = new_tier;
            }

            // Persist the updated access metadata.
            if let Err(e) = self.store.update(memory).await {
                warn!(id = %memory.id, error = %e, "failed to update access metadata");
            }
        }

        Ok(result)
    }

    /// Retrieve a single memory by ID, boosting its access count.
    #[instrument(skip(self))]
    pub async fn get(&self, id: &MemoryId) -> MemoryResult<Option<Memory>> {
        let Some(mut memory) = self.store.retrieve(id).await? else {
            return Ok(None);
        };

        memory.record_access();
        let new_tier = self.check_promotion(&memory);
        if new_tier != memory.tier {
            memory.tier = new_tier;
        }
        let _ = self.store.update(&memory).await;
        Ok(Some(memory))
    }

    /// Explicitly forget (delete) a memory.
    #[instrument(skip(self))]
    pub async fn forget(&self, id: &MemoryId) -> MemoryResult<()> {
        self.store.delete(id).await?;
        debug!("forgot memory {id}");
        Ok(())
    }

    /// Run a compaction pass: apply decay, demote stale memories, expire TTL'd memories.
    ///
    /// This is called periodically by the background task but can also be
    /// invoked manually.
    #[instrument(skip(self))]
    pub async fn compact(&self) -> MemoryResult<CompactionReport> {
        let mut report = CompactionReport::default();
        let all = self.store.list_all(100_000).await?;

        for mut memory in all {
            // TTL expiry.
            if memory.is_expired() {
                self.store.delete(&memory.id).await?;
                report.expired += 1;
                continue;
            }

            // Apply importance decay.
            let old_importance = memory.importance;
            crate::recall::apply_decay(&mut memory, self.config.decay_half_life_hours);
            if (old_importance - memory.importance).abs() > 0.001 {
                report.decayed += 1;
            }

            // Tier demotion.
            let new_tier = self.check_demotion(&memory);
            if new_tier != memory.tier {
                debug!(
                    id = %memory.id,
                    from = %memory.tier,
                    to = %new_tier,
                    "demoting memory"
                );
                memory.tier = new_tier;
                report.demoted += 1;
            }

            // Persist changes.
            if let Err(e) = self.store.update(&memory).await {
                warn!(id = %memory.id, error = %e, "failed to update during compaction");
            }
        }

        info!(
            expired = report.expired,
            decayed = report.decayed,
            demoted = report.demoted,
            "compaction complete"
        );
        Ok(report)
    }

    /// Return aggregate metrics.
    pub async fn metrics(&self) -> MemoryResult<MemoryMetrics> {
        self.store.metrics().await
    }

    // ----- Background task -----

    /// Start the periodic background compaction task.
    ///
    /// Returns a `JoinHandle` for the spawned task. The task runs until
    /// the `MemoryManager` is dropped or the handle is aborted.
    pub async fn start_background_compaction(&self) -> Option<JoinHandle<()>> {
        let mut running = self.bg_running.write().await;
        if *running {
            return None;
        }
        *running = true;

        let manager = self.clone();
        let interval = std::time::Duration::from_secs(self.config.compaction_interval_secs);

        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if let Err(e) = manager.compact().await {
                    warn!(error = %e, "background compaction failed");
                }
            }
        });

        Some(handle)
    }

    // ----- Private helpers -----

    fn assign_initial_tier(&self, memory: &Memory) -> MemoryTier {
        if memory.kind == MemoryKind::Working {
            return MemoryTier::Hot;
        }
        if memory.importance >= self.config.hot_importance_threshold {
            return MemoryTier::Hot;
        }
        MemoryTier::Warm
    }

    fn check_promotion(&self, memory: &Memory) -> MemoryTier {
        match memory.tier {
            MemoryTier::Cold => {
                if memory.access_count >= self.config.promote_to_warm_threshold {
                    MemoryTier::Warm
                } else {
                    MemoryTier::Cold
                }
            }
            MemoryTier::Warm => {
                if memory.access_count >= self.config.promote_to_hot_threshold {
                    MemoryTier::Hot
                } else {
                    MemoryTier::Warm
                }
            }
            MemoryTier::Hot => MemoryTier::Hot,
        }
    }

    fn check_demotion(&self, memory: &Memory) -> MemoryTier {
        let hours_since_access = memory
            .time_since_access()
            .num_seconds()
            .max(0) as f64
            / 3600.0;

        match memory.tier {
            MemoryTier::Hot => {
                if hours_since_access > self.config.demote_from_hot_hours {
                    MemoryTier::Warm
                } else {
                    MemoryTier::Hot
                }
            }
            MemoryTier::Warm => {
                if hours_since_access > self.config.demote_from_warm_hours {
                    MemoryTier::Cold
                } else {
                    MemoryTier::Warm
                }
            }
            MemoryTier::Cold => MemoryTier::Cold,
        }
    }
}

// ---------------------------------------------------------------------------
// CompactionReport
// ---------------------------------------------------------------------------

/// Summary of a compaction run.
#[derive(Debug, Clone, Default)]
pub struct CompactionReport {
    /// Number of memories deleted due to TTL expiry.
    pub expired: u64,
    /// Number of memories whose importance was decayed.
    pub decayed: u64,
    /// Number of memories demoted to a colder tier.
    pub demoted: u64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;
    use crate::types::MemoryKind;
    use chrono::Duration;
    use serde_json::json;

    fn make_manager() -> MemoryManager {
        MemoryManager::new(Arc::new(InMemoryStore::new()))
    }

    #[tokio::test]
    async fn store_and_get() {
        let mgr = make_manager();
        let mem = Memory::new(MemoryKind::Semantic, json!("fact"));
        let id = mgr.store_memory(mem).await.unwrap();
        let retrieved = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.access_count, 1); // get() boosts access
    }

    #[tokio::test]
    async fn initial_tier_based_on_importance() {
        let mgr = make_manager();

        // High importance -> Hot.
        let m = Memory::new(MemoryKind::Semantic, json!("critical"))
            .with_importance(0.95);
        let id = mgr.store_memory(m).await.unwrap();
        let retrieved = mgr.store.retrieve(&id).await.unwrap().unwrap();
        assert_eq!(retrieved.tier, MemoryTier::Hot);

        // Working kind -> always Hot.
        let m = Memory::new(MemoryKind::Working, json!("context"))
            .with_importance(0.1);
        let id = mgr.store_memory(m).await.unwrap();
        let retrieved = mgr.store.retrieve(&id).await.unwrap().unwrap();
        assert_eq!(retrieved.tier, MemoryTier::Hot);

        // Normal importance -> Warm.
        let m = Memory::new(MemoryKind::Semantic, json!("normal"))
            .with_importance(0.5);
        let id = mgr.store_memory(m).await.unwrap();
        let retrieved = mgr.store.retrieve(&id).await.unwrap().unwrap();
        assert_eq!(retrieved.tier, MemoryTier::Warm);
    }

    #[tokio::test]
    async fn forget_deletes_memory() {
        let mgr = make_manager();
        let mem = Memory::new(MemoryKind::Episodic, json!("temp"));
        let id = mgr.store_memory(mem).await.unwrap();
        mgr.forget(&id).await.unwrap();
        assert!(mgr.get(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn compact_expires_ttl_memories() {
        let mgr = make_manager();
        let mut mem = Memory::new(MemoryKind::Episodic, json!("ephemeral"))
            .with_ttl(Duration::seconds(1));
        // Backdate creation so TTL has expired.
        mem.created_at = chrono::Utc::now() - Duration::seconds(10);
        let id = mgr.store_memory(mem).await.unwrap();

        let report = mgr.compact().await.unwrap();
        assert!(report.expired >= 1);
        assert!(mgr.store.retrieve(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn compact_demotes_stale_hot() {
        let config = ManagerConfig {
            demote_from_hot_hours: 0.001, // Demote almost immediately
            ..Default::default()
        };
        let mgr = MemoryManager::with_config(Arc::new(InMemoryStore::new()), config);

        let mut mem = Memory::new(MemoryKind::Semantic, json!("stale"))
            .with_importance(0.95);
        mem.last_accessed = chrono::Utc::now() - Duration::hours(1);
        let id = mgr.store_memory(mem).await.unwrap();

        let report = mgr.compact().await.unwrap();
        assert!(report.demoted >= 1);

        let retrieved = mgr.store.retrieve(&id).await.unwrap().unwrap();
        assert_eq!(retrieved.tier, MemoryTier::Warm);
    }

    #[tokio::test]
    async fn recall_boosts_access() {
        let mgr = make_manager();
        let mem = Memory::new(MemoryKind::Semantic, json!("searchable fact"));
        let id = mgr.store_memory(mem).await.unwrap();

        let query = RecallQuery::new().by_content_search("searchable");
        let result = mgr.recall(&query).await.unwrap();
        assert_eq!(result.memories.len(), 1);

        // Check access was boosted.
        let retrieved = mgr.store.retrieve(&id).await.unwrap().unwrap();
        assert!(retrieved.access_count >= 1);
    }

    #[tokio::test]
    async fn metrics_works() {
        let mgr = make_manager();
        mgr.store_memory(Memory::new(MemoryKind::Semantic, json!("a")))
            .await
            .unwrap();
        mgr.store_memory(Memory::new(MemoryKind::Episodic, json!("b")))
            .await
            .unwrap();

        let metrics = mgr.metrics().await.unwrap();
        assert_eq!(metrics.total_memories, 2);
    }
}
