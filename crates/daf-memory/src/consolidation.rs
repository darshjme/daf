//! Memory consolidation — analogous to sleep consolidation in humans.
//!
//! Reviews recent episodic memories, extracts key learnings, merges
//! duplicates, distills verbose memories into concise semantic ones,
//! and reinforces memories that proved useful. Runs as a periodic
//! background task.

use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, info, instrument, warn};

use crate::error::MemoryResult;
use crate::store::MemoryStore;
use crate::types::{Memory, MemoryId, MemoryKind, MemoryTier};

// ---------------------------------------------------------------------------
// ConsolidationConfig
// ---------------------------------------------------------------------------

/// Configuration for the consolidation process.
#[derive(Debug, Clone)]
pub struct ConsolidationConfig {
    /// Only consolidate memories created within this many hours.
    pub lookback_hours: f64,
    /// Similarity threshold for merging (0.0-1.0). Memories with content
    /// overlap above this ratio are candidates for merging.
    pub similarity_threshold: f64,
    /// Minimum importance boost when reinforcing.
    pub reinforce_boost: f64,
    /// Maximum importance boost when reinforcing (capped at 1.0).
    pub reinforce_max: f64,
    /// Interval in seconds between consolidation runs.
    pub interval_secs: u64,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            lookback_hours: 24.0,
            similarity_threshold: 0.6,
            reinforce_boost: 0.1,
            reinforce_max: 1.0,
            interval_secs: 3600, // 1 hour
        }
    }
}

// ---------------------------------------------------------------------------
// ConsolidationReport
// ---------------------------------------------------------------------------

/// Summary of a consolidation run.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationReport {
    /// Number of memories reviewed.
    pub reviewed: u64,
    /// Number of semantic memories created from episodic ones.
    pub distilled: u64,
    /// Number of memory pairs merged.
    pub merged: u64,
    /// Number of memories reinforced (importance boosted).
    pub reinforced: u64,
}

// ---------------------------------------------------------------------------
// Consolidator
// ---------------------------------------------------------------------------

/// Performs memory consolidation against a store.
///
/// Consolidation is inspired by how the human brain consolidates memories
/// during sleep: reviewing recent experiences, extracting patterns, merging
/// related memories, and strengthening important ones.
#[derive(Clone)]
pub struct Consolidator {
    store: Arc<dyn MemoryStore>,
    config: ConsolidationConfig,
}

impl std::fmt::Debug for Consolidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Consolidator")
            .field("config", &self.config)
            .finish()
    }
}

