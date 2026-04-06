//! # DAF Vault
//!
//! Embedded secrets management for the Darshj's Agent Framework.
//!
//! Provides encrypted storage, envelope encryption with key rotation,
//! credential distribution to agents, and a complete audit trail. Think
//! HashiCorp Vault, but compiled into your agent runtime — no sidecar,
//! no network hop.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────┐     ┌──────────┐     ┌─────────────┐
//!  │ SecretProvider│────▶│ VaultStore│────▶│  Sled / Mem  │
//!  └──────────────┘     └──────────┘     └─────────────┘
//!         │                   │
//!         ▼                   ▼
//!  ┌──────────────┐     ┌──────────┐
//!  │  ChainProvider│     │  KeyRing  │
//!  └──────────────┘     └──────────┘
//!         │                   │
//!         ▼                   ▼
//!  ┌──────────────┐     ┌──────────────┐
//!  │  EnvProvider  │     │ RotationMgr  │
//!  └──────────────┘     └──────────────┘
//!                             │
//!                             ▼
//!                       ┌──────────┐
//!                       │ AuditLog  │
//!                       └──────────┘
//! ```
//!
//! # Security model
//!
//! - **Envelope encryption**: each secret gets its own AES-256-GCM data key;
//!   data keys are encrypted by the master key derived via PBKDF2.
//! - **Seal/unseal**: the vault starts sealed. An operator supplies a
//!   passphrase to derive the master key and unseal.
//! - **Key material hygiene**: sensitive buffers are zeroized on drop.
//! - **Append-only audit**: every read, write, delete, rotate, seal, and
//!   unseal event is recorded in a tamper-evident log.

pub mod audit;
pub mod keyring;
pub mod provider;
pub mod rotation;
pub mod secret;
pub mod store;

// Re-exports for ergonomic use from dependent crates.
pub use audit::{AuditAction, AuditEntry, AuditLog};
pub use keyring::{DataKey, KeyId, KeyRing, MasterKey};
pub use provider::{ChainProvider, EnvProvider, SecretProvider, VaultProvider};
pub use rotation::{RotationManager, RotationPolicy};
pub use secret::{AccessPolicy, Secret, SecretId, SecretKind, SecretRef};
pub use store::{InMemoryVaultStore, SledVaultStore, VaultStore};

/// Crate-level error type.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault is sealed — unseal before performing operations")]
    Sealed,

    #[error("secret not found: {0}")]
    SecretNotFound(String),

    #[error("access denied: {0}")]
    AccessDenied(String),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("key not found: {0}")]
    KeyNotFound(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("rotation error: {0}")]
    Rotation(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("invalid passphrase")]
    InvalidPassphrase,

    #[error("{0}")]
    Internal(String),
}

impl From<serde_json::Error> for VaultError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

impl From<sled::Error> for VaultError {
    fn from(e: sled::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

impl From<ring::error::Unspecified> for VaultError {
    fn from(_: ring::error::Unspecified) -> Self {
        Self::Crypto("unspecified ring error".into())
    }
}

/// Crate-level result alias.
pub type VaultResult<T> = Result<T, VaultError>;
