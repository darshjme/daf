//! Key and secret rotation.
//!
//! Provides automatic and manual rotation of secrets. The [`RotationManager`]
//! runs as a background task, checking secret ages against their
//! [`RotationPolicy`] and triggering re-encryption when secrets exceed their
//! maximum age.
//!
//! # Grace period
//!
//! When a secret is rotated, the old version remains valid for a configurable
//! grace period. During this window, agents using the old value will still
//! succeed while they pick up the rotation notification and switch to the
//! new value.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::secret::{SecretKind, SecretRef};
use crate::store::VaultStore;
use crate::{VaultError, VaultResult};

// ---------------------------------------------------------------------------
// RotationPolicy
// ---------------------------------------------------------------------------

/// Policy controlling when and how a secret should be rotated.
#[derive(Debug, Clone)]
pub struct RotationPolicy {
    /// How often to rotate the secret.
    pub rotate_interval: Duration,
    /// Maximum allowed age before forced rotation.
    pub max_age: Duration,
    /// Whether the rotation manager should rotate automatically.
    pub auto_rotate: bool,
    /// Grace period during which the old secret version remains valid
    /// after rotation, giving agents time to pick up the new value.
    pub grace_period: Duration,
}

impl RotationPolicy {
    /// Create a policy with sensible defaults (30-day rotation, 1-hour grace).
    pub fn new(rotate_interval: Duration) -> Self {
        Self {
            rotate_interval,
            max_age: rotate_interval,
            auto_rotate: true,
            grace_period: Duration::from_secs(3600),
        }
    }

    /// Set the maximum age.
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// Enable or disable auto-rotation.
    pub fn with_auto_rotate(mut self, auto: bool) -> Self {
        self.auto_rotate = auto;
        self
    }

    /// Set the grace period.
    pub fn with_grace_period(mut self, grace: Duration) -> Self {
        self.grace_period = grace;
        self
    }
}

impl Default for RotationPolicy {
    /// Default: 30-day rotation interval, auto-rotate on, 1-hour grace.
    fn default() -> Self {
        Self::new(Duration::from_secs(30 * 24 * 3600))
    }
}

// ---------------------------------------------------------------------------
// RotationEvent
// ---------------------------------------------------------------------------

/// Notification sent to agents when a secret is rotated.
#[derive(Debug, Clone)]
pub struct RotationEvent {
    /// Reference to the rotated secret (with new version).
    pub secret_ref: SecretRef,
    /// When the old version will stop being accepted.
    pub grace_expires_at: chrono::DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// RotationManager
// ---------------------------------------------------------------------------

/// Background manager that enforces rotation policies and notifies agents.
///
/// # Usage
///
/// ```ignore
/// let (mgr, mut rx) = RotationManager::new(store);
/// mgr.register("api_key", RotationPolicy::default(), generator);
///
/// // Start the background check loop.
/// let handle = mgr.start(Duration::from_secs(60));
///
/// // Listen for rotation events in another task.
/// tokio::spawn(async move {
///     while let Ok(event) = rx.recv().await {
///         println!("rotated: {}", event.secret_ref);
///     }
/// });
/// ```
pub struct RotationManager {
    store: Arc<dyn VaultStore>,
    /// Policies indexed by secret name.
    policies: RwLock<HashMap<String, RotationPolicy>>,
    /// Value generators indexed by secret name. Each generator produces
    /// the new plaintext value for a rotation.
    generators: RwLock<HashMap<String, Arc<dyn SecretValueGenerator>>>,
    /// Broadcast sender for rotation events.
    event_tx: broadcast::Sender<RotationEvent>,
    /// Archived old versions, kept for the grace period.
    /// Maps secret name -> list of (old_encrypted_value, expires_at).
    grace_archive: RwLock<HashMap<String, Vec<GraceEntry>>>,
}

/// An archived secret version kept alive during the grace period.
#[derive(Debug, Clone)]
struct GraceEntry {
    /// The old encrypted value.
    _old_value: Vec<u8>,
    /// When this grace entry expires.
    expires_at: chrono::DateTime<Utc>,
}

/// Generates a new plaintext value for a secret during rotation.
///
/// Implement this trait for secrets that can be auto-rotated (e.g., generate
/// a new random API key, request a new token from an OAuth provider, etc.).
#[async_trait::async_trait]
pub trait SecretValueGenerator: Send + Sync {
    /// Produce the new plaintext bytes for the secret.
    async fn generate(&self, secret_name: &str, kind: &SecretKind) -> VaultResult<Vec<u8>>;
}

/// Default generator that produces 32 random bytes.
pub struct RandomValueGenerator;

#[async_trait::async_trait]
impl SecretValueGenerator for RandomValueGenerator {
    async fn generate(&self, _name: &str, _kind: &SecretKind) -> VaultResult<Vec<u8>> {
        use ring::rand::{SecureRandom, SystemRandom};
        let rng = SystemRandom::new();
        let mut buf = vec![0u8; 32];
        rng.fill(&mut buf)
            .map_err(|_| VaultError::Crypto("random generation failed".into()))?;
        Ok(buf)
    }
}

impl RotationManager {
    /// Create a new rotation manager.
    ///
    /// Returns the manager and a broadcast receiver for rotation events.
    /// Clone the receiver for each agent that needs notifications.
    pub fn new(store: Arc<dyn VaultStore>) -> (Self, broadcast::Receiver<RotationEvent>) {
        let (tx, rx) = broadcast::channel(64);
        (
            Self {
                store,
                policies: RwLock::new(HashMap::new()),
                generators: RwLock::new(HashMap::new()),
                event_tx: tx,
                grace_archive: RwLock::new(HashMap::new()),
            },
            rx,
        )
    }

