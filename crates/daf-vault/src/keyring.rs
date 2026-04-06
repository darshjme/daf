//! Key management: master keys, data keys, and envelope encryption.
//!
//! Implements a two-tier key hierarchy:
//!
//! 1. **Master key** — derived from a passphrase via PBKDF2 (600,000 iterations).
//!    Never stored in plaintext; lives only in memory while the vault is unsealed.
//!
//! 2. **Data keys** — one per secret, generated randomly. Each data key is
//!    encrypted (wrapped) by the master key using AES-256-GCM before storage.
//!    When a secret is read, the data key is unwrapped, used to decrypt the
//!    secret value, and then discarded.
//!
//! This is the same envelope-encryption pattern used by AWS KMS, GCP CMEK,
//! and HashiCorp Vault's transit backend.

use std::collections::HashMap;
use std::fmt;

use ring::aead::{self, Aad, BoundKey, Nonce, NonceSequence, NONCE_LEN};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{VaultError, VaultResult};

/// PBKDF2 iteration count — OWASP 2024 recommendation for SHA-256.
const PBKDF2_ITERATIONS: u32 = 600_000;

/// Salt length in bytes.
const SALT_LEN: usize = 32;

/// AES-256-GCM key length.
const KEY_LEN: usize = 32;

// ---------------------------------------------------------------------------
// KeyId
// ---------------------------------------------------------------------------

/// Unique identifier for a data key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyId(Uuid);

impl KeyId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for KeyId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Nonce counter (deterministic for AEAD BoundKey)
// ---------------------------------------------------------------------------

/// Simple counter-based nonce sequence for `ring`'s `BoundKey` API.
///
/// Each `CounterNonce` is created with a starting value and yields exactly
/// one nonce. For our usage pattern (one seal/open per BoundKey instance)
/// this is correct and avoids nonce reuse.
struct CounterNonce {
    nonce: Option<[u8; NONCE_LEN]>,
}

impl CounterNonce {
    fn new(nonce_bytes: [u8; NONCE_LEN]) -> Self {
        Self {
            nonce: Some(nonce_bytes),
        }
    }
}

impl NonceSequence for CounterNonce {
    fn advance(&mut self) -> Result<Nonce, ring::error::Unspecified> {
        self.nonce
            .take()
            .map(Nonce::assume_unique_for_key)
            .ok_or(ring::error::Unspecified)
    }
}

// ---------------------------------------------------------------------------
// MasterKey
// ---------------------------------------------------------------------------

/// Master encryption key derived from a passphrase.
///
/// The raw key material is zeroized on drop to minimize exposure in memory.
pub struct MasterKey {
    /// The derived 256-bit key.
    key_material: [u8; KEY_LEN],
    /// Salt used during derivation — stored alongside the key so we can
    /// verify passphrases on subsequent unseals.
    salt: [u8; SALT_LEN],
}

impl MasterKey {
    /// Derive a new master key from a passphrase, generating a fresh salt.
    pub fn derive(passphrase: &[u8]) -> VaultResult<Self> {
        let rng = SystemRandom::new();
        let mut salt = [0u8; SALT_LEN];
        rng.fill(&mut salt)
            .map_err(|_| VaultError::Crypto("failed to generate salt".into()))?;

        let mut key_material = [0u8; KEY_LEN];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::new(PBKDF2_ITERATIONS).unwrap(),
            &salt,
            passphrase,
            &mut key_material,
        );