impl Consolidator {
    /// Create a new consolidator with default config.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            config: ConsolidationConfig::default(),
        }
    }

    /// Create a new consolidator with custom config.
    pub fn with_config(store: Arc<dyn MemoryStore>, config: ConsolidationConfig) -> Self {
        Self { store, config }
    }

    /// Run a full consolidation cycle: distill, merge, reinforce.
    ///
    /// Returns a report summarizing what was done.
    #[instrument(skip(self))]
    pub async fn consolidate(&self) -> MemoryResult<ConsolidationReport> {
        let mut report = ConsolidationReport::default();

        let all = self.store.list_all(100_000).await?;
        let now = Utc::now();

        // Filter to recent memories within the lookback window.
        let recent: Vec<Memory> = all
            .into_iter()
            .filter(|m| {
                let age_hours = (now - m.created_at).num_seconds().max(0) as f64 / 3600.0;
                age_hours <= self.config.lookback_hours
            })
            .collect();

        report.reviewed = recent.len() as u64;

        // Phase 1: Distill episodic memories into semantic ones.
        let distilled_ids = self.distill_batch(&recent).await?;
        report.distilled = distilled_ids.len() as u64;

        // Phase 2: Merge similar memories.
        let merged = self.merge_similar_batch(&recent).await?;
        report.merged = merged as u64;

        // Phase 3: Reinforce frequently accessed memories.
        let reinforced = self.reinforce_batch(&recent).await?;
        report.reinforced = reinforced as u64;

        info!(
            reviewed = report.reviewed,
            distilled = report.distilled,
            merged = report.merged,
            reinforced = report.reinforced,
            "consolidation complete"
        );

        Ok(report)
    }

    /// Distill episodic memories into concise semantic memories.
    ///
    /// For each episodic memory with sufficient importance, creates a
    /// compact semantic memory capturing the key fact or learning.
    /// The original episodic memory is retained but may be demoted.
    #[instrument(skip(self, memories))]
    pub async fn distill_batch(&self, memories: &[Memory]) -> MemoryResult<Vec<MemoryId>> {
        let mut created = Vec::new();

        for memory in memories {
            if memory.kind != MemoryKind::Episodic {
                continue;
            }
            if memory.importance < 0.4 {
                continue; // Not important enough to distill.
            }

            // Extract the core content for the semantic memory.
            let distilled_content = self.distill_content(&memory.content);

            let semantic = Memory::new(MemoryKind::Semantic, distilled_content)
                .with_importance((memory.importance * 0.9).clamp(0.0, 1.0))
                .with_tier(MemoryTier::Warm)
                .with_tags(memory.tags.clone());

            let id = semantic.id;
            self.store.store(&semantic).await?;
            created.push(id);
            debug!(
                source = %memory.id,
                distilled = %id,
                "distilled episodic → semantic"
            );
        }

        Ok(created)
    }

    /// Find and merge duplicate or similar memories.
    ///
    /// Uses content fingerprinting to identify similar memories. When two
    /// memories are similar enough, the newer one is merged into the older
    /// one (preserving the older ID) and the duplicate is deleted.
    #[instrument(skip(self, memories))]
    pub async fn merge_similar_batch(&self, memories: &[Memory]) -> MemoryResult<u64> {
        let mut merged_count = 0u64;
        let mut merged_ids: Vec<MemoryId> = Vec::new();

        // Group by kind for comparison.
        let mut by_kind: HashMap<String, Vec<&Memory>> = HashMap::new();
        for m in memories {
            by_kind
                .entry(m.kind.to_string())
                .or_default()
                .push(m);
        }

        for (_kind, group) in &by_kind {
            if group.len() < 2 {
                continue;
            }

            for i in 0..group.len() {
                if merged_ids.contains(&group[i].id) {
                    continue;
                }
                for j in (i + 1)..group.len() {
                    if merged_ids.contains(&group[j].id) {
                        continue;
                    }

                    let similarity = content_similarity(
                        &group[i].content.to_string(),
                        &group[j].content.to_string(),
                    );

                    if similarity >= self.config.similarity_threshold {
                        // Merge j into i: keep i, delete j.
                        let mut keeper = group[i].clone();
                        keeper.access_count += group[j].access_count;
                        keeper.importance =
                            keeper.importance.max(group[j].importance);
                        // Merge tags.
                        for tag in &group[j].tags {
                            if !keeper.tags.contains(tag) {
                                keeper.tags.push(tag.clone());
                            }
                        }

                        if let Err(e) = self.store.update(&keeper).await {
                            warn!(id = %keeper.id, error = %e, "failed to update merged memory");
                        }
                        if let Err(e) = self.store.delete(&group[j].id).await {
                            warn!(id = %group[j].id, error = %e, "failed to delete merged duplicate");
                        }

                        merged_ids.push(group[j].id);
                        merged_count += 1;
                        debug!(
                            kept = %group[i].id,
                            removed = %group[j].id,
                            similarity = similarity,
                            "merged similar memories"
                        );
                    }
                }
            }
        }

        Ok(merged_count)
    }

    /// Reinforce memories that have proven useful (high access count).
    ///
    /// Memories accessed more than average get an importance boost,
    /// making them less likely to decay away.
    #[instrument(skip(self, memories))]
    pub async fn reinforce_batch(&self, memories: &[Memory]) -> MemoryResult<u64> {
        if memories.is_empty() {
            return Ok(0);
        }

        let avg_access: f64 =
            memories.iter().map(|m| m.access_count as f64).sum::<f64>() / memories.len() as f64;

        let mut reinforced = 0u64;

        for memory in memories {
            if memory.access_count as f64 > avg_access && memory.access_count > 1 {
                let mut boosted = memory.clone();
                let boost = self.config.reinforce_boost;
                boosted.importance =
                    (boosted.importance + boost).min(self.config.reinforce_max);

                if let Err(e) = self.store.update(&boosted).await {
                    warn!(id = %boosted.id, error = %e, "failed to reinforce memory");
                } else {
                    reinforced += 1;
                    debug!(
                        id = %boosted.id,
                        old_importance = memory.importance,
                        new_importance = boosted.importance,
                        "reinforced memory"
                    );
                }
            }
        }

        Ok(reinforced)
    }

    /// Reinforce a single memory by boosting its importance.
    pub async fn reinforce(&self, id: &MemoryId, boost: f64) -> MemoryResult<()> {
        let Some(mut memory) = self.store.retrieve(id).await? else {
            return Err(crate::error::MemoryError::NotFound(*id));
        };
        memory.importance = (memory.importance + boost).min(1.0);
        self.store.update(&memory).await?;
        Ok(())
    }

    /// Start periodic consolidation as a background task.
    pub fn start_background(self) -> JoinHandle<()> {
        let interval = std::time::Duration::from_secs(self.config.interval_secs);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if let Err(e) = self.consolidate().await {
                    warn!(error = %e, "background consolidation failed");
                }
            }
        })
    }

    // ----- Private helpers -----

    /// Distill memory content into a more compact form.
    ///
    /// For now this does a simple extraction: if the content is an object
    /// with known summary fields, extract those. Otherwise return as-is.
    /// In production, this would call an LLM for summarization.
    fn distill_content(&self, content: &serde_json::Value) -> serde_json::Value {
        if let Some(obj) = content.as_object() {
            // Look for summary-like fields.
            for key in &["summary", "learning", "key_point", "takeaway", "conclusion"] {
                if let Some(val) = obj.get(*key) {
                    return val.clone();
                }
            }
            // If there's a "content" field with a string, use that.
            if let Some(val) = obj.get("content") {
                if val.is_string() {
                    return val.clone();
                }
            }
        }
        // Fall back to the full content.
        content.clone()
    }
}

