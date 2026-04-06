//! Cryptographic identity primitives.
//!
//! Every node and agent in a DAF cluster has a cryptographic identity
//! based on Ed25519 signing keys and blake3 content hashes. This module
//! provides the building blocks for identity verification, payload
//! signing, and fingerprint computation.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{DafError, DafResult};

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

/// Content-addressed node identifier derived from a blake3 hash.
///
/// Used to uniquely identify a physical or virtual node in the cluster.
/// Two nodes with the same public key will always have the same `NodeId`.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(#[serde(with = "hash_serde")] [u8; 32]);

impl NodeId {
    /// Derive a node ID by hashing the given bytes (typically a public key).
    pub fn from_bytes(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        Self(*hash.as_bytes())
    }

    /// Wrap raw hash bytes.
    pub fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return the raw 32-byte hash.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hex-encoded representation.
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Short hex prefix for display (first 16 hex chars = 8 bytes).
    pub fn short(&self) -> String {
        self.to_hex()[..16].to_string()
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.short())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

/// A fingerprint for verifying agent identity.
///
/// Computed from an agent's public key material, the fingerprint is a
/// compact value that can be compared out-of-band to confirm identity.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint(#[serde(with = "hash_serde")] [u8; 32]);

impl Fingerprint {
    /// Compute a fingerprint from raw key material.
    pub fn from_public_key(public_key_bytes: &[u8]) -> Self {
        // Double-hash with a domain separator to prevent cross-protocol attacks.
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"daf-fingerprint-v1:");
        hasher.update(public_key_bytes);
        let hash = hasher.finalize();
        Self(*hash.as_bytes())
    }

    /// Wrap raw hash bytes.
    pub fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return the raw 32-byte hash.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hex-encoded representation.
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Human-friendly colon-separated format (like SSH fingerprints).
    pub fn to_colon_hex(&self) -> String {
        self.0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({}...)", &self.to_hex()[..16])
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_colon_hex())
    }
}

// ---------------------------------------------------------------------------
// KeyPair
// ---------------------------------------------------------------------------

/// An Ed25519 signing key pair for agent or node identity.
///
/// Wraps `ed25519_dalek` types and provides convenient sign/verify methods.
#[derive(Debug)]
pub struct KeyPair {
    signing_key: ed25519_dalek::SigningKey,
}

impl KeyPair {
    /// Generate a new random key pair.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        Self { signing_key }
    }

    /// Reconstruct a key pair from a 32-byte secret seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = ed25519_dalek::SigningKey::from_bytes(seed);
        Self { signing_key }
    }

    /// Return the public verifying key.
    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Return the raw public key bytes.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key().to_bytes()
    }

    /// Compute the [`NodeId`] for this key pair.
    pub fn node_id(&self) -> NodeId {
        NodeId::from_bytes(&self.public_key_bytes())
    }

    /// Compute the [`Fingerprint`] for this key pair.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::from_public_key(&self.public_key_bytes())
    }

    /// Sign arbitrary bytes, returning the 64-byte Ed25519 signature.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        use ed25519_dalek::Signer;
        let sig = self.signing_key.sign(message);
        sig.to_bytes().to_vec()
    }

    /// Verify a signature against this key pair's public key.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> DafResult<()> {
        verify_signature(&self.public_key_bytes(), message, signature)
    }
}

/// Verify an Ed25519 signature given raw public key bytes.
pub fn verify_signature(
    public_key_bytes: &[u8; 32],
    message: &[u8],
    signature_bytes: &[u8],
) -> DafResult<()> {
    use ed25519_dalek::Verifier;

    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|e| DafError::CryptoError(format!("invalid public key: {e}")))?;

    let sig_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| DafError::CryptoError("signature must be exactly 64 bytes".into()))?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);

    verifying_key
        .verify(message, &signature)
        .map_err(|e| DafError::CryptoError(format!("signature verification failed: {e}")))
}

// ---------------------------------------------------------------------------
// SignedPayload
// ---------------------------------------------------------------------------

/// A payload wrapped with an Ed25519 signature for integrity verification.
///
/// The signer serializes `T` to JSON, signs the bytes, and attaches the
/// signature alongside the signer's public key. Any recipient can verify
/// authenticity without needing the private key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPayload<T: Serialize> {
    /// The signed data.
    pub payload: T,
    /// Ed25519 signature over the JSON-serialized payload.
    #[serde(with = "bytes_hex")]
    pub signature: Vec<u8>,
    /// Public key of the signer (32 bytes).
    #[serde(with = "hash_serde")]
    pub signer_public_key: [u8; 32],
}

impl<T: Serialize + serde::de::DeserializeOwned> SignedPayload<T> {
    /// Create a signed payload using the given key pair.
    pub fn sign(payload: T, key_pair: &KeyPair) -> DafResult<Self> {
        let bytes = serde_json::to_vec(&payload)?;
        let signature = key_pair.sign(&bytes);
        Ok(Self {
            payload,
            signature,
            signer_public_key: key_pair.public_key_bytes(),
        })
    }

    /// Verify the signature and return a reference to the payload.
    pub fn verify(&self) -> DafResult<&T> {
        let bytes = serde_json::to_vec(&self.payload)?;
        verify_signature(&self.signer_public_key, &bytes, &self.signature)?;
        Ok(&self.payload)
    }

