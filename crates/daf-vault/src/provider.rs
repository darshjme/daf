//! Secret provider interface.
//!
//! Providers abstract where secrets come from. A [`VaultProvider`] reads from
//! the local vault store, an [`EnvProvider`] reads from environment variables,
//! and a [`ChainProvider`] tries multiple providers in order.
//!
//! The provider layer also handles template string interpolation:
//! `${vault:secret_name}` references are resolved by walking the provider
//! chain.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::secret::{AccessPolicy, Secret, SecretKind};
use crate::store::VaultStore;
use crate::{VaultError, VaultResult};

// ---------------------------------------------------------------------------
// SecretProvider trait
// ---------------------------------------------------------------------------

/// Async interface for resolving a secret by name.
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Resolve a secret by its logical name.
    ///
    /// Returns the full [`Secret`] with decrypted value on success, or
    /// [`VaultError::SecretNotFound`] if the provider cannot locate it.
    async fn resolve(&self, name: &str) -> VaultResult<Secret>;
}

// ---------------------------------------------------------------------------
// VaultProvider
// ---------------------------------------------------------------------------

/// Reads secrets from a [`VaultStore`] backend.
pub struct VaultProvider {
    store: Arc<dyn VaultStore>,
}

impl VaultProvider {
    /// Create a provider backed by the given store.
    pub fn new(store: Arc<dyn VaultStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl SecretProvider for VaultProvider {
    async fn resolve(&self, name: &str) -> VaultResult<Secret> {
        self.store.get_secret(name).await
    }
}

// ---------------------------------------------------------------------------
// EnvProvider
// ---------------------------------------------------------------------------

/// Reads secrets from environment variables.
///
/// The lookup maps a secret name to an environment variable using a
/// configurable prefix. For example, with prefix `"DAF_SECRET_"`, resolving
/// `"openai_key"` reads `DAF_SECRET_OPENAI_KEY`.
pub struct EnvProvider {
    /// Prefix prepended to the uppercased secret name.
    prefix: String,
    /// Optional explicit overrides: name -> env var name.
    overrides: HashMap<String, String>,
}

impl EnvProvider {
    /// Create a provider with the default prefix `"DAF_SECRET_"`.
    pub fn new() -> Self {
        Self {
            prefix: "DAF_SECRET_".into(),
            overrides: HashMap::new(),
        }
    }

    /// Create a provider with a custom prefix.
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            overrides: HashMap::new(),
        }
    }

    /// Add an explicit mapping from secret name to env var name.
    pub fn with_override(mut self, name: impl Into<String>, env_var: impl Into<String>) -> Self {
        self.overrides.insert(name.into(), env_var.into());
        self
    }

    /// Compute the environment variable name for a secret.
    fn env_var_name(&self, name: &str) -> String {
        if let Some(explicit) = self.overrides.get(name) {
            return explicit.clone();
        }
        format!("{}{}", self.prefix, name.to_uppercase())
    }
}

impl Default for EnvProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretProvider for EnvProvider {
    async fn resolve(&self, name: &str) -> VaultResult<Secret> {
        let var_name = self.env_var_name(name);
        let value = std::env::var(&var_name)
            .map_err(|_| VaultError::SecretNotFound(format!("env var '{}' not set", var_name)))?;

        // Wrap the value as a Secret. Since it came from the environment,
        // the "encrypted_value" field contains plaintext (there's no encryption
        // layer for env vars).
        Ok(Secret::new(name, SecretKind::Token, value.into_bytes())
            .with_access_policy(AccessPolicy::AnyAgent)
            .with_metadata("source", "environment")
            .with_metadata("env_var", var_name))
    }
}

// ---------------------------------------------------------------------------
// ChainProvider
// ---------------------------------------------------------------------------

/// Tries multiple providers in order until one succeeds.
///
/// This is the primary provider used in production — it falls through from
/// the vault store to environment variables to any custom providers.
pub struct ChainProvider {
    providers: Vec<Box<dyn SecretProvider>>,
}