// ---------------------------------------------------------------------------
// Content similarity
// ---------------------------------------------------------------------------

/// Compute a simple content similarity score between two strings.
///
/// Uses Jaccard similarity on word-level tokens. Returns a value in [0.0, 1.0].
/// This is a placeholder for a proper embedding-based similarity in production.
fn content_similarity(a: &str, b: &str) -> f64 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    let words_a: std::collections::HashSet<&str> =
        a_lower.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> =
        b_lower.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;
    use serde_json::json;

    fn make_consolidator() -> Consolidator {
        Consolidator::new(Arc::new(InMemoryStore::new()))
    }

    #[test]
    fn content_similarity_identical() {
        assert!((content_similarity("hello world", "hello world") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn content_similarity_disjoint() {
        assert!((content_similarity("hello world", "foo bar")).abs() < f64::EPSILON);
    }

    #[test]
    fn content_similarity_partial() {
        let sim = content_similarity("hello world foo", "hello world bar");
        assert!(sim > 0.3 && sim < 0.8);
    }

    #[tokio::test]
    async fn distill_creates_semantic_memories() {
        let c = make_consolidator();

        let ep = Memory::new(
            MemoryKind::Episodic,
            json!({"event": "deployed auth", "summary": "auth service deployed successfully"}),
        )
        .with_importance(0.8);
        c.store.store(&ep).await.unwrap();

        let memories = c.store.list_all(100).await.unwrap();
        let created = c.distill_batch(&memories).await.unwrap();
        assert_eq!(created.len(), 1);

        // Verify the distilled memory exists and is semantic.
        let distilled = c.store.retrieve(&created[0]).await.unwrap().unwrap();
        assert_eq!(distilled.kind, MemoryKind::Semantic);
        assert_eq!(
            distilled.content,
            json!("auth service deployed successfully")
        );
    }

    #[tokio::test]
    async fn merge_removes_duplicates() {
        let config = ConsolidationConfig {
            similarity_threshold: 0.5,
            ..Default::default()
        };
        let store = Arc::new(InMemoryStore::new());
        let c = Consolidator::with_config(store.clone(), config);

        let m1 = Memory::new(MemoryKind::Semantic, json!("rust is a fast systems language"))
            .with_importance(0.5);
        let m2 = Memory::new(MemoryKind::Semantic, json!("rust is a fast safe systems language"))
            .with_importance(0.7);

        c.store.store(&m1).await.unwrap();
        c.store.store(&m2).await.unwrap();

        let memories = c.store.list_all(100).await.unwrap();
        let merged = c.merge_similar_batch(&memories).await.unwrap();
        assert_eq!(merged, 1);

        // Should have only 1 memory left.
        let remaining = c.store.list_all(100).await.unwrap();
        assert_eq!(remaining.len(), 1);
        // Kept the higher importance.
        assert!(remaining[0].importance >= 0.7);
    }

    #[tokio::test]
    async fn reinforce_boosts_frequent() {
        let c = make_consolidator();

        let mut m1 = Memory::new(MemoryKind::Semantic, json!("used often"))
            .with_importance(0.5);
        m1.access_count = 10;
        let mut m2 = Memory::new(MemoryKind::Semantic, json!("rarely used"))
            .with_importance(0.5);
        m2.access_count = 0;

        c.store.store(&m1).await.unwrap();
        c.store.store(&m2).await.unwrap();

        let memories = c.store.list_all(100).await.unwrap();
        let reinforced = c.reinforce_batch(&memories).await.unwrap();
        assert_eq!(reinforced, 1);

        let boosted = c.store.retrieve(&m1.id).await.unwrap().unwrap();
        assert!(boosted.importance > 0.5);
    }

    #[tokio::test]
    async fn full_consolidation_cycle() {
        let c = make_consolidator();

        c.store
            .store(
                &Memory::new(MemoryKind::Episodic, json!({"summary": "learned X"}))
                    .with_importance(0.7),
            )
            .await
            .unwrap();

        let report = c.consolidate().await.unwrap();
        assert!(report.reviewed >= 1);
        assert!(report.distilled >= 1);
    }

    #[tokio::test]
    async fn reinforce_single() {
        let c = make_consolidator();
        let m = Memory::new(MemoryKind::Semantic, json!("test"))
            .with_importance(0.5);
        c.store.store(&m).await.unwrap();

        c.reinforce(&m.id, 0.2).await.unwrap();
        let updated = c.store.retrieve(&m.id).await.unwrap().unwrap();
        assert!((updated.importance - 0.7).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn reinforce_nonexistent_errors() {
        let c = make_consolidator();
        let result = c.reinforce(&MemoryId::new(), 0.1).await;
        assert!(result.is_err());
    }
}
