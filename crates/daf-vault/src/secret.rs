//! Secret types and access policies.
//!
//! Defines the core data model for secrets stored in the vault: what kinds
//! of secrets exist, who may access them, and how they are represented both
//! in-memory and at rest.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// SecretId
// ---------------------------------------------------------------------------

/// Time-ordered identifier for a secret, backed by UUID v7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct SecretId(Uuid);

impl SecretId {
    /// Generate a new time-ordered secret identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for SecretId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SecretId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secret:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// SecretKind
// ---------------------------------------------------------------------------

/// Classification of the secret's content type.
///
/// Used for policy matching, rotation strategy selection, and UI display.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// Third-party API key (e.g. OpenAI, Stripe).
    ApiKey,
    /// Bearer or session token.
    Token,
    /// X.509 or other certificate material.
    Certificate,
    /// Username/password credential.
    Password,
    /// Asymmetric private key (RSA, Ed25519, ECDSA).
    PrivateKey,
    /// Application-defined kind with a freeform tag.
    Custom(String),
}

impl fmt::Display for SecretKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey => write!(f, "api_key"),
            Self::Token => write!(f, "token"),
            Self::Certificate => write!(f, "certificate"),
            Self::Password => write!(f, "password"),
            Self::PrivateKey => write!(f, "private_key"),
            Self::Custom(s) => write!(f, "custom:{s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentKind (local mirror for access policy matching)
// ---------------------------------------------------------------------------

/// Agent role classification — mirrors the canonical daf-core definition
/// for use in access policy matching without requiring a circular dependency
/// on runtime types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Orchestrator,
    Specialist,
    Worker,
    Monitor,
    Router,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Orchestrator => write!(f, "orchestrator"),
            Self::Specialist => write!(f, "specialist"),
            Self::Worker => write!(f, "worker"),
            Self::Monitor => write!(f, "monitor"),
            Self::Router => write!(f, "router"),
        }
    }
}

// ---------------------------------------------------------------------------
// AccessPolicy
// ---------------------------------------------------------------------------

/// Defines which agents may access a secret.
///
/// Evaluated at read time by the vault store. The `AnyAgent` policy is
/// intentionally permissive — use it only for non-sensitive configuration
/// values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessPolicy {
    /// Any authenticated agent may read this secret.
    AnyAgent,
    /// Only the listed agents may read this secret.
    SpecificAgents(Vec<AgentId>),
    /// Any agent advertising the named capability may read this secret.
    ByCapability(String),
    /// Any agent of the given kind may read this secret.
    ByKind(AgentKind),
}

impl Default for AccessPolicy {
    fn default() -> Self {
        Self::AnyAgent
    }
}

impl AccessPolicy {
    /// Check whether the given agent identity satisfies this policy.
    ///
    /// For `ByCapability` and `ByKind` policies, the caller must supply
    /// the agent's capabilities and kind through the optional parameters.
    /// If those are `None`, the check conservatively denies access.
    pub fn allows(
        &self,
        agent_id: &AgentId,
        agent_kind: Option<&AgentKind>,
        agent_capabilities: Option<&[String]>,
    ) -> bool {
        match self {
            Self::AnyAgent => true,
            Self::SpecificAgents(ids) => ids.contains(agent_id),
            Self::ByCapability(cap) => agent_capabilities
                .map(|caps| caps.contains(cap))
                .unwrap_or(false),
            Self::ByKind(kind) => agent_kind.map(|k| k == kind).unwrap_or(false),
        }
    }
}

// ---------------------------------------------------------------------------
// Secret
// ---------------------------------------------------------------------------

/// A secret stored in the vault.
///
/// The `encrypted_value` field holds the ciphertext produced by the data key
/// associated with this secret. The plaintext is never held in this struct
/// beyond the initial encryption call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secret {
    /// Unique identifier.
    pub id: SecretId,
    /// What kind of secret this is.
    pub kind: SecretKind,
    /// Human-readable name used for lookup (e.g. `"openai_api_key"`).
    pub name: String,
    /// AES-256-GCM ciphertext of the secret value.
    pub encrypted_value: Vec<u8>,
    /// Application-defined metadata (labels, tags, source, etc.).
    pub metadata: HashMap<String, String>,
    /// When the secret was first created.
    pub created_at: DateTime<Utc>,
    /// When the secret expires, if ever.
    pub expires_at: Option<DateTime<Utc>>,
    /// When the secret was last rotated. `None` if never rotated.
    pub rotated_at: Option<DateTime<Utc>>,
    /// Monotonically increasing version counter. Starts at 1.
    pub version: u32,
    /// Who is allowed to read this secret.
    pub access_policy: AccessPolicy,
}