impl ChainProvider {
    /// Create an empty chain.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Add a provider to the end of the chain.
    pub fn with_provider(mut self, provider: impl SecretProvider + 'static) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// Add a provider to the end of the chain (non-builder variant).
    pub fn push(&mut self, provider: impl SecretProvider + 'static) {
        self.providers.push(Box::new(provider));
    }

    /// Number of providers in the chain.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Returns `true` if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl Default for ChainProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretProvider for ChainProvider {
    async fn resolve(&self, name: &str) -> VaultResult<Secret> {
        for provider in &self.providers {
            match provider.resolve(name).await {
                Ok(secret) => return Ok(secret),
                Err(VaultError::SecretNotFound(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(VaultError::SecretNotFound(format!(
            "'{}' not found in any provider ({} tried)",
            name,
            self.providers.len()
        )))
    }
}

// ---------------------------------------------------------------------------
// Template interpolation
// ---------------------------------------------------------------------------

/// Resolve `${vault:secret_name}` references in a template string.
///
/// Each reference is replaced with the UTF-8 decoded secret value. Non-UTF-8
/// values produce an error.
///
/// # Example
///
/// ```text
/// "Authorization: Bearer ${vault:openai_key}"
/// ```
pub async fn resolve_template(
    template: &str,
    provider: &dyn SecretProvider,
) -> VaultResult<String> {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    loop {
        match rest.find("${vault:") {
            Some(start) => {
                // Copy the literal prefix.
                result.push_str(&rest[..start]);

                let after_prefix = &rest[start + 8..];
                let end = after_prefix
                    .find('}')
                    .ok_or_else(|| VaultError::Provider("unclosed ${vault:...} reference".into()))?;

                let secret_name = &after_prefix[..end];
                let secret = provider.resolve(secret_name).await?;

                let value_str = String::from_utf8(secret.encrypted_value)
                    .map_err(|_| VaultError::Provider(format!(
                        "secret '{}' is not valid UTF-8 for template interpolation",
                        secret_name
                    )))?;

                result.push_str(&value_str);
                rest = &after_prefix[end + 1..];
            }
            None => {
                result.push_str(rest);
                break;
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditLog;
    use crate::store::InMemoryVaultStore;
    use std::sync::Arc;

    async fn make_vault_provider() -> (Arc<InMemoryVaultStore>, VaultProvider) {
        let audit = Arc::new(AuditLog::new());
        let store = Arc::new(InMemoryVaultStore::new(b"pass", audit).unwrap());
        let provider = VaultProvider::new(store.clone());
        (store, provider)
    }

    #[tokio::test]
    async fn vault_provider_resolves() {
        let (store, provider) = make_vault_provider().await;
        store
            .store_secret("my_key", SecretKind::ApiKey, b"secret-value", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let secret = provider.resolve("my_key").await.unwrap();
        assert_eq!(secret.encrypted_value, b"secret-value");
    }

    #[tokio::test]
    async fn vault_provider_not_found() {
        let (_, provider) = make_vault_provider().await;
        assert!(provider.resolve("missing").await.is_err());
    }

    #[tokio::test]
    async fn env_provider_resolves() {
        // SAFETY: test-only, single-threaded access to env vars.
        unsafe { std::env::set_var("DAF_SECRET_TEST_TOKEN", "env-value-123") };
        let provider = EnvProvider::new();
        let secret = provider.resolve("test_token").await.unwrap();
        assert_eq!(secret.encrypted_value, b"env-value-123");
        unsafe { std::env::remove_var("DAF_SECRET_TEST_TOKEN") };
    }

    #[tokio::test]
    async fn env_provider_override() {
        // SAFETY: test-only, single-threaded access to env vars.
        unsafe { std::env::set_var("MY_CUSTOM_VAR", "custom-value") };
        let provider = EnvProvider::new().with_override("my_secret", "MY_CUSTOM_VAR");
        let secret = provider.resolve("my_secret").await.unwrap();
        assert_eq!(secret.encrypted_value, b"custom-value");
        unsafe { std::env::remove_var("MY_CUSTOM_VAR") };
    }

    #[tokio::test]
    async fn env_provider_not_found() {
        let provider = EnvProvider::new();
        assert!(provider.resolve("nonexistent_12345").await.is_err());
    }

    #[tokio::test]
    async fn chain_provider_fallback() {
        let (store, vault_prov) = make_vault_provider().await;
        store
            .store_secret("vault_only", SecretKind::ApiKey, b"from-vault", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        // SAFETY: test-only, single-threaded access to env vars.
        unsafe { std::env::set_var("DAF_SECRET_ENV_ONLY", "from-env") };

        let chain = ChainProvider::new()
            .with_provider(vault_prov)
            .with_provider(EnvProvider::new());

        // Found in vault.
        let s1 = chain.resolve("vault_only").await.unwrap();
        assert_eq!(s1.encrypted_value, b"from-vault");

        // Falls through to env.
        let s2 = chain.resolve("env_only").await.unwrap();
        assert_eq!(s2.encrypted_value, b"from-env");

        // Not found anywhere.
        assert!(chain.resolve("totally_missing").await.is_err());

        unsafe { std::env::remove_var("DAF_SECRET_ENV_ONLY") };
    }

    #[tokio::test]
    async fn template_interpolation() {
        let (store, vault_prov) = make_vault_provider().await;
        store
            .store_secret("api_key", SecretKind::ApiKey, b"sk-abc123", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let template = "Authorization: Bearer ${vault:api_key}";
        let result = resolve_template(template, &vault_prov).await.unwrap();
        assert_eq!(result, "Authorization: Bearer sk-abc123");
    }

    #[tokio::test]
    async fn template_multiple_refs() {
        let (store, vault_prov) = make_vault_provider().await;
        store
            .store_secret("host", SecretKind::Custom("config".into()), b"db.example.com", AccessPolicy::AnyAgent)
            .await
            .unwrap();
        store
            .store_secret("port", SecretKind::Custom("config".into()), b"5432", AccessPolicy::AnyAgent)
            .await
            .unwrap();

        let template = "postgres://${vault:host}:${vault:port}/mydb";
        let result = resolve_template(template, &vault_prov).await.unwrap();
        assert_eq!(result, "postgres://db.example.com:5432/mydb");
    }

    #[tokio::test]
    async fn template_no_refs() {
        let (_, vault_prov) = make_vault_provider().await;
        let template = "plain string no references";
        let result = resolve_template(template, &vault_prov).await.unwrap();
        assert_eq!(result, template);
    }

    #[tokio::test]
    async fn template_unclosed_ref_fails() {
        let (_, vault_prov) = make_vault_provider().await;
        let result = resolve_template("bad ${vault:unclosed", &vault_prov).await;
        assert!(result.is_err());
    }
}
