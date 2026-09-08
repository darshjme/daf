//! Memory recall and search — querying the memory system with ranked results.
//!
//! Provides a builder-pattern [`RecallQuery`] for constructing memory searches,
//! a [`RecallStrategy`] for controlling result ranking, and a decay function
//! that models how memories lose salience over time unless reinforced.

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::store::MemoryStore;
use crate::types::{Memory, MemoryKind, MemoryTier};

// ---------------------------------------------------------------------------
// RecallStrategy
// ---------------------------------------------------------------------------

/// How to rank memories in recall results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecallStrategy {
    /// Sort by `last_accessed` descending — what was touched most recently.
    MostRecent,
    /// Sort by `access_count` descending — what has been recalled most often.
    MostAccessed,
    /// Sort by `importance` descending — what the agent values most.
    MostImportant,
    /// Composite ranking combining recency, frequency, and importance using
    /// the relevance score function on [`Memory`].
    MostRelevant,
}

impl Default for RecallStrategy {
    fn default() -> Self {
        Self::MostRelevant
    }
}

// ---------------------------------------------------------------------------
// RecallQuery
// ---------------------------------------------------------------------------

/// Builder for constructing memory recall queries.
///
/// # Example
/// ```
/// use daf_memory::recall::{RecallQuery, RecallStrategy};
/// use daf_memory::types::MemoryKind;
///
/// let query = RecallQuery::new()
///     .by_kind(MemoryKind::Semantic)
///     .by_tags(vec!["rust".into()])
///     .by_importance_threshold(0.5)
///     .strategy(RecallStrategy::MostRelevant)
///     .limit(20);
/// ```
#[derive(Debug, Clone)]
pub struct RecallQuery {
    /// Filter by source agent.
    pub agent_filter: Option<AgentId>,
    /// Filter by memory kind.
    pub kind_filter: Option<MemoryKind>,
    /// Filter by tier.
    pub tier_filter: Option<MemoryTier>,
    /// Filter by tags (all must match).
    pub tag_filters: Vec<String>,
    /// Filter by time range (inclusive).
    pub time_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// Only return memories with importance >= this threshold.
    pub importance_threshold: Option<f64>,
    /// Full-text content search query.
    pub content_search: Option<String>,
    /// Maximum number of results.
    pub result_limit: usize,
    /// Ranking strategy.
    pub ranking_strategy: RecallStrategy,
    /// Weights for the composite relevance score (recency, frequency, importance).
    /// Only used when `ranking_strategy` is `MostRelevant`.
    pub relevance_weights: (f64, f64, f64),
}

impl RecallQuery {
    /// Create a new query with defaults (limit 50, MostRelevant ranking).
    pub fn new() -> Self {
        Self {
            agent_filter: None,
            kind_filter: None,
            tier_filter: None,
            tag_filters: Vec::new(),
            time_range: None,
            importance_threshold: None,
            content_search: None,
            result_limit: 50,
            ranking_strategy: RecallStrategy::MostRelevant,
            relevance_weights: (0.4, 0.2, 0.4),
        }
    }

    /// Filter results to a specific agent.
    pub fn by_agent(mut self, agent: AgentId) -> Self {
        self.agent_filter = Some(agent);
        self
    }

    /// Filter results to a specific memory kind.
    pub fn by_kind(mut self, kind: MemoryKind) -> Self {
        self.kind_filter = Some(kind);
        self
    }

    /// Filter results to a specific tier.
    pub fn by_tier(mut self, tier: MemoryTier) -> Self {
        self.tier_filter = Some(tier);
        self
    }

    /// Filter results by tags. All tags must be present on the memory.
    pub fn by_tags(mut self, tags: Vec<String>) -> Self {
        self.tag_filters = tags;
        self
    }

    /// Filter results to a time range.
    pub fn by_time_range(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.time_range = Some((start, end));
        self
    }

