//! Memory storage backends.
//!
//! Provides the [`MemoryStore`] trait and three implementations:
//! - [`InMemoryStore`] — hash-map based, for testing and ephemeral use.
//! - [`SledStore`] — fast embedded B-tree store for hot/warm memory.
//! - [`RocksStore`] — RocksDB-backed store for cold/archival memory.
//!
//! Each backend handles serialization internally so callers work with
//! typed [`Memory`] values.

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, instrument};

use crate::error::{MemoryError, MemoryResult};
use crate::types::{Memory, MemoryId, MemoryMetrics};

// ---------------------------------------------------------------------------
// MemoryStore trait
// ---------------------------------------------------------------------------

/// Async trait for memory persistence backends.
///
/// All operations are infallible at the type level — errors are returned as
/// [`MemoryResult`]. Implementations must be `Send + Sync` for use across
/// async tasks.
#[async_trait]
pub trait MemoryStore: Send + Sync + std::fmt::Debug {
    /// Persist a memory. If a memory with the same ID already exists, it is
    /// overwritten (upsert semantics).
    async fn store(&self, memory: &Memory) -> MemoryResult<()>;

    /// Retrieve a single memory by ID. Returns `None` if not found.
    async fn retrieve(&self, id: &MemoryId) -> MemoryResult<Option<Memory>>;

    /// Full-text-ish search over memory content. Returns memories whose
    /// serialized content contains the query string (case-insensitive).
    /// Limited to `limit` results.
    async fn search(&self, query: &str, limit: usize) -> MemoryResult<Vec<Memory>>;

    /// Update an existing memory in place. Returns an error if the memory
    /// does not exist.
    async fn update(&self, memory: &Memory) -> MemoryResult<()>;

    /// Delete a memory by ID. No-op if the memory does not exist.
    async fn delete(&self, id: &MemoryId) -> MemoryResult<()>;

    /// List all memories tagged with the given tag.
    async fn list_by_tag(&self, tag: &str, limit: usize) -> MemoryResult<Vec<Memory>>;

    /// List all memories in the store (bounded by limit).
    async fn list_all(&self, limit: usize) -> MemoryResult<Vec<Memory>>;

    /// Return aggregate metrics for this store.
    async fn metrics(&self) -> MemoryResult<MemoryMetrics>;
}

// ---------------------------------------------------------------------------
// InMemoryStore
// ---------------------------------------------------------------------------

/// Hash-map-backed memory store for testing and ephemeral use.
///
/// All data lives in memory and is lost when the process exits. Thread-safe
/// via [`DashMap`].
#[derive(Debug, Clone)]
pub struct InMemoryStore {
    data: Arc<DashMap<MemoryId, Memory>>,
}

