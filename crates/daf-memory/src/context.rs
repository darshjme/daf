//! Working memory / context window management.
//!
//! Working memory is the agent's "scratchpad" — a bounded, LRU-evicted
//! set of memories that represent the current context for decision-making.
//! It bridges persistent memory and the agent's active reasoning.

use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::{debug, instrument, warn};

use crate::error::MemoryResult;
use crate::recall::{RecallQuery, RecallStrategy};
use crate::store::MemoryStore;
use crate::types::{Memory, MemoryId, MemoryKind, MemoryTier};

// ---------------------------------------------------------------------------
// WorkingMemory
// ---------------------------------------------------------------------------

/// Bounded working memory with LRU eviction.
///
/// Represents the agent's current context window. Memories are stored in
/// access order; when capacity is exceeded, the least recently used memory
/// is evicted. Changes can be persisted back to the underlying store.
#[derive(Debug)]
pub struct WorkingMemory {
    /// The memory entries in LRU order (most recently accessed at the back).
    entries: RwLock<VecDeque<Memory>>,
    /// Maximum number of memories in working memory.
    capacity: usize,
}

impl WorkingMemory {
    /// Create a new working memory with the given capacity.
    ///
    /// Typical capacities are 32-128, depending on the agent's context
    /// window size and the complexity of the task.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "working memory capacity must be > 0");
        Self {
            entries: RwLock::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Current number of memories in working memory.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Check if working memory is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Check if working memory is at capacity.
    pub fn is_full(&self) -> bool {
        self.entries.read().len() >= self.capacity
    }

    /// Maximum capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Add a memory to working memory.
    ///
    /// If the memory already exists (by ID), it is moved to the most-recently-
    /// used position. If at capacity, the least-recently-used memory is evicted.
    /// Returns the evicted memory, if any.
    pub fn add(&self, memory: Memory) -> Option<Memory> {
        let mut entries = self.entries.write();

        // Remove existing entry with same ID (will re-add at back).
        entries.retain(|m| m.id != memory.id);

        // Evict LRU if at capacity.
        let evicted = if entries.len() >= self.capacity {
            entries.pop_front()
        } else {
            None
        };

        entries.push_back(memory);
        evicted
    }

    /// Get a memory by ID, promoting it to the most-recently-used position.
    pub fn get(&self, id: &MemoryId) -> Option<Memory> {
        let mut entries = self.entries.write();
        if let Some(pos) = entries.iter().position(|m| m.id == *id) {
            let memory = entries.remove(pos).unwrap();
            let cloned = memory.clone();
            entries.push_back(memory);
            Some(cloned)
        } else {
            None
        }
    }

    /// Remove a memory from working memory by ID.
    pub fn remove(&self, id: &MemoryId) -> Option<Memory> {
        let mut entries = self.entries.write();
        if let Some(pos) = entries.iter().position(|m| m.id == *id) {
            entries.remove(pos)
        } else {
            None
        }
    }

    /// Get all memories in working memory (LRU order, oldest first).
    pub fn all(&self) -> Vec<Memory> {
        self.entries.read().iter().cloned().collect()
    }

    /// Get the most recently used memories (up to `n`).
    pub fn most_recent(&self, n: usize) -> Vec<Memory> {
        let entries = self.entries.read();
        entries.iter().rev().take(n).cloned().collect()
    }

    /// Clear all entries from working memory.
    pub fn clear(&self) {
        self.entries.write().clear();
    }

    /// Search working memory for entries matching a predicate.
    pub fn search<F>(&self, predicate: F) -> Vec<Memory>
    where
        F: Fn(&Memory) -> bool,
    {
        self.entries
            .read()
            .iter()
            .filter(|m| predicate(m))
            .cloned()
            .collect()
    }

    /// Search working memory by content substring.
    pub fn search_content(&self, query: &str) -> Vec<Memory> {
        let query_lower = query.to_lowercase();
        self.search(|m| {
            m.content
                .to_string()
                .to_lowercase()
                .contains(&query_lower)
        })
    }

    /// Search working memory by tag.
    pub fn search_tag(&self, tag: &str) -> Vec<Memory> {
        self.search(|m| m.tags.iter().any(|t| t == tag))
    }
}

// ---------------------------------------------------------------------------
// Context operations
// ---------------------------------------------------------------------------

/// Load relevant context from the store into working memory.
///
/// Given a task description and tags, queries the memory store for relevant
/// memories and loads them into the working memory. Returns the number of
/// memories loaded.
///
/// This is typically called at the start of a task to prime the agent's
/// context with relevant past experience.
#[instrument(skip(store, working_memory))]
pub async fn load_context(
    store: &dyn MemoryStore,
    working_memory: &WorkingMemory,
    task_description: &str,
    tags: &[String],
    max_memories: usize,
) -> MemoryResult<usize> {
    let mut query = RecallQuery::new()
        .by_content_search(task_description)
        .strategy(RecallStrategy::MostRelevant)
        .limit(max_memories);

    if !tags.is_empty() {
        query = query.by_tags(tags.to_vec());
    }

    let result = crate::recall::execute_recall(store, &query).await?;
    let loaded = result.memories.len();

    for memory in result.memories {
        working_memory.add(memory);
    }

    debug!(loaded = loaded, "loaded context from store");
    Ok(loaded)
}

/// Update working memory with a new observation or fact.
///
/// Creates a Working-kind memory and adds it to the working set.
/// Returns the ID of the new memory.
pub fn update_context(
    working_memory: &WorkingMemory,
    content: serde_json::Value,
    tags: Vec<String>,
) -> MemoryId {
    let memory = Memory::new(MemoryKind::Working, content)
        .with_tier(MemoryTier::Hot)
        .with_importance(0.6)
        .with_tags(tags);
    let id = memory.id;
    working_memory.add(memory);
    debug!(id = %id, "updated context");
    id
}

/// Persist all working memory entries back to the store.
///
/// This is typically called at the end of a task or session. Working-kind
/// memories are upgraded to Episodic before persistence so they survive
/// across sessions.
#[instrument(skip(store, working_memory))]
pub async fn save_context(
    store: &dyn MemoryStore,
    working_memory: &WorkingMemory,
) -> MemoryResult<usize> {
    let entries = working_memory.all();
    let count = entries.len();

    for mut memory in entries {
        // Upgrade Working → Episodic for persistence.
        if memory.kind == MemoryKind::Working {
            memory.kind = MemoryKind::Episodic;
            memory.tier = MemoryTier::Warm;
        }
        if let Err(e) = store.store(&memory).await {
            warn!(id = %memory.id, error = %e, "failed to persist working memory");
        }
    }

    debug!(saved = count, "saved context to store");
    Ok(count)
}

// ---------------------------------------------------------------------------
// ContextManager
// ---------------------------------------------------------------------------

/// High-level wrapper combining working memory with a backing store.
///
/// Provides the full context lifecycle: load, update, save, with automatic
/// persistence management.
#[derive(Clone)]
pub struct ContextManager {
    store: Arc<dyn MemoryStore>,
    working_memory: Arc<WorkingMemory>,
}

impl std::fmt::Debug for ContextManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextManager")
            .field("capacity", &self.working_memory.capacity())
            .field("size", &self.working_memory.len())
            .finish()
    }
}

