//! Error types for the memory subsystem.

use crate::types::MemoryId;

/// Memory-specific error type.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// A memory with the given ID was not found.
    #[error("memory not found: {0}")]
    NotFound(MemoryId),

    /// Storage backend error.
    #[error("store error: {0}")]
    StoreError(String),

    /// Serialization or deserialization failure.
    #[error("serialization error: {0}")]
    SerializationError(String),

    /// The operation is invalid in the current state.
    #[error("invalid operation: {0}")]
    InvalidOperation(String),

    /// Catch-all for unexpected internal failures.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Shorthand result type for memory operations.
pub type MemoryResult<T> = Result<T, MemoryError>;

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<serde_json::Error> for MemoryError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

impl From<std::io::Error> for MemoryError {
    fn from(err: std::io::Error) -> Self {
        Self::Internal(format!("I/O error: {err}"))
    }
}

impl From<MemoryError> for daf_core::DafError {
    fn from(err: MemoryError) -> Self {
        match err {
            MemoryError::NotFound(id) => daf_core::DafError::NotFound {
                entity: "memory".into(),
                id: id.to_string(),
            },
            MemoryError::StoreError(msg) => daf_core::DafError::Internal(msg),
            MemoryError::SerializationError(msg) => daf_core::DafError::SerializationError(msg),
            MemoryError::InvalidOperation(msg) => daf_core::DafError::Internal(msg),
            MemoryError::Internal(msg) => daf_core::DafError::Internal(msg),
        }
    }
}
