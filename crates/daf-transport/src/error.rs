//! Transport-specific error types.
//!
//! These errors cover the full range of failures that can occur at the
//! network transport layer: connection lifecycle, TLS handshake, pool
//! exhaustion, address resolution, and raw I/O.

/// Transport-layer error covering all failure modes for TCP, Unix, TLS,
/// and in-process transports.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The remote peer actively refused the connection.
    #[error("connection refused: {address}")]
    ConnectionRefused {
        /// The address that refused the connection.
        address: String,
    },

    /// The connection was reset by the remote peer.
    #[error("connection reset: {address}")]
    ConnectionReset {
        /// The address of the peer that reset.
        address: String,
    },

    /// An operation exceeded its deadline.
    #[error("timeout after {duration:?}: {operation}")]
    Timeout {
        /// The operation that timed out.
        operation: String,
        /// How long we waited.
        duration: std::time::Duration,
    },

    /// TLS handshake or certificate validation failure.
    #[error("tls error: {message}")]
    TlsError {
        /// Human-readable TLS failure description.
        message: String,
    },

    /// The requested bind address is already in use.
    #[error("address in use: {address}")]
    AddressInUse {
        /// The address that was already bound.
        address: String,
    },

    /// The connection pool has no available connections.
    #[error("pool exhausted: {pool_size} connections in use")]
    PoolExhausted {
        /// Current pool capacity.
        pool_size: usize,
    },

    /// The provided address could not be parsed or resolved.
    #[error("invalid address: {address}: {reason}")]
    InvalidAddress {
        /// The offending address string.
        address: String,
        /// Why it is invalid.
        reason: String,
    },

    /// A raw I/O error from the operating system.
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),

    /// The connection is closed and cannot be used.
    #[error("connection closed")]
    ConnectionClosed,

    /// Channel send/receive failure for in-process transport.
    #[error("channel error: {0}")]
    ChannelError(String),
}

/// Shorthand result type for transport operations.
pub type TransportResult<T> = Result<T, TransportError>;

impl TransportError {
    /// Returns `true` if the error is likely transient and a retry may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ConnectionRefused { .. }
                | Self::ConnectionReset { .. }
                | Self::Timeout { .. }
                | Self::PoolExhausted { .. }
        )
    }

    /// Returns `true` if this error indicates the connection is no longer usable.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::TlsError { .. }
                | Self::InvalidAddress { .. }
                | Self::ConnectionClosed
        )
    }
}

impl From<TransportError> for daf_core::error::DafError {
    fn from(err: TransportError) -> Self {
        daf_core::error::DafError::TransportError {
            endpoint: None,
            message: err.to_string(),
            retryable: err.is_retryable(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(TransportError::ConnectionRefused {
            address: "127.0.0.1:8080".into()
        }
        .is_retryable());

        assert!(TransportError::Timeout {
            operation: "connect".into(),
            duration: std::time::Duration::from_secs(5),
        }
        .is_retryable());

        assert!(!TransportError::TlsError {
            message: "bad cert".into()
        }
        .is_retryable());

        assert!(!TransportError::ConnectionClosed.is_retryable());
    }

    #[test]
    fn fatal_classification() {
        assert!(TransportError::TlsError {
            message: "expired".into()
        }
        .is_fatal());

        assert!(TransportError::ConnectionClosed.is_fatal());

        assert!(!TransportError::ConnectionRefused {
            address: "localhost".into()
        }
        .is_fatal());
    }

    #[test]
    fn display_messages() {
        let err = TransportError::PoolExhausted { pool_size: 64 };
        assert!(err.to_string().contains("64"));

        let err = TransportError::InvalidAddress {
            address: "bad://addr".into(),
            reason: "unsupported scheme".into(),
        };
        assert!(err.to_string().contains("unsupported scheme"));
    }

    #[test]
    fn converts_to_daf_error() {
        let terr = TransportError::ConnectionRefused {
            address: "10.0.0.1:443".into(),
        };
        let derr: daf_core::error::DafError = terr.into();
        assert!(matches!(derr, daf_core::error::DafError::TransportError { retryable: true, .. }));
    }

    #[test]
    fn io_error_conversion() {
        let io = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let terr: TransportError = io.into();
        assert!(matches!(terr, TransportError::IoError(_)));
        assert!(terr.to_string().contains("pipe broke"));
    }
}