impl Secret {
    /// Create a new secret with the given plaintext already encrypted.
    ///
    /// The caller is responsible for encrypting `value` before passing it
    /// here — this constructor stores whatever bytes it receives.
    pub fn new(
        name: impl Into<String>,
        kind: SecretKind,
        encrypted_value: Vec<u8>,
    ) -> Self {
        Self {
            id: SecretId::new(),
            kind,
            name: name.into(),
            encrypted_value,
            metadata: HashMap::new(),
            created_at: Utc::now(),
            expires_at: None,
            rotated_at: None,
            version: 1,
            access_policy: AccessPolicy::default(),
        }
    }

    /// Set the access policy.
    pub fn with_access_policy(mut self, policy: AccessPolicy) -> Self {
        self.access_policy = policy;
        self
    }

    /// Set the expiration timestamp.
    pub fn with_expiry(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Insert a metadata entry.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Returns `true` if the secret has expired as of `now`.
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| Utc::now() > exp)
            .unwrap_or(false)
    }

    /// Produce a lightweight reference to this secret (no value).
    pub fn as_ref(&self) -> SecretRef {
        SecretRef {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.clone(),
            version: self.version,
        }
    }
}

// ---------------------------------------------------------------------------
// SecretRef
// ---------------------------------------------------------------------------

/// Lightweight reference to a secret — carries identity but not the value.
///
/// Safe to log, display, and pass around in audit entries and notifications
/// without risk of leaking secret material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRef {
    /// The secret's unique identifier.
    pub id: SecretId,
    /// Human-readable name.
    pub name: String,
    /// Kind of secret.
    pub kind: SecretKind,
    /// Current version.
    pub version: u32,
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({}@v{})", self.name, self.id, self.version)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_id_time_ordered() {
        let a = SecretId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = SecretId::new();
        assert!(a.as_uuid() < b.as_uuid());
    }

    #[test]
    fn secret_id_roundtrip() {
        let id = SecretId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: SecretId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn secret_kind_display() {
        assert_eq!(SecretKind::ApiKey.to_string(), "api_key");
        assert_eq!(SecretKind::Custom("webhook".into()).to_string(), "custom:webhook");
    }

    #[test]
    fn access_policy_any_agent() {
        let policy = AccessPolicy::AnyAgent;
        let id = AgentId::new();
        assert!(policy.allows(&id, None, None));
    }

    #[test]
    fn access_policy_specific_agents() {
        let allowed = AgentId::new();
        let denied = AgentId::new();
        let policy = AccessPolicy::SpecificAgents(vec![allowed]);
        assert!(policy.allows(&allowed, None, None));
        assert!(!policy.allows(&denied, None, None));
    }

    #[test]
    fn access_policy_by_capability() {
        let id = AgentId::new();
        let policy = AccessPolicy::ByCapability("deploy".into());
        let caps = vec!["deploy".to_string(), "lint".to_string()];
        assert!(policy.allows(&id, None, Some(&caps)));
        let wrong_caps = vec!["lint".to_string()];
        assert!(!policy.allows(&id, None, Some(&wrong_caps)));
        // No caps provided — deny.
        assert!(!policy.allows(&id, None, None));
    }

    #[test]
    fn access_policy_by_kind() {
        let id = AgentId::new();
        let policy = AccessPolicy::ByKind(AgentKind::Worker);
        assert!(policy.allows(&id, Some(&AgentKind::Worker), None));
        assert!(!policy.allows(&id, Some(&AgentKind::Monitor), None));
        assert!(!policy.allows(&id, None, None));
    }

    #[test]
    fn secret_expiry() {
        let s = Secret::new("test", SecretKind::Token, vec![1, 2, 3])
            .with_expiry(Utc::now() - chrono::Duration::hours(1));
        assert!(s.is_expired());

        let s2 = Secret::new("test2", SecretKind::Token, vec![1, 2, 3])
            .with_expiry(Utc::now() + chrono::Duration::hours(1));
        assert!(!s2.is_expired());
    }

    #[test]
    fn secret_ref_no_value() {
        let s = Secret::new("api_key", SecretKind::ApiKey, vec![0xDE, 0xAD]);
        let r = s.as_ref();
        assert_eq!(r.name, "api_key");
        assert_eq!(r.version, 1);
        // SecretRef has no encrypted_value field — compile-time proof it's safe to log.
    }

    #[test]
    fn secret_metadata() {
        let s = Secret::new("k", SecretKind::Password, vec![])
            .with_metadata("team", "infra")
            .with_metadata("env", "prod");
        assert_eq!(s.metadata.get("team").unwrap(), "infra");
        assert_eq!(s.metadata.get("env").unwrap(), "prod");
    }
}