        Ok(Self { key_material, salt })
    }

    /// Re-derive the master key from a passphrase and a known salt.
    ///
    /// Used during unseal to reconstruct the key and verify it against
    /// stored ciphertext.
    pub fn derive_with_salt(passphrase: &[u8], salt: &[u8; SALT_LEN]) -> Self {
        let mut key_material = [0u8; KEY_LEN];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::new(PBKDF2_ITERATIONS).unwrap(),
            salt,
            passphrase,
            &mut key_material,
        );
        Self {
            key_material,
            salt: *salt,
        }
    }

    /// Verify that a passphrase matches this master key's derivation.
    pub fn verify(&self, passphrase: &[u8]) -> bool {
        pbkdf2::verify(
            pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::new(PBKDF2_ITERATIONS).unwrap(),
            &self.salt,
            passphrase,
            &self.key_material,
        )
        .is_ok()
    }

    /// Return the salt (needed for persistent storage of the seal parameters).
    pub fn salt(&self) -> &[u8; SALT_LEN] {
        &self.salt
    }

    /// Encrypt `plaintext` using AES-256-GCM with a random nonce.
    ///
    /// Returns `nonce || ciphertext || tag` as a single `Vec<u8>`.
    pub fn encrypt(&self, plaintext: &[u8]) -> VaultResult<Vec<u8>> {
        aead_encrypt(&self.key_material, plaintext)
    }

    /// Decrypt `nonce || ciphertext || tag` produced by [`encrypt`](Self::encrypt).
    pub fn decrypt(&self, ciphertext: &[u8]) -> VaultResult<Vec<u8>> {
        aead_decrypt(&self.key_material, ciphertext)
    }
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        // Best-effort zeroization. The compiler may elide this in release
        // builds; a proper zeroize crate would use a volatile write. For our
        // threat model (process-local secrets) this is acceptable.
        self.key_material.fill(0);
        self.salt.fill(0);
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MasterKey")
            .field("key_material", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// DataKey
// ---------------------------------------------------------------------------

/// Per-secret data key used for envelope encryption.
///
/// The plaintext key material is zeroized on drop.
#[derive(Clone)]
pub struct DataKey {
    /// Identifier for this data key.
    pub id: KeyId,
    /// Raw 256-bit key material.
    key_material: [u8; KEY_LEN],
}

impl DataKey {
    /// Generate a new random data key.
    pub fn generate() -> VaultResult<Self> {
        let rng = SystemRandom::new();
        let mut key_material = [0u8; KEY_LEN];
        rng.fill(&mut key_material)
            .map_err(|_| VaultError::Crypto("failed to generate data key".into()))?;
        Ok(Self {
            id: KeyId::new(),
            key_material,
        })
    }

    /// Reconstruct a data key from raw bytes (after unwrapping).
    pub fn from_raw(id: KeyId, material: [u8; KEY_LEN]) -> Self {
        Self {
            id,
            key_material: material,
        }
    }

    /// Encrypt plaintext with this data key.
    pub fn encrypt(&self, plaintext: &[u8]) -> VaultResult<Vec<u8>> {
        aead_encrypt(&self.key_material, plaintext)
    }

    /// Decrypt ciphertext encrypted by this data key.
    pub fn decrypt(&self, ciphertext: &[u8]) -> VaultResult<Vec<u8>> {
        aead_decrypt(&self.key_material, ciphertext)
    }

    /// Return the raw key material (for wrapping by the master key).
    pub fn raw(&self) -> &[u8; KEY_LEN] {
        &self.key_material
    }
}

impl Drop for DataKey {
    fn drop(&mut self) {
        self.key_material.fill(0);
    }
}

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DataKey")
            .field("id", &self.id)
            .field("key_material", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// WrappedDataKey (serializable form for storage)
// ---------------------------------------------------------------------------

/// A data key encrypted (wrapped) by the master key, safe for storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedDataKey {
    /// The data key's identifier.
    pub id: KeyId,
    /// `nonce || ciphertext || tag` of the data key material.
    pub wrapped: Vec<u8>,
}

// ---------------------------------------------------------------------------
// KeyRing
// ---------------------------------------------------------------------------

/// Manages the master key and a collection of data keys.
///
/// The key ring starts in a sealed state. Call [`unseal`](Self::unseal) with
/// the operator passphrase to derive the master key and make the ring usable.
pub struct KeyRing {
    /// The master key — `None` while sealed.
    master: Option<MasterKey>,
    /// Persisted salt from the initial seal setup.
    salt: Option<[u8; SALT_LEN]>,
    /// Wrapped data keys indexed by [`KeyId`].
    wrapped_keys: HashMap<KeyId, WrappedDataKey>,
    /// Verification ciphertext: a known plaintext encrypted with the master
    /// key so we can verify passphrases without storing the key itself.
    seal_check: Option<Vec<u8>>,
}

/// Magic plaintext used to verify the passphrase on unseal.
const SEAL_CHECK_PLAINTEXT: &[u8] = b"daf-vault-seal-check-v1";

impl KeyRing {
    /// Create a new, empty, sealed key ring.
    pub fn new() -> Self {
        Self {
            master: None,
            salt: None,
            wrapped_keys: HashMap::new(),
            seal_check: None,
        }
    }

    /// Initialize the key ring with a passphrase for the first time.
    ///
    /// Derives a master key, stores the salt and a verification ciphertext,
    /// and leaves the ring in the unsealed state.
    pub fn initialize(&mut self, passphrase: &[u8]) -> VaultResult<()> {
        let master = MasterKey::derive(passphrase)?;
        let check = master.encrypt(SEAL_CHECK_PLAINTEXT)?;
        self.salt = Some(*master.salt());
        self.seal_check = Some(check);
        self.master = Some(master);
        Ok(())
    }

    /// Seal the vault — drop the master key from memory.
    pub fn seal(&mut self) {
        self.master = None;
        tracing::info!("vault sealed");
    }

    /// Unseal the vault by re-deriving the master key from the passphrase.
    ///
    /// Fails if the passphrase does not match.
    pub fn unseal(&mut self, passphrase: &[u8]) -> VaultResult<()> {
        let salt = self.salt.ok_or(VaultError::Internal(
            "key ring not initialized — call initialize() first".into(),
        ))?;
        let candidate = MasterKey::derive_with_salt(passphrase, &salt);

        // Verify by decrypting the seal check ciphertext.
        let check_ct = self
            .seal_check
            .as_ref()
            .ok_or(VaultError::Internal("missing seal check".into()))?;
        let plaintext = candidate
            .decrypt(check_ct)
            .map_err(|_| VaultError::InvalidPassphrase)?;
        if plaintext != SEAL_CHECK_PLAINTEXT {
            return Err(VaultError::InvalidPassphrase);
        }

        self.master = Some(candidate);
        tracing::info!("vault unsealed");
        Ok(())
    }

    /// Returns `true` if the vault is currently unsealed (master key in memory).
    pub fn is_unsealed(&self) -> bool {
        self.master.is_some()
    }

    /// Returns `true` if the vault is sealed.
    pub fn is_sealed(&self) -> bool {
        self.master.is_none()
    }

    /// Generate a new data key and wrap it with the master key.
    ///
    /// Returns the unwrapped data key for immediate use and stores the
    /// wrapped copy internally.
    pub fn generate_data_key(&mut self) -> VaultResult<DataKey> {
        let master = self.master.as_ref().ok_or(VaultError::Sealed)?;
        let dk = DataKey::generate()?;
        let wrapped = master.encrypt(dk.raw())?;
        let wdk = WrappedDataKey {
            id: dk.id,
            wrapped,
        };
        self.wrapped_keys.insert(dk.id, wdk);
        Ok(dk)
    }

    /// Unwrap (decrypt) a data key by its identifier.
    pub fn unwrap_data_key(&self, key_id: &KeyId) -> VaultResult<DataKey> {
        let master = self.master.as_ref().ok_or(VaultError::Sealed)?;
        let wdk = self
            .wrapped_keys
            .get(key_id)
            .ok_or_else(|| VaultError::KeyNotFound(key_id.to_string()))?;
        let raw = master.decrypt(&wdk.wrapped)?;
        if raw.len() != KEY_LEN {
            return Err(VaultError::Crypto("invalid data key length".into()));
        }
        let mut material = [0u8; KEY_LEN];
        material.copy_from_slice(&raw);
        Ok(DataKey::from_raw(*key_id, material))
    }

    /// Remove a data key from the ring.
    pub fn remove_data_key(&mut self, key_id: &KeyId) -> bool {
        self.wrapped_keys.remove(key_id).is_some()
    }

    /// Re-wrap all data keys with a new master key derived from a new passphrase.
    ///
    /// This is used for master key rotation. All data keys are unwrapped with
    /// the old master, then re-wrapped with the new one.
    pub fn rotate_master_key(&mut self, new_passphrase: &[u8]) -> VaultResult<()> {
        let old_master = self.master.as_ref().ok_or(VaultError::Sealed)?;

        // Unwrap all data keys with the old master.
        let mut unwrapped: Vec<(KeyId, [u8; KEY_LEN])> = Vec::new();
        for (kid, wdk) in &self.wrapped_keys {
            let raw = old_master.decrypt(&wdk.wrapped)?;
            let mut material = [0u8; KEY_LEN];
            material.copy_from_slice(&raw);
            unwrapped.push((*kid, material));
        }

        // Derive new master.
        let new_master = MasterKey::derive(new_passphrase)?;
        let check = new_master.encrypt(SEAL_CHECK_PLAINTEXT)?;

        // Re-wrap all data keys.
        let mut new_wrapped = HashMap::new();
        for (kid, material) in &unwrapped {
            let wrapped = new_master.encrypt(material)?;
            new_wrapped.insert(
                *kid,
                WrappedDataKey {
                    id: *kid,
                    wrapped,
                },
            );
        }

        // Zeroize unwrapped material.
        for (_, mut material) in unwrapped {
            material.fill(0);
        }

        self.salt = Some(*new_master.salt());
        self.seal_check = Some(check);
        self.wrapped_keys = new_wrapped;
        self.master = Some(new_master);

        tracing::info!("master key rotated");
        Ok(())
    }

    /// Return the salt (for external persistence).
    pub fn salt(&self) -> Option<&[u8; SALT_LEN]> {
        self.salt.as_ref()
    }

    /// Return the seal check ciphertext (for external persistence).
    pub fn seal_check(&self) -> Option<&[u8]> {
        self.seal_check.as_deref()
    }

    /// Restore persisted state (salt, seal check, wrapped keys).
    pub fn restore(
        salt: [u8; SALT_LEN],
        seal_check: Vec<u8>,
        wrapped_keys: HashMap<KeyId, WrappedDataKey>,
    ) -> Self {
        Self {
            master: None,
            salt: Some(salt),
            wrapped_keys,
            seal_check: Some(seal_check),
        }
    }

    /// Return all wrapped data keys (for external persistence).
    pub fn wrapped_keys(&self) -> &HashMap<KeyId, WrappedDataKey> {
        &self.wrapped_keys
    }

    /// Import a wrapped data key (used when restoring from storage).
    pub fn import_wrapped_key(&mut self, wdk: WrappedDataKey) {
        self.wrapped_keys.insert(wdk.id, wdk);
    }
}

impl Default for KeyRing {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AEAD helpers (AES-256-GCM)
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` with AES-256-GCM using the given 256-bit key.
///
/// Output format: `nonce (12 bytes) || ciphertext || tag (16 bytes)`.
fn aead_encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> VaultResult<Vec<u8>> {
    let rng = SystemRandom::new();
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rng.fill(&mut nonce_bytes)
        .map_err(|_| VaultError::Crypto("nonce generation failed".into()))?;

    let unbound =
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| VaultError::Crypto("invalid key".into()))?;
    let mut sealing_key = aead::SealingKey::new(unbound, CounterNonce::new(nonce_bytes));

    let mut in_out = plaintext.to_vec();
    sealing_key
        .seal_in_place_append_tag(Aad::empty(), &mut in_out)
        .map_err(|_| VaultError::Crypto("seal failed".into()))?;

    // Prepend nonce.
    let mut output = Vec::with_capacity(NONCE_LEN + in_out.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&in_out);
    Ok(output)
}

/// Decrypt `nonce || ciphertext || tag` with AES-256-GCM.
fn aead_decrypt(key: &[u8; KEY_LEN], data: &[u8]) -> VaultResult<Vec<u8>> {
    if data.len() < NONCE_LEN + aead::AES_256_GCM.tag_len() {
        return Err(VaultError::Crypto("ciphertext too short".into()));
    }

    let (nonce_bytes, ct_and_tag) = data.split_at(NONCE_LEN);
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(nonce_bytes);

    let unbound =
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| VaultError::Crypto("invalid key".into()))?;
    let mut opening_key = aead::OpeningKey::new(unbound, CounterNonce::new(nonce_arr));

    let mut in_out = ct_and_tag.to_vec();
    let plaintext = opening_key
        .open_in_place(Aad::empty(), &mut in_out)
        .map_err(|_| VaultError::Crypto("decryption failed — wrong key or corrupted data".into()))?;

    Ok(plaintext.to_vec())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_key_roundtrip() {
        let mk = MasterKey::derive(b"hunter2").unwrap();
        let ct = mk.encrypt(b"hello world").unwrap();
        let pt = mk.decrypt(&ct).unwrap();
        assert_eq!(pt, b"hello world");
    }

    #[test]
    fn master_key_wrong_passphrase_fails() {
        let mk = MasterKey::derive(b"correct").unwrap();
        let ct = mk.encrypt(b"secret data").unwrap();

        let bad = MasterKey::derive_with_salt(b"wrong", mk.salt());
        assert!(bad.decrypt(&ct).is_err());
    }

    #[test]
    fn master_key_verify() {
        let mk = MasterKey::derive(b"passphrase").unwrap();
        assert!(mk.verify(b"passphrase"));
        assert!(!mk.verify(b"wrong"));
    }

    #[test]
    fn data_key_roundtrip() {
        let dk = DataKey::generate().unwrap();
        let ct = dk.encrypt(b"agent credentials").unwrap();
        let pt = dk.decrypt(&ct).unwrap();
        assert_eq!(pt, b"agent credentials");
    }

    #[test]
    fn envelope_encryption_roundtrip() {
        let mk = MasterKey::derive(b"master-pass").unwrap();
        let dk = DataKey::generate().unwrap();

        // Wrap the data key.
        let wrapped = mk.encrypt(dk.raw()).unwrap();

        // Encrypt secret with data key.
        let secret_ct = dk.encrypt(b"top secret").unwrap();

        // Unwrap data key.
        let raw = mk.decrypt(&wrapped).unwrap();
        let mut material = [0u8; KEY_LEN];
        material.copy_from_slice(&raw);
        let dk2 = DataKey::from_raw(dk.id, material);

        // Decrypt secret.
        let secret_pt = dk2.decrypt(&secret_ct).unwrap();
        assert_eq!(secret_pt, b"top secret");
    }

    #[test]
    fn keyring_seal_unseal() {
        let mut kr = KeyRing::new();
        kr.initialize(b"passphrase").unwrap();
        assert!(kr.is_unsealed());

        let dk = kr.generate_data_key().unwrap();
        let ct = dk.encrypt(b"data").unwrap();

        kr.seal();
        assert!(kr.is_sealed());
        assert!(kr.generate_data_key().is_err());
        assert!(kr.unwrap_data_key(&dk.id).is_err());

        kr.unseal(b"passphrase").unwrap();
        assert!(kr.is_unsealed());

        let dk2 = kr.unwrap_data_key(&dk.id).unwrap();
        let pt = dk2.decrypt(&ct).unwrap();
        assert_eq!(pt, b"data");
    }

    #[test]
    fn keyring_wrong_passphrase() {
        let mut kr = KeyRing::new();
        kr.initialize(b"correct").unwrap();
        kr.seal();
        assert!(kr.unseal(b"wrong").is_err());
    }

    #[test]
    fn keyring_master_rotation() {
        let mut kr = KeyRing::new();
        kr.initialize(b"old-pass").unwrap();

        let dk = kr.generate_data_key().unwrap();
        let ct = dk.encrypt(b"payload").unwrap();

        kr.rotate_master_key(b"new-pass").unwrap();

        // Old passphrase should fail.
        kr.seal();
        assert!(kr.unseal(b"old-pass").is_err());

        // New passphrase works.
        kr.unseal(b"new-pass").unwrap();
        let dk2 = kr.unwrap_data_key(&dk.id).unwrap();
        let pt = dk2.decrypt(&ct).unwrap();
        assert_eq!(pt, b"payload");
    }

    #[test]
    fn aead_empty_plaintext() {
        let mk = MasterKey::derive(b"x").unwrap();
        let ct = mk.encrypt(b"").unwrap();
        let pt = mk.decrypt(&ct).unwrap();
        assert!(pt.is_empty());
    }

    #[test]
    fn aead_truncated_ciphertext_fails() {
        let result = aead_decrypt(&[0u8; KEY_LEN], &[0u8; 5]);
        assert!(result.is_err());
    }
}
