//! Comprehensive error types for the DAF framework.
//!
//! Every crate in the DAF workspace depends on these error types. They cover
//! the full surface area of failures that can occur across agent lifecycle,
//! transport, task execution, resource management, and serialization.

use std::fmt;

/// Unified error type for all DAF operations.
///
/// Each variant carries enough context to produce actionable diagnostics
/// without leaking internal implementation details across crate boundaries.
#[derive(Debug, thiserror::Error)]
pub enum DafError {
    /// An error originating from agent lifecycle operations (spawn, shutdown, health).
    #[error("agent error: {message}")]
    AgentError {
        /// Identifier of the agent that faulted, if known.
        agent_id: Option<uuid::Uuid>,
        /// Human-readable description of the failure.
        message: String,
    },

    /// Transport-layer failure (connection drop, TLS handshake, DNS).
    #[error("transport error: {message}")]
    TransportError {
        /// The address or endpoint involved, when available.
        endpoint: Option<String>,
        /// Human-readable description.
        message: String,
        /// Whether a retry is likely to succeed.
        retryable: bool,
    },

    /// Protocol violation — malformed frames, unexpected message kinds.
    #[error("protocol error: {message}")]
    ProtocolError {
        /// Human-readable description.
        message: String,
    },

    /// Task execution failure.
    #[error("task error [{task_id}]: {message}")]
    TaskError {
        /// The task that failed.
        task_id: uuid::Uuid,
        /// Human-readable description.
        message: String,
    },

    /// A deadline was exceeded.
    #[error("timeout after {duration:?}: {operation}")]
    TimeoutError {
        /// The operation that timed out.
        operation: String,
        /// How long we waited before giving up.
        duration: std::time::Duration,
    },

    /// A resource pool is fully consumed.
    #[error("resource exhausted: {resource}")]
    ResourceExhausted {
        /// Which resource kind ran out.
        resource: String,
    },

    /// The caller lacks permission for the requested operation.
    #[error("unauthorized: {message}")]
    Unauthorized {
        /// Human-readable description.
        message: String,
    },

    /// A requested entity (agent, task, message) does not exist.
    #[error("not found: {entity} with id {id}")]
    NotFound {
        /// The kind of entity (e.g. "agent", "task").
        entity: String,
        /// The identifier that was looked up.
        id: String,
    },

    /// Catch-all for unexpected internal failures.
    #[error("internal error: {0}")]
    Internal(String),

    /// Serialization or deserialization failure.
    #[error("serialization error: {0}")]
    SerializationError(String),

    /// Configuration is invalid or missing required fields.
    #[error("config error: {0}")]
    ConfigError(String),

    /// Cryptographic operation failed (signing, verification, key generation).
    #[error("crypto error: {0}")]
    CryptoError(String),
}

impl DafError {
    /// Returns `true` if this error is retryable at the transport level.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::TransportError { retryable, .. } => *retryable,
            Self::TimeoutError { .. } => true,
            Self::ResourceExhausted { .. } => true,
            _ => false,
        }
    }

    /// Returns `true` if this error indicates the entity was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    /// Convenience constructor for agent errors.
    pub fn agent(agent_id: impl Into<Option<uuid::Uuid>>, msg: impl Into<String>) -> Self {
        Self::AgentError {
            agent_id: agent_id.into(),
            message: msg.into(),
        }
    }

    /// Convenience constructor for task errors.
    pub fn task(task_id: uuid::Uuid, msg: impl Into<String>) -> Self {
        Self::TaskError {
            task_id,
            message: msg.into(),
        }
    }

    /// Convenience constructor for transport errors.
    pub fn transport(msg: impl Into<String>, retryable: bool) -> Self {
        Self::TransportError {
            endpoint: None,
            message: msg.into(),
            retryable,
        }
    }
}

/// Shorthand result type used throughout the DAF framework.
pub type DafResult<T> = Result<T, DafError>;

// ---------------------------------------------------------------------------
// From impls for common error types
// ---------------------------------------------------------------------------

impl From<serde_json::Error> for DafError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

impl From<std::io::Error> for DafError {
    fn from(err: std::io::Error) -> Self {
        Self::Internal(format!("I/O error: {err}"))
    }
}

impl From<uuid::Error> for DafError {
    fn from(err: uuid::Error) -> Self {
        Self::SerializationError(format!("UUID parse error: {err}"))
    }
}

impl From<std::string::FromUtf8Error> for DafError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Self::SerializationError(format!("UTF-8 error: {err}"))
    }
}

impl From<anyhow::Error> for DafError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(format!("{err:#}"))
    }
}

// ---------------------------------------------------------------------------
// ErrorContext extension trait
// ---------------------------------------------------------------------------

/// Extension trait that adds `.context(msg)` to `DafResult` values,
/// similar to `anyhow::Context` but keeping the typed error.
pub trait ErrorContext<T> {
    /// Wrap the error in [`DafError::Internal`] with additional context.
    fn context(self, msg: impl fmt::Display) -> DafResult<T>;
}

impl<T> ErrorContext<T> for DafResult<T> {
    fn context(self, msg: impl fmt::Display) -> DafResult<T> {
        self.map_err(|e| DafError::Internal(format!("{msg}: {e}")))
    }
}

impl<T> ErrorContext<T> for Result<T, std::io::Error> {
    fn context(self, msg: impl fmt::Display) -> DafResult<T> {
        self.map_err(|e| DafError::Internal(format!("{msg}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_formats_are_human_readable() {
        let err = DafError::agent(None, "spawn failed");
        assert_eq!(err.to_string(), "agent error: spawn failed");

        let id = uuid::Uuid::nil();
        let err = DafError::task(id, "out of memory");
        assert!(err.to_string().contains("out of memory"));
    }

    #[test]
    fn retryable_classification() {
        assert!(DafError::transport("conn reset", true).is_retryable());
        assert!(!DafError::transport("bad cert", false).is_retryable());
        assert!(DafError::TimeoutError {
            operation: "rpc".into(),
            duration: std::time::Duration::from_secs(5),
        }
        .is_retryable());
        assert!(!DafError::Internal("bug".into()).is_retryable());
    }

    #[test]
    fn from_serde_json_error() {
        let raw = serde_json::from_str::<serde_json::Value>("not json");
        let err: DafError = raw.unwrap_err().into();
        assert!(matches!(err, DafError::SerializationError(_)));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: DafError = io_err.into();
        assert!(matches!(err, DafError::Internal(_)));
    }

    #[test]
    fn context_extension() {
        let result: DafResult<()> = Err(DafError::Internal("root".into()));
        let wrapped = result.context("during init");
        assert!(wrapped.unwrap_err().to_string().contains("during init"));
    }

    #[test]
    fn not_found_predicate() {
        let err = DafError::NotFound {
            entity: "agent".into(),
            id: "abc".into(),
        };
        assert!(err.is_not_found());
        assert!(!DafError::Internal("x".into()).is_not_found());
    }
}
