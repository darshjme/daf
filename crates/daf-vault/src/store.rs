//! Encrypted secret storage backends.
//!
//! Defines the [`VaultStore`] trait for async secret CRUD and two concrete
//! implementations:
//!
//! - [`SledVaultStore`] — production-grade, encrypted at rest, backed by sled.
//! - [`InMemoryVaultStore`] — lightweight, for testing and dev environments.
//!
//! All secrets are encrypted with per-secret data keys (envelope encryption)
//! before they touch storage. The store never sees plaintext values.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::audit::{AuditAction, AuditEntry, AuditLog};
use crate::keyring::{KeyId, KeyRing, WrappedDataKey};
use crate::secret::{AccessPolicy, Secret, SecretKind, SecretRef};
use crate::{VaultError, VaultResult};

// ---------------------------------------------------------------------------
// VaultStore trait
// ---------------------------------------------------------------------------

/// Async interface for secret storage operations.
///
/// Implementations must guarantee that secret values are encrypted at rest.
/// The `audit_log` is provided by the caller and should be populated by
/// every operation.
#[async_trait]
pub trait VaultStore: Send + Sync {
    /// Store a new secret. The `plaintext` will be encrypted before storage.
    async fn store_secret(
        &self,
        name: &str,
        kind: SecretKind,
        plaintext: &[u8],
        policy: AccessPolicy,
    ) -> VaultResult<SecretRef>;

    /// Retrieve a secret by name, decrypting its value.
    ///
    /// Returns the full [`Secret`] with the `encrypted_value` field containing
    /// the **decrypted** plaintext (yes, the field name is confusing — it
    /// reflects the at-rest representation; in-flight we return cleartext).
    async fn get_secret(&self, name: &str) -> VaultResult<Secret>;

    /// List all secrets without their values.
    async fn list_secrets(&self) -> VaultResult<Vec<SecretRef>>;

    /// Delete a secret by name.
    async fn delete_secret(&self, name: &str) -> VaultResult<()>;

    /// Rotate a secret: generate a new data key, re-encrypt with new value,
    /// increment version.
    async fn rotate_secret(&self, name: &str, new_plaintext: &[u8]) -> VaultResult<SecretRef>;
}

// ---------------------------------------------------------------------------
// StoredSecret — internal on-disk representation
// ---------------------------------------------------------------------------

/// Internal representation stored in sled/memory. Contains the encrypted
/// value and a reference to the data key that encrypted it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSecret {
    /// The secret metadata + encrypted value.
    secret: Secret,
    /// Which data key was used to encrypt this secret's value.
    data_key_id: KeyId,
}

// ---------------------------------------------------------------------------
// SledVaultStore
// ---------------------------------------------------------------------------

/// Production secret store backed by sled with per-secret envelope encryption.
///
/// # Storage layout
///
/// - Tree `secrets`: name -> `StoredSecret` (JSON)
/// - Tree `keys`: key_id -> `WrappedDataKey` (JSON)
/// - Tree `meta`: `salt`, `seal_check` (raw bytes)
pub struct SledVaultStore {
    db: sled::Db,
    keyring: RwLock<KeyRing>,
    audit: Arc<AuditLog>,
}

impl SledVaultStore {
    /// Open or create a vault store at the given path.
    ///
    /// The vault starts sealed. Call [`initialize`](Self::initialize) for
    /// first-time setup or [`unseal`](Self::unseal) to unlock an existing vault.
    pub fn open(path: impl AsRef<std::path::Path>, audit: Arc<AuditLog>) -> VaultResult<Self> {
        let db = sled::open(path)?;

        // Try to restore keyring state from the meta tree.
        let meta_tree = db.open_tree("meta")?;
        let keys_tree = db.open_tree("keys")?;

        let keyring = if let (Some(salt_iv), Some(check_iv)) =
            (meta_tree.get("salt")?, meta_tree.get("seal_check")?)
        {
            let mut salt = [0u8; 32];
            if salt_iv.len() == 32 {
                salt.copy_from_slice(&salt_iv);
            }

            // Restore wrapped keys.
            let mut wrapped = HashMap::new();
            for entry in keys_tree.iter() {
                let (_, v) = entry?;
                let wdk: WrappedDataKey = serde_json::from_slice(&v)?;
                wrapped.insert(wdk.id, wdk);
            }

            KeyRing::restore(salt, check_iv.to_vec(), wrapped)
        } else {
            KeyRing::new()
        };

        Ok(Self {
            db,
            keyring: RwLock::new(keyring),
            audit,
        })
    }