    /// Subscribe to rotation events (additional receiver).
    pub fn subscribe(&self) -> broadcast::Receiver<RotationEvent> {
        self.event_tx.subscribe()
    }

    /// Register a rotation policy and value generator for a secret.
    pub fn register(
        &self,
        secret_name: impl Into<String>,
        policy: RotationPolicy,
        generator: Arc<dyn SecretValueGenerator>,
    ) {
        let name = secret_name.into();
        self.policies.write().insert(name.clone(), policy);
        self.generators.write().insert(name, generator);
    }

    /// Remove a secret from rotation management.
    pub fn unregister(&self, secret_name: &str) {
        self.policies.write().remove(secret_name);
        self.generators.write().remove(secret_name);
    }

    /// Manually rotate a single secret, regardless of policy.
    pub async fn rotate_secret(&self, secret_name: &str) -> VaultResult<SecretRef> {
        let generator = {
            let gens = self.generators.read();
            gens.get(secret_name)
                .cloned()
                .ok_or_else(|| VaultError::Rotation(format!(
                    "no generator registered for '{}'",
                    secret_name
                )))?
        };

        // Get the current secret to know its kind.
        let current = self.store.get_secret(secret_name).await?;

        // Archive the old value for the grace period.
        let grace_duration = self
            .policies
            .read()
            .get(secret_name)
            .map(|p| p.grace_period)
            .unwrap_or(Duration::from_secs(3600));

        {
            let mut archive = self.grace_archive.write();
            let entries = archive.entry(secret_name.to_string()).or_default();
            entries.push(GraceEntry {
                _old_value: current.encrypted_value.clone(),
                expires_at: Utc::now() + chrono::Duration::from_std(grace_duration).unwrap_or_default(),
            });
        }

        // Generate new value and rotate.
        let new_value = generator.generate(secret_name, &current.kind).await?;
        let new_ref = self.store.rotate_secret(secret_name, &new_value).await?;

        info!(name = secret_name, version = new_ref.version, "secret rotated");

        // Notify listeners.
        let event = RotationEvent {
            secret_ref: new_ref.clone(),
            grace_expires_at: Utc::now()
                + chrono::Duration::from_std(grace_duration).unwrap_or_default(),
        };
        // Ignore send errors — there may be no active receivers.
        let _ = self.event_tx.send(event);

        Ok(new_ref)
    }