impl ContextManager {
    /// Create a new context manager.
    pub fn new(store: Arc<dyn MemoryStore>, capacity: usize) -> Self {
        Self {
            store,
            working_memory: Arc::new(WorkingMemory::new(capacity)),
        }
    }

    /// Get a reference to the working memory.
    pub fn working_memory(&self) -> &WorkingMemory {
        &self.working_memory
    }

    /// Load context for a task.
    pub async fn load(
        &self,
        task_description: &str,
        tags: &[String],
        max_memories: usize,
    ) -> MemoryResult<usize> {
        load_context(
            self.store.as_ref(),
            &self.working_memory,
            task_description,
            tags,
            max_memories,
        )
        .await
    }

    /// Add a new observation to working memory.
    pub fn observe(&self, content: serde_json::Value, tags: Vec<String>) -> MemoryId {
        update_context(&self.working_memory, content, tags)
    }

    /// Persist working memory back to the store.
    pub async fn save(&self) -> MemoryResult<usize> {
        save_context(self.store.as_ref(), &self.working_memory).await
    }

    /// Clear working memory without persisting.
    pub fn clear(&self) {
        self.working_memory.clear();
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

    #[test]
    fn working_memory_lru_eviction() {
        let wm = WorkingMemory::new(3);

        let m1 = Memory::new(MemoryKind::Working, json!("first"));
        let m2 = Memory::new(MemoryKind::Working, json!("second"));
        let m3 = Memory::new(MemoryKind::Working, json!("third"));
        let m4 = Memory::new(MemoryKind::Working, json!("fourth"));

        let id1 = m1.id;

        wm.add(m1);
        wm.add(m2);
        wm.add(m3);
        assert_eq!(wm.len(), 3);
        assert!(wm.is_full());

        // Adding fourth should evict first (LRU).
        let evicted = wm.add(m4);
        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().id, id1);
        assert_eq!(wm.len(), 3);
    }