    /// First-time vault initialization with a passphrase.
    ///
    /// Derives the master key and persists the salt and seal check to sled.
    pub fn initialize(&self, passphrase: &[u8]) -> VaultResult<()> {
        let mut kr = self.keyring.write();
        kr.initialize(passphrase)?;

        let meta_tree = self.db.open_tree("meta")?;
        if let Some(salt) = kr.salt() {
            meta_tree.insert("salt", salt.as_slice())?;
        }
        if let Some(check) = kr.seal_check() {
            meta_tree.insert("seal_check", check)?;
        }
        self.db.flush()?;

        self.audit
            .record(AuditEntry::success(AuditAction::Unseal).with_detail("vault initialized"));
        Ok(())
    }

    /// Seal the vault — drop the master key from memory.
    pub fn seal(&self) {
        self.keyring.write().seal();
        self.audit.record(AuditEntry::success(AuditAction::Seal));
    }

    /// Unseal the vault with the operator passphrase.
    pub fn unseal(&self, passphrase: &[u8]) -> VaultResult<()> {
        let mut kr = self.keyring.write();
        kr.unseal(passphrase).map_err(|e| {
            self.audit
                .record(AuditEntry::failure(AuditAction::Unseal, e.to_string()));
            e
        })?;
        self.audit.record(AuditEntry::success(AuditAction::Unseal));
        Ok(())
    }

    /// Check if the vault is unsealed.
    pub fn is_unsealed(&self) -> bool {
        self.keyring.read().is_unsealed()
    }

    /// Persist a wrapped data key to the keys tree.
    fn persist_wrapped_key(&self, wdk: &WrappedDataKey) -> VaultResult<()> {
        let keys_tree = self.db.open_tree("keys")?;
        let data = serde_json::to_vec(wdk)?;
        keys_tree.insert(wdk.id.as_uuid().to_string(), data)?;
        Ok(())
    }

    /// Remove a wrapped data key from the keys tree.
    fn remove_persisted_key(&self, key_id: &KeyId) -> VaultResult<()> {
        let keys_tree = self.db.open_tree("keys")?;
        keys_tree.remove(key_id.as_uuid().to_string())?;
        Ok(())
    }
}

#[async_trait]
impl VaultStore for SledVaultStore {
    async fn store_secret(
        &self,
        name: &str,
        kind: SecretKind,
        plaintext: &[u8],
        policy: AccessPolicy,
    ) -> VaultResult<SecretRef> {
        let secrets_tree = self.db.open_tree("secrets")?;

        // Check for duplicate name.
        if secrets_tree.contains_key(name)? {
            let err = VaultError::Storage(format!("secret '{}' already exists", name));
            self.audit.record(
                AuditEntry::failure(AuditAction::Write, err.to_string()),
            );
            return Err(err);
        }

        // Generate a data key and encrypt the plaintext.
        let dk = {
            let mut kr = self.keyring.write();
            kr.generate_data_key()?
        };
        let encrypted = dk.encrypt(plaintext)?;

        // Persist the wrapped data key.
        {
            let kr = self.keyring.read();
            let wdk = kr
                .wrapped_keys()
                .get(&dk.id)
                .ok_or_else(|| VaultError::Internal("data key not in keyring".into()))?;
            self.persist_wrapped_key(wdk)?;
        }

        let secret = Secret::new(name, kind, encrypted).with_access_policy(policy);
        let secret_ref = secret.as_ref();

        let stored = StoredSecret {
            secret,
            data_key_id: dk.id,
        };
        let data = serde_json::to_vec(&stored)?;
        secrets_tree.insert(name, data)?;
        self.db.flush()?;

        debug!(name, "secret stored");
        self.audit.record(
            AuditEntry::success(AuditAction::Write).with_secret(secret_ref.clone()),
        );
        Ok(secret_ref)
    }