    /// Check all registered secrets and rotate any that have exceeded their
    /// policy age. Returns the names of secrets that were rotated.
    pub async fn check_and_rotate(&self) -> Vec<String> {
        let policies: Vec<(String, RotationPolicy)> = {
            self.policies
                .read()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        let mut rotated = Vec::new();

        for (name, policy) in &policies {
            if !policy.auto_rotate {
                continue;
            }

            match self.store.get_secret(name).await {
                Ok(secret) => {
                    let age = Utc::now() - secret.created_at;
                    let effective_age = if let Some(rotated_at) = secret.rotated_at {
                        Utc::now() - rotated_at
                    } else {
                        age
                    };

                    let max_age =
                        chrono::Duration::from_std(policy.max_age).unwrap_or_default();

                    if effective_age > max_age {
                        match self.rotate_secret(name).await {
                            Ok(new_ref) => {
                                debug!(
                                    name,
                                    new_version = new_ref.version,
                                    "auto-rotated secret"
                                );
                                rotated.push(name.clone());
                            }
                            Err(e) => {
                                warn!(name, error = %e, "auto-rotation failed");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(name, error = %e, "failed to check secret for rotation");
                }
            }
        }

        // Purge expired grace entries.
        self.purge_expired_grace();

        rotated
    }

    /// Start the background rotation check loop.
    ///
    /// Returns a `JoinHandle` that runs until the returned handle is aborted
    /// or the runtime shuts down.
    pub fn start(self: &Arc<Self>, check_interval: Duration) -> tokio::task::JoinHandle<()> {
        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);
            loop {
                interval.tick().await;
                let rotated = mgr.check_and_rotate().await;
                if !rotated.is_empty() {
                    info!(count = rotated.len(), "rotation check completed");
                }
            }
        })
    }

    /// Remove grace entries that have expired.
    fn purge_expired_grace(&self) {
        let now = Utc::now();
        let mut archive = self.grace_archive.write();
        for entries in archive.values_mut() {
            entries.retain(|e| e.expires_at > now);
        }
        archive.retain(|_, v| !v.is_empty());
    }

    /// Check if a secret name is registered for rotation.
    pub fn is_registered(&self, name: &str) -> bool {
        self.policies.read().contains_key(name)
    }

    /// Get the rotation policy for a secret, if registered.
    pub fn policy(&self, name: &str) -> Option<RotationPolicy> {
        self.policies.read().get(name).cloned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditLog;
    use crate::secret::AccessPolicy;
    use crate::store::InMemoryVaultStore;

    async fn setup() -> (Arc<InMemoryVaultStore>, Arc<RotationManager>, broadcast::Receiver<RotationEvent>) {
        let audit = Arc::new(AuditLog::new());
        let store = Arc::new(InMemoryVaultStore::new(b"pass", audit).unwrap());
        let (mgr, rx) = RotationManager::new(store.clone());
        (store, Arc::new(mgr), rx)
    }

    #[tokio::test]
    async fn manual_rotation() {
        let (store, mgr, _rx) = setup().await;

        store
            .store_secret("rotate_me", SecretKind::ApiKey, b"old-key", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        mgr.register(
            "rotate_me",
            RotationPolicy::default(),
            Arc::new(RandomValueGenerator),
        );

        let new_ref = mgr.rotate_secret("rotate_me").await.unwrap();
        assert_eq!(new_ref.version, 2);

        // The stored value should be different from the original.
        let secret = store.get_secret("rotate_me").await.unwrap();
        assert_ne!(secret.encrypted_value, b"old-key");
        assert_eq!(secret.version, 2);
    }

    #[tokio::test]
    async fn rotation_event_broadcast() {
        let (store, mgr, mut rx) = setup().await;

        store
            .store_secret("notified", SecretKind::Token, b"tok", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        mgr.register(
            "notified",
            RotationPolicy::default(),
            Arc::new(RandomValueGenerator),
        );

        mgr.rotate_secret("notified").await.unwrap();

        let event = rx.try_recv().unwrap();
        assert_eq!(event.secret_ref.name, "notified");
        assert_eq!(event.secret_ref.version, 2);
    }

    #[tokio::test]
    async fn auto_rotation_skips_fresh() {
        let (store, mgr, _rx) = setup().await;

        store
            .store_secret("fresh", SecretKind::ApiKey, b"val", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        // Policy with 1-hour max age — the secret we just created is fresh.
        mgr.register(
            "fresh",
            RotationPolicy::new(Duration::from_secs(3600)),
            Arc::new(RandomValueGenerator),
        );

        let rotated = mgr.check_and_rotate().await;
        assert!(rotated.is_empty(), "fresh secret should not be rotated");
    }

    #[tokio::test]
    async fn unregister_stops_rotation() {
        let (store, mgr, _rx) = setup().await;

        store
            .store_secret("temp", SecretKind::Token, b"val", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        mgr.register(
            "temp",
            RotationPolicy::default(),
            Arc::new(RandomValueGenerator),
        );
        assert!(mgr.is_registered("temp"));

        mgr.unregister("temp");
        assert!(!mgr.is_registered("temp"));
    }

    #[tokio::test]
    async fn rotate_unregistered_fails() {
        let (_store, mgr, _rx) = setup().await;
        let result = mgr.rotate_secret("unknown").await;
        assert!(result.is_err());
    }

    #[test]
    fn rotation_policy_builder() {
        let policy = RotationPolicy::new(Duration::from_secs(86400))
            .with_max_age(Duration::from_secs(172800))
            .with_auto_rotate(false)
            .with_grace_period(Duration::from_secs(300));

        assert_eq!(policy.rotate_interval, Duration::from_secs(86400));
        assert_eq!(policy.max_age, Duration::from_secs(172800));
        assert!(!policy.auto_rotate);
        assert_eq!(policy.grace_period, Duration::from_secs(300));
    }
}
