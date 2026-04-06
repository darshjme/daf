//! # DAF Logger
//!
//! Structured conversation logging, KB extraction, and episode memory for
//! the Darshj's Agent Framework (DAF).
//!
//! This crate is the system's black box recorder. Every agent-to-agent message,
//! tool invocation, and decision point flows through here so that:
//!
//! 1. **Humans can review** what agents discussed and decided.
//! 2. **Knowledge is extracted** — decisions, errors, patterns — and stored in
//!    the KB for future reference.
//! 3. **Episodic memory** is built — bounded, self-contained chunks of activity
//!    that agents can recall ("have I seen this before?").
//!
//! ## Architecture
//!
//! ```text
//! LogEntry ──→ LogSink pipeline ──→ FileWriter / RocksWriter
//!                   │
//!                   ├──→ KBExtractor ──→ KnowledgeEntry[]
//!                   └──→ EpisodeRecorder ──→ Episode[]
//! ```
//!
//! ## Quick start
//!
//! ```rust,ignore
//! use daf_logger::{LogEntry, MemoryWriter, LogWriter};
//! use daf_core::AgentId;
//! use uuid::Uuid;
//!
//! let writer = MemoryWriter::new(1000);
//! let entry = LogEntry::text(
//!     AgentId::new(),
//!     AgentId::new(),
//!     Uuid::now_v7(),
//!     0,
//!     "Hello from agent A",
//! );
//! writer.write_entry(&entry).await.unwrap();
//! ```

pub mod entry;
pub mod episode;
pub mod error;
pub mod extractor;
pub mod query;
pub mod retention;
pub mod sink;
pub mod writer;

// Re-export primary types at crate root for ergonomic imports.
pub use entry::{ContentType, ConversationLog, LogEntry, LogLevel};
pub use episode::{Episode, EpisodeEvent, EpisodeEventType, EpisodeRecorder};
pub use error::LoggerError;
pub use extractor::{ExtractionRule, KBExtractor, KnowledgeCategory, KnowledgeEntry};
pub use query::{
    count_by_agent, count_by_level, conversation_duration_stats, DurationStats, LogQuery,
    QueryResult,
};
pub use retention::{RetentionManager, RetentionPolicy, RetentionStats};
pub use sink::{BufferedSink, CollectorSink, FanOutSink, FilterSink, LogSink, TransformSink};
pub use writer::{FileWriter, LogWriter, MemoryWriter, RocksWriter, RotationPolicy};