    /// Verify the signature and consume, returning the inner payload.
    pub fn into_verified(self) -> DafResult<T> {
        let bytes = serde_json::to_vec(&self.payload)?;
        verify_signature(&self.signer_public_key, &bytes, &self.signature)?;
        Ok(self.payload)
    }

    /// Return the signer's fingerprint.
    pub fn signer_fingerprint(&self) -> Fingerprint {
        Fingerprint::from_public_key(&self.signer_public_key)
    }
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Serde module for 32-byte arrays as hex strings.
mod hash_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex = String::deserialize(deserializer)?;
        let bytes = hex_to_bytes(&hex).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected exactly 32 bytes"))
    }

    fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
        if hex.len() % 2 != 0 {
            return Err("odd-length hex string".into());
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
            .collect()
    }
}

/// Serde module for variable-length byte vecs as hex strings.
mod bytes_hex {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex = String::deserialize(deserializer)?;
        if hex.len() % 2 != 0 {
            return Err(serde::de::Error::custom("odd-length hex string"));
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex[i..i + 2], 16).map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_deterministic() {
        let a = NodeId::from_bytes(b"same input");
        let b = NodeId::from_bytes(b"same input");
        assert_eq!(a, b);

        let c = NodeId::from_bytes(b"different");
        assert_ne!(a, c);
    }

    #[test]
    fn node_id_display() {
        let id = NodeId::from_bytes(b"test");
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars
        assert!(id.short().len() == 16);
    }

    #[test]
    fn node_id_serde_roundtrip() {
        let id = NodeId::from_bytes(b"roundtrip");
        let json = serde_json::to_string(&id).unwrap();
        let back: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn fingerprint_from_public_key() {
        let fp1 = Fingerprint::from_public_key(b"key1");
        let fp2 = Fingerprint::from_public_key(b"key1");
        assert_eq!(fp1, fp2);

        let fp3 = Fingerprint::from_public_key(b"key2");
        assert_ne!(fp1, fp3);
    }

    #[test]
    fn fingerprint_colon_format() {
        let fp = Fingerprint::from_public_key(b"test");
        let colon = fp.to_colon_hex();
        assert!(colon.contains(':'));
        // 32 bytes * 2 hex chars + 31 colons = 95 chars
        assert_eq!(colon.len(), 95);
    }

    #[test]
    fn keypair_sign_verify() {
        let kp = KeyPair::generate();
        let message = b"hello world";
        let sig = kp.sign(message);
        assert_eq!(sig.len(), 64);
        kp.verify(message, &sig).unwrap();
    }

    #[test]
    fn keypair_verify_wrong_message() {
        let kp = KeyPair::generate();
        let sig = kp.sign(b"correct");
        let result = kp.verify(b"wrong", &sig);
        assert!(result.is_err());
    }

    #[test]
    fn keypair_verify_wrong_key() {
        let kp1 = KeyPair::generate();
        let kp2 = KeyPair::generate();
        let sig = kp1.sign(b"message");
        let result = kp2.verify(b"message", &sig);
        assert!(result.is_err());
    }

    #[test]
    fn keypair_from_seed_deterministic() {
        let seed = [42u8; 32];
        let kp1 = KeyPair::from_seed(&seed);
        let kp2 = KeyPair::from_seed(&seed);
        assert_eq!(kp1.public_key_bytes(), kp2.public_key_bytes());
    }

    #[test]
    fn keypair_node_id_and_fingerprint() {
        let kp = KeyPair::generate();
        let node_id = kp.node_id();
        let fingerprint = kp.fingerprint();

        // Both should be deterministic for the same key.
        assert_eq!(node_id, kp.node_id());
        assert_eq!(fingerprint, kp.fingerprint());

        // But different from each other (different derivation).
        assert_ne!(node_id.as_bytes(), fingerprint.as_bytes());
    }

    #[test]
    fn signed_payload_roundtrip() {
        let kp = KeyPair::generate();
        let data = serde_json::json!({"action": "deploy", "version": 3});

        let signed = SignedPayload::sign(data.clone(), &kp).unwrap();
        let verified = signed.verify().unwrap();
        assert_eq!(*verified, data);
    }

    #[test]
    fn signed_payload_tampered() {
        let kp = KeyPair::generate();
        let data = serde_json::json!({"amount": 100});

        let mut signed = SignedPayload::sign(data, &kp).unwrap();
        // Tamper with the payload.
        signed.payload = serde_json::json!({"amount": 999});
        assert!(signed.verify().is_err());
    }

    #[test]
    fn signed_payload_serde() {
        let kp = KeyPair::generate();
        let data = serde_json::json!({"x": 1});
        let signed = SignedPayload::sign(data, &kp).unwrap();

        let json = serde_json::to_string(&signed).unwrap();
        let back: SignedPayload<serde_json::Value> = serde_json::from_str(&json).unwrap();
        back.verify().unwrap();
    }

    #[test]
    fn verify_signature_bad_length() {
        let pk = [0u8; 32];
        let result = verify_signature(&pk, b"msg", &[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn signed_payload_fingerprint() {
        let kp = KeyPair::generate();
        let signed = SignedPayload::sign("test".to_string(), &kp).unwrap();
        assert_eq!(signed.signer_fingerprint(), kp.fingerprint());
    }
}