impl InMemoryStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self {
            data: Arc::new(DashMap::new()),
        }
    }

    /// Return the number of stored memories.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    #[instrument(skip(self, memory), fields(id = %memory.id))]
    async fn store(&self, memory: &Memory) -> MemoryResult<()> {
        self.data.insert(memory.id, memory.clone());
        debug!("stored memory {}", memory.id);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn retrieve(&self, id: &MemoryId) -> MemoryResult<Option<Memory>> {
        Ok(self.data.get(id).map(|entry| entry.value().clone()))
    }

    #[instrument(skip(self))]
    async fn search(&self, query: &str, limit: usize) -> MemoryResult<Vec<Memory>> {
        let query_lower = query.to_lowercase();
        let results: Vec<Memory> = self
            .data
            .iter()
            .filter(|entry| {
                let content_str = entry.value().content.to_string().to_lowercase();
                content_str.contains(&query_lower)
            })
            .take(limit)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(results)
    }

    #[instrument(skip(self, memory), fields(id = %memory.id))]
    async fn update(&self, memory: &Memory) -> MemoryResult<()> {
        if !self.data.contains_key(&memory.id) {
            return Err(MemoryError::NotFound(memory.id));
        }
        self.data.insert(memory.id, memory.clone());
        Ok(())
    }

    #[instrument(skip(self))]
    async fn delete(&self, id: &MemoryId) -> MemoryResult<()> {
        self.data.remove(id);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn list_by_tag(&self, tag: &str, limit: usize) -> MemoryResult<Vec<Memory>> {
        let results: Vec<Memory> = self
            .data
            .iter()
            .filter(|entry| entry.value().tags.iter().any(|t| t == tag))
            .take(limit)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(results)
    }

    #[instrument(skip(self))]
    async fn list_all(&self, limit: usize) -> MemoryResult<Vec<Memory>> {
        let results: Vec<Memory> = self
            .data
            .iter()
            .take(limit)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(results)
    }

    async fn metrics(&self) -> MemoryResult<MemoryMetrics> {
        let mut metrics = MemoryMetrics::default();
        metrics.total_memories = self.data.len() as u64;
        for entry in self.data.iter() {
            let m = entry.value();
            *metrics
                .by_tier
                .entry(m.tier.to_string())
                .or_insert(0) += 1;
            *metrics
                .by_kind
                .entry(m.kind.to_string())
                .or_insert(0) += 1;
        }
        // Approximate storage by serializing one entry if available.
        if let Some(entry) = self.data.iter().next() {
            let sample_size = serde_json::to_vec(entry.value())
                .map(|v| v.len())
                .unwrap_or(256);
            metrics.storage_bytes = (sample_size as u64) * metrics.total_memories;
        }
        Ok(metrics)
    }
}

// ---------------------------------------------------------------------------
// SledStore
// ---------------------------------------------------------------------------

/// Sled-backed embedded store for hot and warm memory tiers.
///
/// Sled provides a lock-free B+ tree with crash-safe persistence and
/// sub-millisecond reads for hot data. Ideal for the working set.
#[derive(Debug, Clone)]
pub struct SledStore {
    db: sled::Db,
}

impl SledStore {
    /// Open or create a sled database at the given path.
    pub fn open(path: impl AsRef<Path>) -> MemoryResult<Self> {
        let db = sled::open(path.as_ref()).map_err(|e| {
            MemoryError::StoreError(format!("failed to open sled db: {e}"))
        })?;
        Ok(Self { db })
    }

    /// Open a temporary sled database (useful for tests).
    pub fn temporary() -> MemoryResult<Self> {
        let config = sled::Config::new().temporary(true);
        let db = config.open().map_err(|e| {
            MemoryError::StoreError(format!("failed to open temporary sled db: {e}"))
        })?;
        Ok(Self { db })
    }

    fn key(id: &MemoryId) -> Vec<u8> {
        id.0.as_bytes().to_vec()
    }

    fn serialize(memory: &Memory) -> MemoryResult<Vec<u8>> {
        serde_json::to_vec(memory).map_err(|e| {
            MemoryError::SerializationError(format!("failed to serialize memory: {e}"))
        })
    }

    fn deserialize(bytes: &[u8]) -> MemoryResult<Memory> {
        serde_json::from_slice(bytes).map_err(|e| {
            MemoryError::SerializationError(format!("failed to deserialize memory: {e}"))
        })
    }
}

#[async_trait]
impl MemoryStore for SledStore {
    #[instrument(skip(self, memory), fields(id = %memory.id))]
    async fn store(&self, memory: &Memory) -> MemoryResult<()> {
        let key = Self::key(&memory.id);
        let value = Self::serialize(memory)?;
        self.db.insert(key, value).map_err(|e| {
            MemoryError::StoreError(format!("sled insert failed: {e}"))
        })?;
        debug!("stored memory {} in sled", memory.id);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn retrieve(&self, id: &MemoryId) -> MemoryResult<Option<Memory>> {
        let key = Self::key(id);
        match self.db.get(key) {
            Ok(Some(bytes)) => Ok(Some(Self::deserialize(&bytes)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(MemoryError::StoreError(format!("sled get failed: {e}"))),
        }
    }

    #[instrument(skip(self))]
    async fn search(&self, query: &str, limit: usize) -> MemoryResult<Vec<Memory>> {
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();
        for entry in self.db.iter() {
            if results.len() >= limit {
                break;
            }
            let (_, value) = entry.map_err(|e| {
                MemoryError::StoreError(format!("sled iteration error: {e}"))
            })?;
            let memory = Self::deserialize(&value)?;
            if memory.content.to_string().to_lowercase().contains(&query_lower) {
                results.push(memory);
            }
        }
        Ok(results)
    }

    #[instrument(skip(self, memory), fields(id = %memory.id))]
    async fn update(&self, memory: &Memory) -> MemoryResult<()> {
        let key = Self::key(&memory.id);
        if !self.db.contains_key(&key).map_err(|e| {
            MemoryError::StoreError(format!("sled contains_key failed: {e}"))
        })? {
            return Err(MemoryError::NotFound(memory.id));
        }
        let value = Self::serialize(memory)?;
        self.db.insert(key, value).map_err(|e| {
            MemoryError::StoreError(format!("sled update failed: {e}"))
        })?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn delete(&self, id: &MemoryId) -> MemoryResult<()> {
        let key = Self::key(id);
        self.db.remove(key).map_err(|e| {
            MemoryError::StoreError(format!("sled delete failed: {e}"))
        })?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn list_by_tag(&self, tag: &str, limit: usize) -> MemoryResult<Vec<Memory>> {
        let mut results = Vec::new();
        for entry in self.db.iter() {
            if results.len() >= limit {
                break;
            }
            let (_, value) = entry.map_err(|e| {
                MemoryError::StoreError(format!("sled iteration error: {e}"))
            })?;
            let memory = Self::deserialize(&value)?;
            if memory.tags.iter().any(|t| t == tag) {
                results.push(memory);
            }
        }
        Ok(results)
    }

    #[instrument(skip(self))]
    async fn list_all(&self, limit: usize) -> MemoryResult<Vec<Memory>> {
        let mut results = Vec::new();
        for entry in self.db.iter() {
            if results.len() >= limit {
                break;
            }
            let (_, value) = entry.map_err(|e| {
                MemoryError::StoreError(format!("sled iteration error: {e}"))
            })?;
            results.push(Self::deserialize(&value)?);
        }
        Ok(results)
    }

    async fn metrics(&self) -> MemoryResult<MemoryMetrics> {
        let mut metrics = MemoryMetrics::default();
        metrics.total_memories = self.db.len() as u64;
        metrics.storage_bytes = self.db.size_on_disk().unwrap_or(0);
        for entry in self.db.iter() {
            let (_, value) = entry.map_err(|e| {
                MemoryError::StoreError(format!("sled iteration error: {e}"))
            })?;
            let memory = Self::deserialize(&value)?;
            *metrics.by_tier.entry(memory.tier.to_string()).or_insert(0) += 1;
            *metrics.by_kind.entry(memory.kind.to_string()).or_insert(0) += 1;
        }
        Ok(metrics)
    }
}

// ---------------------------------------------------------------------------
// RocksStore
// ---------------------------------------------------------------------------

/// RocksDB-backed store for cold/archival memory.
///
/// Optimized for large datasets with efficient compression and range scans.
/// Suitable for memories that are rarely accessed but must be retained.
#[derive(Debug)]
pub struct RocksStore {
    db: rocksdb::DB,
}

impl RocksStore {
    /// Open or create a RocksDB database at the given path.
    pub fn open(path: impl AsRef<Path>) -> MemoryResult<Self> {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        opts.set_max_open_files(256);
        opts.set_write_buffer_size(64 * 1024 * 1024); // 64 MB

        let db = rocksdb::DB::open(&opts, path.as_ref()).map_err(|e| {
            MemoryError::StoreError(format!("failed to open RocksDB: {e}"))
        })?;
        Ok(Self { db })
    }

    fn key(id: &MemoryId) -> Vec<u8> {
        id.0.as_bytes().to_vec()
    }

    fn serialize(memory: &Memory) -> MemoryResult<Vec<u8>> {
        serde_json::to_vec(memory).map_err(|e| {
            MemoryError::SerializationError(format!("failed to serialize memory: {e}"))
        })
    }

    fn deserialize(bytes: &[u8]) -> MemoryResult<Memory> {
        serde_json::from_slice(bytes).map_err(|e| {
            MemoryError::SerializationError(format!("failed to deserialize memory: {e}"))
        })
    }
}

#[async_trait]
impl MemoryStore for RocksStore {
    #[instrument(skip(self, memory), fields(id = %memory.id))]
    async fn store(&self, memory: &Memory) -> MemoryResult<()> {
        let key = Self::key(&memory.id);
        let value = Self::serialize(memory)?;
        self.db.put(&key, &value).map_err(|e| {
            MemoryError::StoreError(format!("rocksdb put failed: {e}"))
        })?;
        debug!("stored memory {} in rocksdb", memory.id);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn retrieve(&self, id: &MemoryId) -> MemoryResult<Option<Memory>> {
        let key = Self::key(id);
        match self.db.get(&key) {
            Ok(Some(bytes)) => Ok(Some(Self::deserialize(&bytes)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(MemoryError::StoreError(format!("rocksdb get failed: {e}"))),
        }
    }

    #[instrument(skip(self))]
    async fn search(&self, query: &str, limit: usize) -> MemoryResult<Vec<Memory>> {
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();
        let iter = self.db.iterator(rocksdb::IteratorMode::Start);
        for item in iter {
            if results.len() >= limit {
                break;
            }
            let (_, value) = item.map_err(|e| {
                MemoryError::StoreError(format!("rocksdb iteration error: {e}"))
            })?;
            let memory = Self::deserialize(&value)?;
            if memory.content.to_string().to_lowercase().contains(&query_lower) {
                results.push(memory);
            }
        }
        Ok(results)
    }

    #[instrument(skip(self, memory), fields(id = %memory.id))]
    async fn update(&self, memory: &Memory) -> MemoryResult<()> {
        let key = Self::key(&memory.id);
        if self.db.get(&key).map_err(|e| {
            MemoryError::StoreError(format!("rocksdb get failed: {e}"))
        })?.is_none() {
            return Err(MemoryError::NotFound(memory.id));
        }
        let value = Self::serialize(memory)?;
        self.db.put(&key, &value).map_err(|e| {
            MemoryError::StoreError(format!("rocksdb update failed: {e}"))
        })?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn delete(&self, id: &MemoryId) -> MemoryResult<()> {
        let key = Self::key(id);
        self.db.delete(&key).map_err(|e| {
            MemoryError::StoreError(format!("rocksdb delete failed: {e}"))
        })?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn list_by_tag(&self, tag: &str, limit: usize) -> MemoryResult<Vec<Memory>> {
        let mut results = Vec::new();
        let iter = self.db.iterator(rocksdb::IteratorMode::Start);
        for item in iter {
            if results.len() >= limit {
                break;
            }
            let (_, value) = item.map_err(|e| {
                MemoryError::StoreError(format!("rocksdb iteration error: {e}"))
            })?;
            let memory = Self::deserialize(&value)?;
            if memory.tags.iter().any(|t| t == tag) {
                results.push(memory);
            }
        }
        Ok(results)
    }

    #[instrument(skip(self))]
    async fn list_all(&self, limit: usize) -> MemoryResult<Vec<Memory>> {
        let mut results = Vec::new();
        let iter = self.db.iterator(rocksdb::IteratorMode::Start);
        for item in iter {
            if results.len() >= limit {
                break;
            }
            let (_, value) = item.map_err(|e| {
                MemoryError::StoreError(format!("rocksdb iteration error: {e}"))
            })?;
            results.push(Self::deserialize(&value)?);
        }
        Ok(results)
    }

    async fn metrics(&self) -> MemoryResult<MemoryMetrics> {
        let mut metrics = MemoryMetrics::default();
        let iter = self.db.iterator(rocksdb::IteratorMode::Start);
        for item in iter {
            let (_, value) = item.map_err(|e| {
                MemoryError::StoreError(format!("rocksdb iteration error: {e}"))
            })?;
            metrics.total_memories += 1;
            metrics.storage_bytes += value.len() as u64;
            if let Ok(memory) = Self::deserialize(&value) {
                *metrics.by_tier.entry(memory.tier.to_string()).or_insert(0) += 1;
                *metrics.by_kind.entry(memory.kind.to_string()).or_insert(0) += 1;
            }
        }
        Ok(metrics)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryKind;
    use serde_json::json;

    async fn store_and_retrieve(store: &dyn MemoryStore) {
        let mem = Memory::new(MemoryKind::Semantic, json!({"fact": "Rust is fast"}))
            .with_tags(vec!["lang".into(), "perf".into()])
            .with_importance(0.9);

        store.store(&mem).await.unwrap();

        let retrieved = store.retrieve(&mem.id).await.unwrap().unwrap();
        assert_eq!(retrieved.id, mem.id);
        assert_eq!(retrieved.content, mem.content);
        assert_eq!(retrieved.tags, mem.tags);
    }

    async fn search_works(store: &dyn MemoryStore) {
        let m1 = Memory::new(MemoryKind::Semantic, json!("Rust has zero-cost abstractions"));
        let m2 = Memory::new(MemoryKind::Semantic, json!("Python is dynamically typed"));

        store.store(&m1).await.unwrap();
        store.store(&m2).await.unwrap();

        let results = store.search("rust", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, m1.id);
    }

    async fn delete_works(store: &dyn MemoryStore) {
        let mem = Memory::new(MemoryKind::Episodic, json!("something happened"));
        store.store(&mem).await.unwrap();
        store.delete(&mem.id).await.unwrap();
        assert!(store.retrieve(&mem.id).await.unwrap().is_none());
    }

    async fn update_works(store: &dyn MemoryStore) {
        let mut mem = Memory::new(MemoryKind::Episodic, json!("v1"));
        store.store(&mem).await.unwrap();

        mem.content = json!("v2");
        store.update(&mem).await.unwrap();

        let retrieved = store.retrieve(&mem.id).await.unwrap().unwrap();
        assert_eq!(retrieved.content, json!("v2"));
    }

    async fn list_by_tag_works(store: &dyn MemoryStore) {
        let m1 = Memory::new(MemoryKind::Semantic, json!("a"))
            .with_tags(vec!["alpha".into()]);
        let m2 = Memory::new(MemoryKind::Semantic, json!("b"))
            .with_tags(vec!["beta".into()]);

        store.store(&m1).await.unwrap();
        store.store(&m2).await.unwrap();

        let results = store.list_by_tag("alpha", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, m1.id);
    }

    #[tokio::test]
    async fn in_memory_store_crud() {
        let store = InMemoryStore::new();
        store_and_retrieve(&store).await;
    }

    #[tokio::test]
    async fn in_memory_store_search() {
        let store = InMemoryStore::new();
        search_works(&store).await;
    }

    #[tokio::test]
    async fn in_memory_store_delete() {
        let store = InMemoryStore::new();
        delete_works(&store).await;
    }

    #[tokio::test]
    async fn in_memory_store_update() {
        let store = InMemoryStore::new();
        update_works(&store).await;
    }

    #[tokio::test]
    async fn in_memory_store_list_by_tag() {
        let store = InMemoryStore::new();
        list_by_tag_works(&store).await;
    }

    #[tokio::test]
    async fn sled_store_crud() {
        let store = SledStore::temporary().unwrap();
        store_and_retrieve(&store).await;
    }

    #[tokio::test]
    async fn sled_store_search() {
        let store = SledStore::temporary().unwrap();
        search_works(&store).await;
    }

    #[tokio::test]
    async fn sled_store_delete() {
        let store = SledStore::temporary().unwrap();
        delete_works(&store).await;
    }

    #[tokio::test]
    async fn sled_store_update() {
        let store = SledStore::temporary().unwrap();
        update_works(&store).await;
    }

    #[tokio::test]
    async fn sled_store_list_by_tag() {
        let store = SledStore::temporary().unwrap();
        list_by_tag_works(&store).await;
    }

    #[tokio::test]
    async fn rocks_store_crud() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path().join("test_rocks")).unwrap();
        store_and_retrieve(&store).await;
    }

    #[tokio::test]
    async fn rocks_store_search() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path().join("test_rocks")).unwrap();
        search_works(&store).await;
    }

    #[tokio::test]
    async fn rocks_store_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path().join("test_rocks")).unwrap();
        delete_works(&store).await;
    }

    #[tokio::test]
    async fn update_nonexistent_returns_error() {
        let store = InMemoryStore::new();
        let mem = Memory::new(MemoryKind::Episodic, json!("ghost"));
        let result = store.update(&mem).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn metrics_accurate() {
        let store = InMemoryStore::new();
        let m1 = Memory::new(MemoryKind::Semantic, json!("a"))
            .with_tier(MemoryTier::Hot);
        let m2 = Memory::new(MemoryKind::Episodic, json!("b"))
            .with_tier(MemoryTier::Cold);

        store.store(&m1).await.unwrap();
        store.store(&m2).await.unwrap();

        let metrics = store.metrics().await.unwrap();
        assert_eq!(metrics.total_memories, 2);
        assert_eq!(*metrics.by_tier.get("hot").unwrap_or(&0), 1);
        assert_eq!(*metrics.by_tier.get("cold").unwrap_or(&0), 1);
    }
}