    async fn get_secret(&self, name: &str) -> VaultResult<Secret> {
        let secrets_tree = self.db.open_tree("secrets")?;

        let data = secrets_tree
            .get(name)?
            .ok_or_else(|| VaultError::SecretNotFound(name.into()))?;
        let stored: StoredSecret = serde_json::from_slice(&data)?;

        // Unwrap the data key and decrypt.
        let dk = self.keyring.read().unwrap_data_key(&stored.data_key_id)?;
        let plaintext = dk.decrypt(&stored.secret.encrypted_value)?;

        let mut secret = stored.secret;
        secret.encrypted_value = plaintext;

        self.audit.record(
            AuditEntry::success(AuditAction::Read).with_secret(secret.as_ref()),
        );
        Ok(secret)
    }

    async fn list_secrets(&self) -> VaultResult<Vec<SecretRef>> {
        let secrets_tree = self.db.open_tree("secrets")?;
        let mut refs = Vec::new();

        for entry in secrets_tree.iter() {
            let (_, v) = entry?;
            let stored: StoredSecret = serde_json::from_slice(&v)?;
            refs.push(stored.secret.as_ref());
        }

        self.audit.record(AuditEntry::success(AuditAction::List));
        Ok(refs)
    }

    async fn delete_secret(&self, name: &str) -> VaultResult<()> {
        let secrets_tree = self.db.open_tree("secrets")?;

        let data = secrets_tree
            .remove(name)?
            .ok_or_else(|| VaultError::SecretNotFound(name.into()))?;
        let stored: StoredSecret = serde_json::from_slice(&data)?;

        // Remove the data key from keyring and storage.
        {
            let mut kr = self.keyring.write();
            kr.remove_data_key(&stored.data_key_id);
        }
        self.remove_persisted_key(&stored.data_key_id)?;
        self.db.flush()?;

        debug!(name, "secret deleted");
        self.audit.record(
            AuditEntry::success(AuditAction::Delete).with_secret(stored.secret.as_ref()),
        );
        Ok(())
    }

    async fn rotate_secret(&self, name: &str, new_plaintext: &[u8]) -> VaultResult<SecretRef> {
        let secrets_tree = self.db.open_tree("secrets")?;

        let data = secrets_tree
            .get(name)?
            .ok_or_else(|| VaultError::SecretNotFound(name.into()))?;
        let mut stored: StoredSecret = serde_json::from_slice(&data)?;

        // Generate a new data key.
        let new_dk = {
            let mut kr = self.keyring.write();
            kr.generate_data_key()?
        };
        let encrypted = new_dk.encrypt(new_plaintext)?;

        // Persist the new wrapped data key.
        {
            let kr = self.keyring.read();
            let wdk = kr
                .wrapped_keys()
                .get(&new_dk.id)
                .ok_or_else(|| VaultError::Internal("data key not in keyring".into()))?;
            self.persist_wrapped_key(wdk)?;
        }

        // Remove old data key.
        let old_key_id = stored.data_key_id;
        {
            let mut kr = self.keyring.write();
            kr.remove_data_key(&old_key_id);
        }
        self.remove_persisted_key(&old_key_id)?;

        // Update the secret.
        stored.secret.encrypted_value = encrypted;
        stored.secret.version += 1;
        stored.secret.rotated_at = Some(Utc::now());
        stored.data_key_id = new_dk.id;

        let secret_ref = stored.secret.as_ref();
        let data = serde_json::to_vec(&stored)?;
        secrets_tree.insert(name, data)?;
        self.db.flush()?;

        debug!(name, version = secret_ref.version, "secret rotated");
        self.audit.record(
            AuditEntry::success(AuditAction::Rotate).with_secret(secret_ref.clone()),
        );
        Ok(secret_ref)
    }
}

// ---------------------------------------------------------------------------
// InMemoryVaultStore
// ---------------------------------------------------------------------------

/// In-memory vault store for testing.
///
/// Secrets are encrypted with the keyring just like [`SledVaultStore`],
/// but stored in a `HashMap` instead of on disk.
pub struct InMemoryVaultStore {
    secrets: RwLock<HashMap<String, StoredSecret>>,
    keyring: RwLock<KeyRing>,
    audit: Arc<AuditLog>,
}