    /// Only return memories above this importance threshold.
    pub fn by_importance_threshold(mut self, threshold: f64) -> Self {
        self.importance_threshold = Some(threshold.clamp(0.0, 1.0));
        self
    }

    /// Search content for this query string.
    pub fn by_content_search(mut self, query: impl Into<String>) -> Self {
        self.content_search = Some(query.into());
        self
    }

    /// Set the maximum number of results.
    pub fn limit(mut self, limit: usize) -> Self {
        self.result_limit = limit;
        self
    }

    /// Set the ranking strategy.
    pub fn strategy(mut self, strategy: RecallStrategy) -> Self {
        self.ranking_strategy = strategy;
        self
    }

    /// Set custom relevance weights (recency, frequency, importance).
    /// Only applies when strategy is `MostRelevant`.
    pub fn with_weights(mut self, recency: f64, frequency: f64, importance: f64) -> Self {
        self.relevance_weights = (recency, frequency, importance);
        self
    }
}

impl Default for RecallQuery {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RecallResult
// ---------------------------------------------------------------------------

/// Result of a recall query, containing matched memories and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResult {
    /// The matched memories, ranked according to the query's strategy.
    pub memories: Vec<Memory>,
    /// Total number of memories that matched the filters (before limiting).
    pub total_matches: usize,
    /// Time taken to execute the query, in microseconds.
    pub query_time_us: u64,
}

impl RecallResult {
    /// Check if the result set is empty.
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty()
    }

    /// Number of returned memories.
    pub fn len(&self) -> usize {
        self.memories.len()
    }
}

// ---------------------------------------------------------------------------
// Recall execution
// ---------------------------------------------------------------------------