    #[test]
    fn working_memory_access_promotes() {
        let wm = WorkingMemory::new(3);

        let m1 = Memory::new(MemoryKind::Working, json!("first"));
        let m2 = Memory::new(MemoryKind::Working, json!("second"));
        let m3 = Memory::new(MemoryKind::Working, json!("third"));

        let id1 = m1.id;
        let id2 = m2.id;

        wm.add(m1);
        wm.add(m2);
        wm.add(m3);

        // Access m1 (currently LRU), promoting it to MRU.
        wm.get(&id1);

        // Now m2 should be LRU and evicted when adding m4.
        let m4 = Memory::new(MemoryKind::Working, json!("fourth"));
        let evicted = wm.add(m4);
        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().id, id2);
    }

    #[test]
    fn working_memory_dedup_on_add() {
        let wm = WorkingMemory::new(5);

        let m = Memory::new(MemoryKind::Working, json!("test"));
        let id = m.id;
        wm.add(m.clone());
        wm.add(m); // Same ID, should not duplicate.
        assert_eq!(wm.len(), 1);

        assert!(wm.get(&id).is_some());
    }

    #[test]
    fn working_memory_remove() {
        let wm = WorkingMemory::new(5);
        let m = Memory::new(MemoryKind::Working, json!("test"));
        let id = m.id;
        wm.add(m);
        assert_eq!(wm.len(), 1);

        let removed = wm.remove(&id);
        assert!(removed.is_some());
        assert_eq!(wm.len(), 0);
    }

    #[test]
    fn working_memory_search_content() {
        let wm = WorkingMemory::new(10);
        wm.add(Memory::new(MemoryKind::Working, json!("Rust is fast")));
        wm.add(Memory::new(MemoryKind::Working, json!("Python is slow")));

        let results = wm.search_content("rust");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn working_memory_search_tag() {
        let wm = WorkingMemory::new(10);
        wm.add(
            Memory::new(MemoryKind::Working, json!("a"))
                .with_tags(vec!["lang".into()]),
        );
        wm.add(
            Memory::new(MemoryKind::Working, json!("b"))
                .with_tags(vec!["deploy".into()]),
        );

        let results = wm.search_tag("lang");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn working_memory_most_recent() {
        let wm = WorkingMemory::new(10);
        wm.add(Memory::new(MemoryKind::Working, json!("old")));
        wm.add(Memory::new(MemoryKind::Working, json!("new")));

        let recent = wm.most_recent(1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].content, json!("new"));
    }

    #[test]
    fn update_context_creates_memory() {
        let wm = WorkingMemory::new(10);
        let id = update_context(&wm, json!("observation"), vec!["test".into()]);
        assert_eq!(wm.len(), 1);
        let m = wm.get(&id).unwrap();
        assert_eq!(m.kind, MemoryKind::Working);
        assert_eq!(m.tier, MemoryTier::Hot);
    }

    #[tokio::test]
    async fn save_context_upgrades_working_to_episodic() {
        let store = Arc::new(InMemoryStore::new());
        let wm = WorkingMemory::new(10);

        let m = Memory::new(MemoryKind::Working, json!("session data"));
        let id = m.id;
        wm.add(m);

        let saved = save_context(store.as_ref(), &wm).await.unwrap();
        assert_eq!(saved, 1);

        let persisted = store.retrieve(&id).await.unwrap().unwrap();
        assert_eq!(persisted.kind, MemoryKind::Episodic);
        assert_eq!(persisted.tier, MemoryTier::Warm);
    }

    #[tokio::test]
    async fn load_context_populates_working_memory() {
        let store = Arc::new(InMemoryStore::new());

        // Pre-populate store with relevant memories.
        store
            .store(
                &Memory::new(MemoryKind::Semantic, json!("Rust borrow checker"))
                    .with_tags(vec!["rust".into()]),
            )
            .await
            .unwrap();
        store
            .store(
                &Memory::new(MemoryKind::Semantic, json!("Python GIL"))
                    .with_tags(vec!["python".into()]),
            )
            .await
            .unwrap();

        let wm = WorkingMemory::new(10);
        let loaded = load_context(store.as_ref(), &wm, "rust", &[], 10)
            .await
            .unwrap();
        assert_eq!(loaded, 1); // Only "Rust borrow checker" matches.
        assert_eq!(wm.len(), 1);
    }

    #[tokio::test]
    async fn context_manager_full_lifecycle() {
        let store = Arc::new(InMemoryStore::new());
        store
            .store(&Memory::new(MemoryKind::Semantic, json!("background knowledge")))
            .await
            .unwrap();

        let ctx = ContextManager::new(store.clone(), 20);

        // Load.
        let loaded = ctx.load("background", &[], 10).await.unwrap();
        assert_eq!(loaded, 1);

        // Observe.
        ctx.observe(json!("new observation"), vec!["test".into()]);
        assert_eq!(ctx.working_memory().len(), 2);

        // Save.
        let saved = ctx.save().await.unwrap();
        assert_eq!(saved, 2);

        // Clear.
        ctx.clear();
        assert!(ctx.working_memory().is_empty());
    }
}