impl InMemoryVaultStore {
    /// Create a new in-memory vault store, pre-initialized and unsealed
    /// with the given passphrase.
    pub fn new(passphrase: &[u8], audit: Arc<AuditLog>) -> VaultResult<Self> {
        let mut kr = KeyRing::new();
        kr.initialize(passphrase)?;
        Ok(Self {
            secrets: RwLock::new(HashMap::new()),
            keyring: RwLock::new(kr),
            audit,
        })
    }

    /// Seal the vault.
    pub fn seal(&self) {
        self.keyring.write().seal();
    }

    /// Unseal the vault.
    pub fn unseal(&self, passphrase: &[u8]) -> VaultResult<()> {
        self.keyring.write().unseal(passphrase)
    }

    /// Check if unsealed.
    pub fn is_unsealed(&self) -> bool {
        self.keyring.read().is_unsealed()
    }
}

#[async_trait]
impl VaultStore for InMemoryVaultStore {
    async fn store_secret(
        &self,
        name: &str,
        kind: SecretKind,
        plaintext: &[u8],
        policy: AccessPolicy,
    ) -> VaultResult<SecretRef> {
        if self.secrets.read().contains_key(name) {
            return Err(VaultError::Storage(format!(
                "secret '{}' already exists",
                name
            )));
        }

        let dk = self.keyring.write().generate_data_key()?;
        let encrypted = dk.encrypt(plaintext)?;

        let secret = Secret::new(name, kind, encrypted).with_access_policy(policy);
        let secret_ref = secret.as_ref();

        let stored = StoredSecret {
            secret,
            data_key_id: dk.id,
        };
        self.secrets.write().insert(name.to_string(), stored);

        self.audit.record(
            AuditEntry::success(AuditAction::Write).with_secret(secret_ref.clone()),
        );
        Ok(secret_ref)
    }

    async fn get_secret(&self, name: &str) -> VaultResult<Secret> {
        let secrets = self.secrets.read();
        let stored = secrets
            .get(name)
            .ok_or_else(|| VaultError::SecretNotFound(name.into()))?;

        let dk = self.keyring.read().unwrap_data_key(&stored.data_key_id)?;
        let plaintext = dk.decrypt(&stored.secret.encrypted_value)?;

        let mut secret = stored.secret.clone();
        secret.encrypted_value = plaintext;

        self.audit.record(
            AuditEntry::success(AuditAction::Read).with_secret(secret.as_ref()),
        );
        Ok(secret)
    }

    async fn list_secrets(&self) -> VaultResult<Vec<SecretRef>> {
        let refs: Vec<SecretRef> = self
            .secrets
            .read()
            .values()
            .map(|s| s.secret.as_ref())
            .collect();

        self.audit.record(AuditEntry::success(AuditAction::List));
        Ok(refs)
    }

    async fn delete_secret(&self, name: &str) -> VaultResult<()> {
        let stored = self
            .secrets
            .write()
            .remove(name)
            .ok_or_else(|| VaultError::SecretNotFound(name.into()))?;

        self.keyring.write().remove_data_key(&stored.data_key_id);

        self.audit.record(
            AuditEntry::success(AuditAction::Delete).with_secret(stored.secret.as_ref()),
        );
        Ok(())
    }