/// Execute a recall query against a memory store.
///
/// This function:
/// 1. Fetches candidate memories using content search or full scan.
/// 2. Applies all filters (agent, kind, tier, tags, time range, importance).
/// 3. Ranks results according to the query's strategy.
/// 4. Truncates to the result limit.
pub async fn execute_recall(
    store: &dyn MemoryStore,
    query: &RecallQuery,
) -> crate::error::MemoryResult<RecallResult> {
    let start = Instant::now();

    // Step 1: Fetch every candidate before filtering and ranking. The store
    // API cannot apply these filters or ranking, so limiting here can discard
    // valid matches and the highest-ranked result. Apply the limit only below.
    let candidates = if let Some(ref search) = query.content_search {
        // Use content search if specified — let the store do initial filtering.
        store.search(search, usize::MAX).await?
    } else {
        store.list_all(usize::MAX).await?
    };

    // Step 2: Apply filters.
    let mut filtered: Vec<Memory> = candidates
        .into_iter()
        .filter(|m| {
            // Agent filter.
            if let Some(ref agent) = query.agent_filter {
                if m.source_agent.as_ref() != Some(agent) {
                    return false;
                }
            }
            // Kind filter.
            if let Some(ref kind) = query.kind_filter {
                if m.kind != *kind {
                    return false;
                }
            }
            // Tier filter.
            if let Some(ref tier) = query.tier_filter {
                if m.tier != *tier {
                    return false;
                }
            }
            // Tag filter (all must match).
            if !query.tag_filters.is_empty()
                && !query
                    .tag_filters
                    .iter()
                    .all(|tag| m.tags.contains(tag))
            {
                return false;
            }
            // Time range filter.
            if let Some((start, end)) = query.time_range {
                if m.created_at < start || m.created_at > end {
                    return false;
                }
            }
            // Importance threshold.
            if let Some(threshold) = query.importance_threshold {
                if m.importance < threshold {
                    return false;
                }
            }
            true
        })
        .collect();

    let total_matches = filtered.len();

    // Step 3: Rank.
    let (rw, fw, iw) = query.relevance_weights;
    match query.ranking_strategy {
        RecallStrategy::MostRecent => {
            filtered.sort_by(|a, b| b.last_accessed.cmp(&a.last_accessed));
        }
        RecallStrategy::MostAccessed => {
            filtered.sort_by(|a, b| b.access_count.cmp(&a.access_count));
        }
        RecallStrategy::MostImportant => {
            filtered.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        RecallStrategy::MostRelevant => {
            filtered.sort_by(|a, b| {
                let sa = a.relevance_score(rw, fw, iw);
                let sb = b.relevance_score(rw, fw, iw);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    // Step 4: Truncate.
    filtered.truncate(query.result_limit);

    let elapsed = start.elapsed();

    Ok(RecallResult {
        memories: filtered,
        total_matches,
        query_time_us: elapsed.as_micros() as u64,
    })
}

// ---------------------------------------------------------------------------
// Decay function
// ---------------------------------------------------------------------------

/// Apply time-based importance decay to a memory.
///
/// Models the psychological finding that memories lose salience over time
/// unless reinforced. Uses exponential decay with a configurable half-life.
///
/// - `half_life_hours`: hours after which importance is halved (default: 168 = 1 week).
/// - The decay is computed from `last_accessed`, not `created_at`, so
///   reinforcing a memory resets the decay clock.
///
/// Returns the new importance value (does not mutate the memory).
pub fn compute_decay(memory: &Memory, half_life_hours: f64) -> f64 {
    let hours_since_access = memory
        .time_since_access()
        .num_seconds()
        .max(0) as f64
        / 3600.0;

    // Exponential decay: importance * 2^(-t/half_life)
    let decay_factor = (-hours_since_access * (2.0_f64.ln()) / half_life_hours).exp();

    (memory.importance * decay_factor).clamp(0.0, 1.0)
}

/// Apply decay to a memory in-place and return the old importance.
pub fn apply_decay(memory: &mut Memory, half_life_hours: f64) -> f64 {
    let old = memory.importance;
    memory.importance = compute_decay(memory, half_life_hours);
    old
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;
    use crate::types::MemoryKind;
    use serde_json::json;

    #[test]
    fn recall_query_builder() {
        let q = RecallQuery::new()
            .by_kind(MemoryKind::Semantic)
            .by_tags(vec!["rust".into()])
            .by_importance_threshold(0.5)
            .limit(10)
            .strategy(RecallStrategy::MostImportant);

        assert_eq!(q.kind_filter, Some(MemoryKind::Semantic));
        assert_eq!(q.tag_filters, vec!["rust"]);
        assert_eq!(q.importance_threshold, Some(0.5));
        assert_eq!(q.result_limit, 10);
        assert_eq!(q.ranking_strategy, RecallStrategy::MostImportant);
    }

    #[tokio::test]
    async fn execute_recall_filters_and_ranks() {
        let store = InMemoryStore::new();

        let m1 = Memory::new(MemoryKind::Semantic, json!("Rust is fast"))
            .with_importance(0.9)
            .with_tags(vec!["lang".into()]);
        let m2 = Memory::new(MemoryKind::Episodic, json!("deployed service"))
            .with_importance(0.3)
            .with_tags(vec!["deploy".into()]);
        let m3 = Memory::new(MemoryKind::Semantic, json!("Go has goroutines"))
            .with_importance(0.7)
            .with_tags(vec!["lang".into()]);

        store.store(&m1).await.unwrap();
        store.store(&m2).await.unwrap();
        store.store(&m3).await.unwrap();

        // Filter by kind + sort by importance.
        let query = RecallQuery::new()
            .by_kind(MemoryKind::Semantic)
            .strategy(RecallStrategy::MostImportant);

        let result = execute_recall(&store, &query).await.unwrap();
        assert_eq!(result.total_matches, 2);
        assert_eq!(result.memories.len(), 2);
        // Most important first.
        assert!(result.memories[0].importance >= result.memories[1].importance);
    }

    #[tokio::test]
    async fn execute_recall_content_search() {
        let store = InMemoryStore::new();
        store
            .store(&Memory::new(MemoryKind::Semantic, json!("Rust ownership")))
            .await
            .unwrap();
        store
            .store(&Memory::new(MemoryKind::Semantic, json!("Python GIL")))
            .await
            .unwrap();

        let query = RecallQuery::new().by_content_search("rust");
        let result = execute_recall(&store, &query).await.unwrap();
        assert_eq!(result.memories.len(), 1);
    }

    #[tokio::test]
    async fn execute_recall_tag_filter() {
        let store = InMemoryStore::new();
        store
            .store(
                &Memory::new(MemoryKind::Semantic, json!("a"))
                    .with_tags(vec!["x".into(), "y".into()]),
            )
            .await
            .unwrap();
        store
            .store(
                &Memory::new(MemoryKind::Semantic, json!("b"))
                    .with_tags(vec!["x".into()]),
            )
            .await
            .unwrap();

        // Require both x and y.
        let query = RecallQuery::new().by_tags(vec!["x".into(), "y".into()]);
        let result = execute_recall(&store, &query).await.unwrap();
        assert_eq!(result.memories.len(), 1);
    }

    #[tokio::test]
    async fn execute_recall_filters_and_ranks_beyond_old_candidate_limit() {
        // Sled orders these explicit keys, placing both valid matches after
        // the old ten-candidate boundary for a one-result query.
        let store = crate::store::SledStore::temporary().unwrap();
        let best_id = crate::types::MemoryId::from_uuid(uuid::Uuid::from_u128(12));
        for index in 1..=12 {
            let mut memory = Memory::new(MemoryKind::Semantic, json!("needle"))
                .with_tags(vec![if index > 10 { "target" } else { "noise" }.into()])
                .with_importance(index as f64 / 12.0);
            memory.id = crate::types::MemoryId::from_uuid(uuid::Uuid::from_u128(index));
            store.store(&memory).await.unwrap();
        }

        // Exercise both candidate sources and ensure the limit affects only
        // returned memories, not total_matches or global ranking.
        for content_search in [false, true] {
            let mut query = RecallQuery::new()
                .by_tags(vec!["target".into()])
                .strategy(RecallStrategy::MostImportant)
                .limit(1);
            if content_search {
                query = query.by_content_search("needle");
            }
            let result = execute_recall(&store, &query).await.unwrap();
            assert_eq!(result.total_matches, 2);
            assert_eq!(result.memories.len(), 1);
            assert_eq!(result.memories[0].id, best_id);

            let result = execute_recall(&store, &query.limit(0)).await.unwrap();
            assert_eq!(result.total_matches, 2);
            assert!(result.memories.is_empty());
        }
    }

    #[test]
    fn decay_reduces_importance() {
        let mut m = Memory::new(MemoryKind::Semantic, json!("old fact"))
            .with_importance(1.0);
        // Simulate being accessed a long time ago.
        m.last_accessed = Utc::now() - chrono::Duration::hours(168);

        let decayed = compute_decay(&m, 168.0);
        // After one half-life, importance should be ~0.5.
        assert!((decayed - 0.5).abs() < 0.05);
    }

    #[test]
    fn decay_is_zero_for_fresh_memory() {
        let m = Memory::new(MemoryKind::Semantic, json!("fresh"))
            .with_importance(0.8);
        let decayed = compute_decay(&m, 168.0);
        // Freshly accessed memory should retain nearly full importance.
        assert!((decayed - 0.8).abs() < 0.01);
    }

    #[test]
    fn apply_decay_mutates() {
        let mut m = Memory::new(MemoryKind::Semantic, json!("test"))
            .with_importance(0.9);
        m.last_accessed = Utc::now() - chrono::Duration::hours(336); // 2 half-lives
        let old = apply_decay(&mut m, 168.0);
        assert!((old - 0.9).abs() < f64::EPSILON);
        assert!(m.importance < 0.3);
    }
}
