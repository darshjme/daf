//! Error types for the logger crate.

/// Errors produced by logger operations.
#[derive(Debug, thiserror::Error)]
pub enum LoggerError {
    /// I/O error (file system, network).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization / deserialization failure.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Backend storage error (RocksDB, etc.).
    #[error("storage error: {0}")]
    Storage(String),

    /// Attempted to write to a closed writer.
    #[error("writer is closed")]
    WriterClosed,

    /// Query error.
    #[error("query error: {0}")]
    Query(String),

    /// Retention / archival error.
    #[error("retention error: {0}")]
    Retention(String),

    /// Catch-all.
    #[error("{0}")]
    Other(String),
}

impl From<LoggerError> for daf_core::DafError {
    fn from(e: LoggerError) -> Self {
        daf_core::DafError::Internal(e.to_string())
    }
}