    async fn rotate_secret(&self, name: &str, new_plaintext: &[u8]) -> VaultResult<SecretRef> {
        let mut secrets = self.secrets.write();
        let stored = secrets
            .get_mut(name)
            .ok_or_else(|| VaultError::SecretNotFound(name.into()))?;

        let new_dk = self.keyring.write().generate_data_key()?;
        let encrypted = new_dk.encrypt(new_plaintext)?;

        // Remove old data key.
        self.keyring.write().remove_data_key(&stored.data_key_id);

        stored.secret.encrypted_value = encrypted;
        stored.secret.version += 1;
        stored.secret.rotated_at = Some(Utc::now());
        stored.data_key_id = new_dk.id;

        let secret_ref = stored.secret.as_ref();
        self.audit.record(
            AuditEntry::success(AuditAction::Rotate).with_secret(secret_ref.clone()),
        );
        Ok(secret_ref)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> InMemoryVaultStore {
        let audit = Arc::new(AuditLog::new());
        InMemoryVaultStore::new(b"test-pass", audit).unwrap()
    }

    #[tokio::test]
    async fn store_and_retrieve() {
        let store = make_store().await;
        store
            .store_secret("api_key", SecretKind::ApiKey, b"sk-123", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let secret = store.get_secret("api_key").await.unwrap();
        assert_eq!(secret.name, "api_key");
        assert_eq!(secret.encrypted_value, b"sk-123");
        assert_eq!(secret.version, 1);
    }

    #[tokio::test]
    async fn duplicate_name_rejected() {
        let store = make_store().await;
        store
            .store_secret("dup", SecretKind::Token, b"v1", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let result = store
            .store_secret("dup", SecretKind::Token, b"v2", AccessPolicy::AnyAgent)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_secrets() {
        let store = make_store().await;
        store
            .store_secret("a", SecretKind::ApiKey, b"1", AccessPolicy::AnyAgent)
            .await
            .unwrap();
        store
            .store_secret("b", SecretKind::Password, b"2", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let refs = store.list_secrets().await.unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[tokio::test]
    async fn delete_secret() {
        let store = make_store().await;
        store
            .store_secret("ephemeral", SecretKind::Token, b"tok", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        store.delete_secret("ephemeral").await.unwrap();
        assert!(store.get_secret("ephemeral").await.is_err());
    }

    #[tokio::test]
    async fn rotate_secret() {
        let store = make_store().await;
        store
            .store_secret("rotatable", SecretKind::ApiKey, b"old", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let new_ref = store.rotate_secret("rotatable", b"new").await.unwrap();
        assert_eq!(new_ref.version, 2);

        let secret = store.get_secret("rotatable").await.unwrap();
        assert_eq!(secret.encrypted_value, b"new");
        assert!(secret.rotated_at.is_some());
    }

    #[tokio::test]
    async fn sealed_store_rejects_operations() {
        let store = make_store().await;
        store
            .store_secret("before_seal", SecretKind::Token, b"v", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        store.seal();

        // All operations should fail.
        assert!(store.get_secret("before_seal").await.is_err());
        assert!(store
            .store_secret("new", SecretKind::Token, b"x", AccessPolicy::AnyAgent)
            .await
            .is_err());

        // Unseal and verify.
        store.unseal(b"test-pass").unwrap();
        let s = store.get_secret("before_seal").await.unwrap();
        assert_eq!(s.encrypted_value, b"v");
    }

    #[tokio::test]
    async fn audit_trail_recorded() {
        let audit = Arc::new(AuditLog::new());
        let store = InMemoryVaultStore::new(b"pass", audit.clone()).unwrap();

        store
            .store_secret("x", SecretKind::ApiKey, b"v", AccessPolicy::AnyAgent)
            .await
            .unwrap();
        store.get_secret("x").await.unwrap();
        store.delete_secret("x").await.unwrap();

        assert_eq!(audit.len(), 3);
        let actions: Vec<_> = audit.entries().iter().map(|e| e.action).collect();
        assert_eq!(actions, vec![AuditAction::Write, AuditAction::Read, AuditAction::Delete]);
    }

    #[tokio::test]
    async fn sled_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Arc::new(AuditLog::new());

        {
            let store = SledVaultStore::open(dir.path().join("vault"), audit.clone()).unwrap();
            store.initialize(b"sled-pass").unwrap();

            store
                .store_secret("db_password", SecretKind::Password, b"s3cret", AccessPolicy::AnyAgent)
                .await
                .unwrap();

            let s = store.get_secret("db_password").await.unwrap();
            assert_eq!(s.encrypted_value, b"s3cret");
        }

        // Re-open and unseal.
        {
            let audit2 = Arc::new(AuditLog::new());
            let store = SledVaultStore::open(dir.path().join("vault"), audit2).unwrap();
            store.unseal(b"sled-pass").unwrap();

            let s = store.get_secret("db_password").await.unwrap();
            assert_eq!(s.encrypted_value, b"s3cret");
        }
    }
}
