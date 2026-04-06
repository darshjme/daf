//! Vault audit trail.
//!
//! Every operation against the vault — reads, writes, deletes, rotations,
//! seal/unseal events — is recorded in an append-only audit log. Entries
//! are never deleted, only appended.
//!
//! The audit log is essential for compliance, forensics, and detecting
//! credential misuse by agents.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::secret::SecretRef;

// ---------------------------------------------------------------------------
// AuditAction
// ---------------------------------------------------------------------------

/// The operation that triggered an audit entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// A secret's plaintext was read.
    Read,
    /// A new secret was written to the vault.
    Write,
    /// A secret was deleted.
    Delete,
    /// A secret or master key was rotated.
    Rotate,
    /// The vault was sealed.
    Seal,
    /// The vault was unsealed.
    Unseal,
    /// A secret listing was requested.
    List,
}

impl std::fmt::Display for AuditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Delete => write!(f, "delete"),
            Self::Rotate => write!(f, "rotate"),
            Self::Seal => write!(f, "seal"),
            Self::Unseal => write!(f, "unseal"),
            Self::List => write!(f, "list"),
        }
    }
}

// ---------------------------------------------------------------------------
// AuditEntry
// ---------------------------------------------------------------------------

/// A single audit log entry recording a vault operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// When the operation occurred.
    pub timestamp: DateTime<Utc>,
    /// What operation was performed.
    pub action: AuditAction,
    /// Which secret was involved, if applicable.
    pub secret_ref: Option<SecretRef>,
    /// Which agent performed the operation, if applicable.
    pub agent_id: Option<AgentId>,
    /// Whether the operation succeeded.
    pub success: bool,
    /// Source IP address of the requester, if available.
    pub ip_address: Option<IpAddr>,
    /// Optional human-readable detail or error message.
    pub detail: Option<String>,
}

impl AuditEntry {
    /// Create a new successful audit entry.
    pub fn success(action: AuditAction) -> Self {
        Self {
            timestamp: Utc::now(),
            action,
            secret_ref: None,
            agent_id: None,
            success: true,
            ip_address: None,
            detail: None,
        }
    }

    /// Create a new failed audit entry with an error message.
    pub fn failure(action: AuditAction, detail: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            action,
            secret_ref: None,
            agent_id: None,
            success: false,
            ip_address: None,
            detail: Some(detail.into()),
        }
    }

    /// Set the secret reference.
    pub fn with_secret(mut self, secret_ref: SecretRef) -> Self {
        self.secret_ref = Some(secret_ref);
        self
    }

    /// Set the agent identifier.
    pub fn with_agent(mut self, agent_id: AgentId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    /// Set the source IP address.
    pub fn with_ip(mut self, ip: IpAddr) -> Self {
        self.ip_address = Some(ip);
        self
    }

    /// Set the detail message.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

// ---------------------------------------------------------------------------
// AuditLog
// ---------------------------------------------------------------------------

/// Append-only audit trail for vault operations.
///
/// Thread-safe via `parking_lot::RwLock`. All writes acquire an exclusive
/// lock; reads take a shared lock.
pub struct AuditLog {
    entries: RwLock<Vec<AuditEntry>>,
}

impl AuditLog {
    /// Create a new empty audit log.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Append an entry to the log.
    pub fn record(&self, entry: AuditEntry) {
        tracing::debug!(
            action = %entry.action,
            success = entry.success,
            agent = ?entry.agent_id,
            secret = ?entry.secret_ref.as_ref().map(|s| &s.name),
            "audit"
        );
        self.entries.write().push(entry);
    }

    /// Return all entries (newest last).
    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.read().clone()
    }

    /// Return the total number of entries.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Returns `true` if the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Query entries by time range (inclusive on both ends).
    pub fn query_by_time(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<AuditEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| e.timestamp >= from && e.timestamp <= to)
            .cloned()
            .collect()
    }

    /// Query entries for a specific agent.
    pub fn query_by_agent(&self, agent_id: &AgentId) -> Vec<AuditEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| e.agent_id.as_ref() == Some(agent_id))
            .cloned()
            .collect()
    }

    /// Query entries by action type.
    pub fn query_by_action(&self, action: AuditAction) -> Vec<AuditEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| e.action == action)
            .cloned()
            .collect()
    }

    /// Query entries for a specific secret (by name).
    pub fn query_by_secret_name(&self, name: &str) -> Vec<AuditEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| {
                e.secret_ref
                    .as_ref()
                    .map(|s| s.name == name)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Query only failed operations.
    pub fn query_failures(&self) -> Vec<AuditEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| !e.success)
            .cloned()
            .collect()
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::{SecretId, SecretKind};

    fn sample_ref() -> SecretRef {
        SecretRef {
            id: SecretId::new(),
            name: "test_key".into(),
            kind: SecretKind::ApiKey,
            version: 1,
        }
    }

    #[test]
    fn append_and_read() {
        let log = AuditLog::new();
        assert!(log.is_empty());

        log.record(AuditEntry::success(AuditAction::Write).with_secret(sample_ref()));
        log.record(AuditEntry::success(AuditAction::Read).with_secret(sample_ref()));

        assert_eq!(log.len(), 2);
        let entries = log.entries();
        assert_eq!(entries[0].action, AuditAction::Write);
        assert_eq!(entries[1].action, AuditAction::Read);
    }

    #[test]
    fn query_by_action() {
        let log = AuditLog::new();
        log.record(AuditEntry::success(AuditAction::Read));
        log.record(AuditEntry::success(AuditAction::Write));
        log.record(AuditEntry::success(AuditAction::Read));

        let reads = log.query_by_action(AuditAction::Read);
        assert_eq!(reads.len(), 2);
    }

    #[test]
    fn query_by_agent() {
        let log = AuditLog::new();
        let agent = AgentId::new();
        let other = AgentId::new();

        log.record(AuditEntry::success(AuditAction::Read).with_agent(agent));
        log.record(AuditEntry::success(AuditAction::Read).with_agent(other));
        log.record(AuditEntry::success(AuditAction::Write).with_agent(agent));

        let agent_entries = log.query_by_agent(&agent);
        assert_eq!(agent_entries.len(), 2);
    }

    #[test]
    fn query_failures() {
        let log = AuditLog::new();
        log.record(AuditEntry::success(AuditAction::Read));
        log.record(AuditEntry::failure(AuditAction::Read, "access denied"));
        log.record(AuditEntry::success(AuditAction::Write));

        let failures = log.query_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].detail.as_deref(), Some("access denied"));
    }

    #[test]
    fn query_by_secret_name() {
        let log = AuditLog::new();
        log.record(AuditEntry::success(AuditAction::Read).with_secret(sample_ref()));
        log.record(AuditEntry::success(AuditAction::Read));

        let results = log.query_by_secret_name("test_key");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn query_by_time_range() {
        let log = AuditLog::new();
        let before = Utc::now();
        log.record(AuditEntry::success(AuditAction::Write));
        std::thread::sleep(std::time::Duration::from_millis(10));
        let after = Utc::now();

        let results = log.query_by_time(before, after);
        assert_eq!(results.len(), 1);

        // Query a range in the past — should return nothing.
        let old = before - chrono::Duration::hours(1);
        let also_old = before - chrono::Duration::minutes(1);
        let results2 = log.query_by_time(old, also_old);
        assert!(results2.is_empty());
    }
}
