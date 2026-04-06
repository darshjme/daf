//! # DAF Memory
//!
//! Episode-based memory system for the Darshj's Agent Framework.
//!
//! This crate gives agents persistent memory across sessions — they learn from
//! past interactions, store knowledge, and recall relevant context. Inspired by
//! human episodic memory and the Mohini 3-tier memory architecture.
//!
//! ## Architecture
//!
//! The memory system is organized into three tiers:
//!
//! | Tier | Latency  | Purpose                          | Backend      |
//! |------|----------|----------------------------------|--------------|
//! | Hot  | < 1 ms   | Active context, instant recall   | In-memory/Sled |
//! | Warm | < 10 ms  | Recent memories, session data    | Sled         |
//! | Cold | < 100 ms | Archival, knowledge base         | RocksDB      |
//!
//! Memories flow through four kinds:
//! - **Episodic** — records of what happened (events, conversations, decisions)
//! - **Semantic** — distilled facts and knowledge
//! - **Procedural** — how-to knowledge and workflows
//! - **Working** — current context, bounded by capacity
//!
//! ## Key Components
//!
//! - [`MemoryManager`](manager::MemoryManager) — orchestrates tier assignment,
//!   promotion/demotion, TTL expiry, and periodic compaction.
//! - [`MemoryStore`](store::MemoryStore) — trait for storage backends, with
//!   [`InMemoryStore`](store::InMemoryStore), [`SledStore`](store::SledStore),
//!   and [`RocksStore`](store::RocksStore) implementations.
//! - [`Episode`](episode::Episode) — captures sequences of agent actions
//!   as coherent narratives for later analysis.
//! - [`RecallQuery`](recall::RecallQuery) — builder for searching memories
//!   with ranking by recency, frequency, or importance.
//! - [`Consolidator`](consolidation::Consolidator) — periodic process that
//!   distills episodic memories into semantic ones, merges duplicates, and
//!   reinforces frequently-used knowledge.
//! - [`WorkingMemory`](context::WorkingMemory) — bounded LRU context window
//!   for agent decision-making.
//!
//! ## Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use daf_memory::{
//!     store::InMemoryStore,
//!     manager::MemoryManager,
//!     types::{Memory, MemoryKind},
//!     recall::{RecallQuery, RecallStrategy},
//!     context::ContextManager,
//! };
//! use serde_json::json;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a memory manager with an in-memory store.
//! let store = Arc::new(InMemoryStore::new());
//! let manager = MemoryManager::new(store.clone());
//!
//! // Store a memory.
//! let mem = Memory::new(MemoryKind::Semantic, json!({"fact": "Rust is fast"}))
//!     .with_importance(0.9)
//!     .with_tags(vec!["lang".into(), "perf".into()]);
//! let id = manager.store_memory(mem).await?;
//!
//! // Recall relevant memories.
//! let query = RecallQuery::new()
//!     .by_kind(MemoryKind::Semantic)
//!     .by_content_search("rust")
//!     .strategy(RecallStrategy::MostRelevant)
//!     .limit(10);
//! let result = manager.recall(&query).await?;
//!
//! // Use working memory for context management.
//! let ctx = ContextManager::new(store, 64);
//! ctx.load("rust performance", &[], 20).await?;
//! ctx.observe(json!("compilation takes 2 minutes"), vec!["perf".into()]);
//! ctx.save().await?;
//! # Ok(())
//! # }
//! ```

pub mod consolidation;
pub mod context;
pub mod episode;
pub mod error;
pub mod manager;
pub mod recall;
pub mod store;
pub mod types;

// Re-export the most commonly used items for ergonomic imports.
pub use error::{MemoryError, MemoryResult};
pub use types::{Memory, MemoryId, MemoryKind, MemoryMetrics, MemoryTier};
