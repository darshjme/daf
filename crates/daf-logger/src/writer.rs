//! Log writer backends.
//!
//! Every [`LogEntry`] must ultimately land somewhere persistent. The [`LogWriter`]
//! trait abstracts the storage layer so the rest of the crate never cares
//! whether logs go to files, RocksDB, or an in-memory ring buffer.
//!
//! Included backends:
//! - [`FileWriter`]   — append-only NDJSON files with size/time rotation.
//! - [`RocksWriter`]  — structured key-value storage in RocksDB.
//! - [`MemoryWriter`] — bounded ring buffer for testing and debugging.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};
use uuid::Uuid;

use crate::entry::LogEntry;
use crate::error::LoggerError;

// ---------------------------------------------------------------------------
// LogWriter trait
// ---------------------------------------------------------------------------

/// Async backend for persisting log entries.
#[async_trait]
pub trait LogWriter: Send + Sync + 'static {
    /// Write a single entry.
    async fn write_entry(&self, entry: &LogEntry) -> Result<(), LoggerError>;

    /// Write a batch of entries (default: sequential writes).
    async fn write_batch(&self, entries: &[LogEntry]) -> Result<(), LoggerError> {
        for entry in entries {
            self.write_entry(entry).await?;
        }
        Ok(())
    }

    /// Flush any internal buffers to durable storage.
    async fn flush(&self) -> Result<(), LoggerError>;

    /// Rotate the underlying storage (e.g. roll to a new file).
    async fn rotate(&self) -> Result<(), LoggerError>;

    /// Gracefully close the writer, flushing remaining data.
    async fn close(&self) -> Result<(), LoggerError>;
}

// ---------------------------------------------------------------------------
// RotationPolicy
// ---------------------------------------------------------------------------

/// When a log file should be rotated.
#[derive(Debug, Clone)]
pub struct RotationPolicy {
    /// Rotate when the current file exceeds this many bytes.
    pub max_size_bytes: u64,
    /// Rotate after this duration since the file was opened.
    pub max_age: chrono::Duration,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_size_bytes: 100 * 1024 * 1024, // 100 MiB
            max_age: chrono::Duration::hours(24),
        }
    }
}

// ---------------------------------------------------------------------------
// FileWriter
// ---------------------------------------------------------------------------

/// State shared behind a mutex for the active file.
struct FileState {
    current_path: PathBuf,
    current_size: u64,
    opened_at: DateTime<Utc>,
    closed: bool,
}

/// Append-only NDJSON log writer with rotation support.
///
/// Each line is a self-contained JSON object — trivial to parse with `jq`,
/// stream with `tail -f`, or ingest into any log aggregator.
pub struct FileWriter {
    dir: PathBuf,
    prefix: String,
    policy: RotationPolicy,
    state: Arc<Mutex<FileState>>,
}

impl FileWriter {
    /// Create a new `FileWriter` that writes into `dir` with the given prefix.
    pub async fn new(
        dir: impl AsRef<Path>,
        prefix: impl Into<String>,
        policy: RotationPolicy,
    ) -> Result<Self, LoggerError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).await.map_err(LoggerError::Io)?;

        let prefix = prefix.into();
        let path = Self::make_path(&dir, &prefix);

        let state = Arc::new(Mutex::new(FileState {
            current_path: path,
            current_size: 0,
            opened_at: Utc::now(),
            closed: false,
        }));

        info!(directory = %dir.display(), "FileWriter initialized");

        Ok(Self {
            dir,
            prefix,
            policy,
            state,
        })
    }

    fn make_path(dir: &Path, prefix: &str) -> PathBuf {
        let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
        dir.join(format!("{prefix}_{ts}.ndjson"))
    }

    /// Check whether the current file needs rotation.
    fn needs_rotation(&self, state: &FileState) -> bool {
        if state.closed {
            return false;
        }
        state.current_size >= self.policy.max_size_bytes
            || Utc::now() - state.opened_at >= self.policy.max_age
    }
}

#[async_trait]
impl LogWriter for FileWriter {
    async fn write_entry(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        let mut line = serde_json::to_string(entry).map_err(LoggerError::Serde)?;
        line.push('\n');
        let line_len = line.len() as u64;

        let path = {
            let mut state = self.state.lock();
            if state.closed {
                return Err(LoggerError::WriterClosed);
            }
            if self.needs_rotation(&state) {
                let new_path = Self::make_path(&self.dir, &self.prefix);
                debug!(old = %state.current_path.display(), new = %new_path.display(), "rotating log file");
                state.current_path = new_path;
                state.current_size = 0;
                state.opened_at = Utc::now();
            }
            state.current_size += line_len;
            state.current_path.clone()
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(LoggerError::Io)?;

        file.write_all(line.as_bytes())
            .await
            .map_err(LoggerError::Io)?;

        Ok(())
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        // Append mode — each write is flushed at the OS level.
        Ok(())
    }

    async fn rotate(&self) -> Result<(), LoggerError> {
        let mut state = self.state.lock();
        let new_path = Self::make_path(&self.dir, &self.prefix);
        info!(new = %new_path.display(), "manual log rotation");
        state.current_path = new_path;
        state.current_size = 0;
        state.opened_at = Utc::now();
        Ok(())
    }

    async fn close(&self) -> Result<(), LoggerError> {
        let mut state = self.state.lock();
        state.closed = true;
        info!("FileWriter closed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RocksWriter
// ---------------------------------------------------------------------------

/// Persistent structured storage backed by RocksDB.
///
/// Key layout: `{conversation_id}/{turn_number:010}/{entry_id}`
/// This gives us ordered iteration per conversation for free.
pub struct RocksWriter {
    db: Arc<rocksdb::DB>,
    closed: Arc<Mutex<bool>>,
}

impl RocksWriter {
    /// Open or create a RocksDB instance at the given path.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, LoggerError> {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.set_compression_type(rocksdb::DBCompressionType::Zstd);

        let db = rocksdb::DB::open(&opts, path.as_ref()).map_err(|e| {
            LoggerError::Storage(format!("RocksDB open failed: {e}"))
        })?;

        info!(path = %path.as_ref().display(), "RocksWriter initialized");

        Ok(Self {
            db: Arc::new(db),
            closed: Arc::new(Mutex::new(false)),
        })
    }

    /// Build the key for a log entry.
    fn entry_key(entry: &LogEntry) -> String {
        format!(
            "{}/{:010}/{}",
            entry.conversation_id, entry.turn_number, entry.id
        )
    }

    /// Read all entries for a conversation, ordered by turn number.
    pub fn read_conversation(&self, conversation_id: &Uuid) -> Result<Vec<LogEntry>, LoggerError> {
        let prefix = format!("{conversation_id}/");
        let iter = self.db.prefix_iterator(prefix.as_bytes());
        let mut entries = Vec::new();

        for item in iter {
            let (key, value) = item.map_err(|e| LoggerError::Storage(e.to_string()))?;
            let key_str = String::from_utf8_lossy(&key);
            if !key_str.starts_with(&prefix) {
                break;
            }
            let entry: LogEntry =
                serde_json::from_slice(&value).map_err(LoggerError::Serde)?;
            entries.push(entry);
        }

        Ok(entries)
    }
}

#[async_trait]
impl LogWriter for RocksWriter {
    async fn write_entry(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        if *self.closed.lock() {
            return Err(LoggerError::WriterClosed);
        }

        let key = Self::entry_key(entry);
        let value = serde_json::to_vec(entry).map_err(LoggerError::Serde)?;

        self.db
            .put(key.as_bytes(), &value)
            .map_err(|e| LoggerError::Storage(e.to_string()))?;

        Ok(())
    }

    async fn write_batch(&self, entries: &[LogEntry]) -> Result<(), LoggerError> {
        if *self.closed.lock() {
            return Err(LoggerError::WriterClosed);
        }

        let mut batch = rocksdb::WriteBatch::default();
        for entry in entries {
            let key = Self::entry_key(entry);
            let value = serde_json::to_vec(entry).map_err(LoggerError::Serde)?;
            batch.put(key.as_bytes(), &value);
        }

        self.db
            .write(batch)
            .map_err(|e| LoggerError::Storage(e.to_string()))?;

        Ok(())
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        self.db
            .flush()
            .map_err(|e| LoggerError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn rotate(&self) -> Result<(), LoggerError> {
        // RocksDB manages its own compaction — nothing to do.
        Ok(())
    }

    async fn close(&self) -> Result<(), LoggerError> {
        let mut closed = self.closed.lock();
        *closed = true;
        // Flush before marking closed.
        self.db
            .flush()
            .map_err(|e| LoggerError::Storage(e.to_string()))?;
        info!("RocksWriter closed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MemoryWriter
// ---------------------------------------------------------------------------

/// In-memory ring buffer writer for testing and debugging.
///
/// Once the buffer reaches `capacity`, the oldest entries are evicted.
pub struct MemoryWriter {
    buffer: Arc<Mutex<Vec<LogEntry>>>,
    capacity: usize,
    closed: Arc<Mutex<bool>>,
}

impl MemoryWriter {
    /// Create a ring buffer with the given maximum capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::with_capacity(capacity.min(4096)))),
            capacity,
            closed: Arc::new(Mutex::new(false)),
        }
    }

    /// Snapshot of all entries currently in the buffer.
    pub fn entries(&self) -> Vec<LogEntry> {
        self.buffer.lock().clone()
    }

    /// Number of entries in the buffer.
    pub fn len(&self) -> usize {
        self.buffer.lock().len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.lock().is_empty()
    }

    /// Clear the buffer.
    pub fn clear(&self) {
        self.buffer.lock().clear();
    }
}

#[async_trait]
impl LogWriter for MemoryWriter {
    async fn write_entry(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        if *self.closed.lock() {
            return Err(LoggerError::WriterClosed);
        }

        let mut buf = self.buffer.lock();
        if buf.len() >= self.capacity {
            buf.remove(0);
        }
        buf.push(entry.clone());
        Ok(())
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        Ok(())
    }

    async fn rotate(&self) -> Result<(), LoggerError> {
        self.buffer.lock().clear();
        debug!("MemoryWriter rotated (buffer cleared)");
        Ok(())
    }

    async fn close(&self) -> Result<(), LoggerError> {
        *self.closed.lock() = true;
        info!("MemoryWriter closed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{ContentType, LogLevel};
    use daf_core::AgentId;

    fn make_entry(turn: u64) -> LogEntry {
        LogEntry::new(
            AgentId::new(),
            Some(AgentId::new()),
            Uuid::now_v7(),
            turn,
            ContentType::Text,
            serde_json::Value::String(format!("turn {turn}")),
            LogLevel::Info,
        )
    }

    #[tokio::test]
    async fn memory_writer_ring_buffer() {
        let w = MemoryWriter::new(3);
        for i in 0..5 {
            w.write_entry(&make_entry(i)).await.unwrap();
        }
        let entries = w.entries();
        assert_eq!(entries.len(), 3);
        // Oldest entries (0, 1) should have been evicted.
        assert_eq!(entries[0].turn_number, 2);
        assert_eq!(entries[2].turn_number, 4);
    }

    #[tokio::test]
    async fn memory_writer_closed_rejects() {
        let w = MemoryWriter::new(10);
        w.close().await.unwrap();
        let result = w.write_entry(&make_entry(0)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_writer_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let w = FileWriter::new(tmp.path(), "test", RotationPolicy::default())
            .await
            .unwrap();

        let entry = make_entry(1);
        w.write_entry(&entry).await.unwrap();
        w.close().await.unwrap();

        // Verify at least one .ndjson file was created.
        let mut found = false;
        let mut rd = fs::read_dir(tmp.path()).await.unwrap();
        while let Some(de) = rd.next_entry().await.unwrap() {
            if de.file_name().to_string_lossy().ends_with(".ndjson") {
                found = true;
                let content = fs::read_to_string(de.path()).await.unwrap();
                let parsed: LogEntry = serde_json::from_str(content.trim()).unwrap();
                assert_eq!(parsed.id, entry.id);
            }
        }
        assert!(found, "expected an .ndjson file");
    }

    #[tokio::test]
    async fn rocks_writer_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let w = RocksWriter::new(tmp.path()).unwrap();

        let conv_id = Uuid::now_v7();
        let src = AgentId::new();
        let tgt = AgentId::new();

        let e1 = LogEntry::text(src, tgt, conv_id, 0, "first");
        let e2 = LogEntry::text(tgt, src, conv_id, 1, "second");

        w.write_batch(&[e1.clone(), e2.clone()]).await.unwrap();

        let read_back = w.read_conversation(&conv_id).unwrap();
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].turn_number, 0);
        assert_eq!(read_back[1].turn_number, 1);
    }
}
