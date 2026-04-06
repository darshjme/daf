//! State management for provisioned resources.
//!
//! Analogous to Terraform's `terraform.tfstate`, the provision state tracks
//! every resource the system knows about — its spec, lifecycle state, outputs,
//! and physical ID. The state is persisted between runs so that the planner
//! can diff desired configuration against actual infrastructure.
//!
//! # State locking
//!
//! Concurrent provisioning against the same state is dangerous. The
//! [`StateStore`] trait requires implementations to support advisory locking
//! so that only one planner/applier can operate at a time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use daf_core::{DafError, DafResult};

use crate::resource::{Resource, ResourceSpec};

// ---------------------------------------------------------------------------
// ProvisionState
// ---------------------------------------------------------------------------

/// Complete snapshot of all known provisioned resources.
///
/// Every mutation bumps the `serial` counter, giving a total ordering of
/// state versions suitable for optimistic concurrency checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionState {
    /// Schema version for forward compatibility. Readers that encounter a
    /// version they don't understand should refuse to modify the state.
    pub version: u32,

    /// Monotonically increasing counter bumped on every write.
    pub serial: u64,

    /// All tracked resources keyed by logical name.
    pub resources: HashMap<String, Resource>,

    /// Top-level outputs exposed to the operator after a successful apply.
    pub outputs: HashMap<String, Value>,

    /// When this state snapshot was last written.
    pub last_modified: DateTime<Utc>,
}

/// Current state file schema version.
const STATE_VERSION: u32 = 1;

impl ProvisionState {
    /// Create a fresh empty state.
    pub fn new() -> Self {
        Self {
            version: STATE_VERSION,
            serial: 0,
            resources: HashMap::new(),
            outputs: HashMap::new(),
            last_modified: Utc::now(),
        }
    }

    /// Bump the serial and timestamp. Call before every persist.
    pub fn bump(&mut self) {
        self.serial += 1;
        self.last_modified = Utc::now();
    }

    /// Insert or update a resource in the state.
    pub fn upsert_resource(&mut self, resource: Resource) {
        self.resources.insert(resource.name().to_owned(), resource);
    }

    /// Remove a resource from the state (after successful destroy).
    pub fn remove_resource(&mut self, name: &str) -> Option<Resource> {
        self.resources.remove(name)
    }

    /// Look up a resource by logical name.
    pub fn get_resource(&self, name: &str) -> Option<&Resource> {
        self.resources.get(name)
    }

    /// Look up a resource mutably by logical name.
    pub fn get_resource_mut(&mut self, name: &str) -> Option<&mut Resource> {
        self.resources.get_mut(name)
    }

    /// Return all resources in the `Created` state.
    pub fn live_resources(&self) -> impl Iterator<Item = &Resource> {
        self.resources.values().filter(|r| r.state.is_live())
    }

    /// Compute a diff summary: which resources in `desired` are new, changed,
    /// or absent compared to `self`.
    pub fn diff(&self, desired: &[ResourceSpec]) -> StateDiff {
        let mut diff = StateDiff::default();

        for spec in desired {
            match self.resources.get(&spec.name) {
                None => {
                    diff.additions.push(spec.name.clone());
                }
                Some(existing) => {
                    // Compare the config blobs. If they differ, mark as changed.
                    if existing.spec.config != spec.config
                        || existing.spec.type_ != spec.type_
                        || existing.spec.provider != spec.provider
                    {
                        diff.changes.push(spec.name.clone());
                    }
                }
            }
        }

        let desired_names: std::collections::HashSet<&str> =
            desired.iter().map(|s| s.name.as_str()).collect();

        for name in self.resources.keys() {
            if !desired_names.contains(name.as_str()) {
                diff.deletions.push(name.clone());
            }
        }

        diff
    }
}

impl Default for ProvisionState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// StateDiff
// ---------------------------------------------------------------------------

/// Summary of differences between current state and desired configuration.
#[derive(Debug, Default, Clone)]
pub struct StateDiff {
    /// Resource names that exist in desired but not in current state.
    pub additions: Vec<String>,
    /// Resource names that exist in both but differ in configuration.
    pub changes: Vec<String>,
    /// Resource names that exist in current state but not in desired.
    pub deletions: Vec<String>,
}

impl StateDiff {
    /// Returns `true` if there are no differences.
    pub fn is_empty(&self) -> bool {
        self.additions.is_empty() && self.changes.is_empty() && self.deletions.is_empty()
    }

    /// Total number of affected resources.
    pub fn total(&self) -> usize {
        self.additions.len() + self.changes.len() + self.deletions.len()
    }
}

// ---------------------------------------------------------------------------
// StateStore trait
// ---------------------------------------------------------------------------

/// Persistent storage backend for provision state.
///
/// Implementations must handle serialization, locking, and versioning.
/// The lock/unlock methods provide advisory mutual exclusion — they do not
/// guarantee OS-level file locking across processes (though `FileStateStore`
/// does attempt it).
#[async_trait::async_trait]
pub trait StateStore: Send + Sync + 'static {
    /// Load the current state from the backend. Returns a fresh empty state
    /// if none exists yet.
    async fn load(&self) -> DafResult<ProvisionState>;

    /// Persist the given state, bumping its serial first.
    async fn save(&self, state: &mut ProvisionState) -> DafResult<()>;

    /// Acquire an advisory lock. Returns an error if the lock is already held
    /// by another process or thread.
    async fn lock(&self) -> DafResult<()>;

    /// Release the advisory lock.
    async fn unlock(&self) -> DafResult<()>;
}

// ---------------------------------------------------------------------------
// FileStateStore
// ---------------------------------------------------------------------------

/// Stores provision state as a JSON file on the local filesystem.
///
/// File locking is implemented with a `.lock` sidecar file. This is advisory
/// and best-effort — it prevents accidental concurrent runs but is not
/// foolproof against hostile actors.
pub struct FileStateStore {
    path: PathBuf,
    lock_path: PathBuf,
    locked: Arc<Mutex<bool>>,
}

impl FileStateStore {
    /// Create a store backed by the given file path.
    ///
    /// The lock file will be placed alongside the state file with a `.lock`
    /// extension.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock_path = path.with_extension("tfstate.lock");
        Self {
            path,
            lock_path,
            locked: Arc::new(Mutex::new(false)),
        }
    }

    /// Return the path to the state file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait::async_trait]
impl StateStore for FileStateStore {
    async fn load(&self) -> DafResult<ProvisionState> {
        if !self.path.exists() {
            debug!(path = %self.path.display(), "no state file found, starting fresh");
            return Ok(ProvisionState::new());
        }

        let data = tokio::fs::read_to_string(&self.path).await.map_err(|e| {
            DafError::Internal(format!(
                "failed to read state file {}: {e}",
                self.path.display()
            ))
        })?;

        let state: ProvisionState = serde_json::from_str(&data).map_err(|e| {
            DafError::SerializationError(format!(
                "failed to parse state file {}: {e}",
                self.path.display()
            ))
        })?;

        if state.version > STATE_VERSION {
            return Err(DafError::ConfigError(format!(
                "state file version {} is newer than supported version {STATE_VERSION}",
                state.version,
            )));
        }

        debug!(
            path = %self.path.display(),
            serial = state.serial,
            resources = state.resources.len(),
            "loaded provision state"
        );

        Ok(state)
    }

    async fn save(&self, state: &mut ProvisionState) -> DafResult<()> {
        state.bump();

        let json = serde_json::to_string_pretty(state)
            .map_err(|e| DafError::SerializationError(format!("failed to serialize state: {e}")))?;

        // Write to a temp file first, then rename for atomicity.
        let tmp_path = self.path.with_extension("tfstate.tmp");

        tokio::fs::write(&tmp_path, &json).await.map_err(|e| {
            DafError::Internal(format!(
                "failed to write temp state file {}: {e}",
                tmp_path.display()
            ))
        })?;

        tokio::fs::rename(&tmp_path, &self.path)
            .await
            .map_err(|e| {
                DafError::Internal(format!(
                    "failed to rename state file {} -> {}: {e}",
                    tmp_path.display(),
                    self.path.display()
                ))
            })?;

        debug!(
            path = %self.path.display(),
            serial = state.serial,
            "saved provision state"
        );

        Ok(())
    }

    async fn lock(&self) -> DafResult<()> {
        let mut locked = self.locked.lock().await;
        if *locked {
            return Err(DafError::Internal(
                "state is already locked by this process".into(),
            ));
        }

        // Attempt to create the lock file exclusively.
        if self.lock_path.exists() {
            // Check if the lock is stale (older than 10 minutes).
            if let Ok(metadata) = tokio::fs::metadata(&self.lock_path).await {
                if let Ok(modified) = metadata.modified() {
                    let age = modified.elapsed().unwrap_or_default();
                    if age.as_secs() > 600 {
                        warn!(
                            lock_path = %self.lock_path.display(),
                            age_secs = age.as_secs(),
                            "removing stale lock file"
                        );
                        let _ = tokio::fs::remove_file(&self.lock_path).await;
                    } else {
                        return Err(DafError::Internal(format!(
                            "state is locked by another process (lock file: {})",
                            self.lock_path.display()
                        )));
                    }
                }
            }
        }

        tokio::fs::write(&self.lock_path, format!("pid:{}", std::process::id()))
            .await
            .map_err(|e| {
                DafError::Internal(format!(
                    "failed to create lock file {}: {e}",
                    self.lock_path.display()
                ))
            })?;

        *locked = true;
        debug!(lock_path = %self.lock_path.display(), "acquired state lock");
        Ok(())
    }

    async fn unlock(&self) -> DafResult<()> {
        let mut locked = self.locked.lock().await;
        if !*locked {
            return Ok(());
        }

        let _ = tokio::fs::remove_file(&self.lock_path).await;
        *locked = false;
        debug!(lock_path = %self.lock_path.display(), "released state lock");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// InMemoryStateStore
// ---------------------------------------------------------------------------

/// In-memory state store for testing. State is lost when the store is dropped.
pub struct InMemoryStateStore {
    state: Arc<Mutex<ProvisionState>>,
    locked: Arc<Mutex<bool>>,
}

impl InMemoryStateStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ProvisionState::new())),
            locked: Arc::new(Mutex::new(false)),
        }
    }

    /// Create a store pre-loaded with the given state.
    pub fn with_state(state: ProvisionState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            locked: Arc::new(Mutex::new(false)),
        }
    }
}

impl Default for InMemoryStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl StateStore for InMemoryStateStore {
    async fn load(&self) -> DafResult<ProvisionState> {
        Ok(self.state.lock().await.clone())
    }

    async fn save(&self, state: &mut ProvisionState) -> DafResult<()> {
        state.bump();
        *self.state.lock().await = state.clone();
        Ok(())
    }

    async fn lock(&self) -> DafResult<()> {
        let mut locked = self.locked.lock().await;
        if *locked {
            return Err(DafError::Internal("already locked".into()));
        }
        *locked = true;
        Ok(())
    }

    async fn unlock(&self) -> DafResult<()> {
        *self.locked.lock().await = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceType;

    fn sample_spec(name: &str) -> ResourceSpec {
        ResourceSpec::new(ResourceType::AgentPool, name, "agent_pool")
            .with_config(serde_json::json!({"count": 3}))
    }

    #[test]
    fn state_new_is_empty() {
        let state = ProvisionState::new();
        assert_eq!(state.version, STATE_VERSION);
        assert_eq!(state.serial, 0);
        assert!(state.resources.is_empty());
    }

    #[test]
    fn state_bump_increments() {
        let mut state = ProvisionState::new();
        state.bump();
        assert_eq!(state.serial, 1);
        state.bump();
        assert_eq!(state.serial, 2);
    }

    #[test]
    fn state_upsert_and_lookup() {
        let mut state = ProvisionState::new();
        let spec = sample_spec("workers");
        let resource = Resource::from_spec(spec);
        state.upsert_resource(resource);

        assert!(state.get_resource("workers").is_some());
        assert!(state.get_resource("nonexistent").is_none());
    }

    #[test]
    fn state_remove_resource() {
        let mut state = ProvisionState::new();
        let spec = sample_spec("temp");
        state.upsert_resource(Resource::from_spec(spec));

        let removed = state.remove_resource("temp");
        assert!(removed.is_some());
        assert!(state.get_resource("temp").is_none());
    }

    #[test]
    fn state_diff_additions() {
        let state = ProvisionState::new();
        let desired = vec![sample_spec("new-pool")];
        let diff = state.diff(&desired);

        assert_eq!(diff.additions, vec!["new-pool"]);
        assert!(diff.changes.is_empty());
        assert!(diff.deletions.is_empty());
    }

    #[test]
    fn state_diff_deletions() {
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(sample_spec("old")));
        let desired: Vec<ResourceSpec> = vec![];
        let diff = state.diff(&desired);

        assert!(diff.additions.is_empty());
        assert_eq!(diff.deletions, vec!["old"]);
    }

    #[test]
    fn state_diff_changes() {
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(sample_spec("pool")));

        let mut changed = sample_spec("pool");
        changed.config = serde_json::json!({"count": 10}); // different count
        let diff = state.diff(&[changed]);

        assert_eq!(diff.changes, vec!["pool"]);
        assert!(diff.additions.is_empty());
        assert!(diff.deletions.is_empty());
    }

    #[test]
    fn state_diff_empty_when_matching() {
        let mut state = ProvisionState::new();
        let spec = sample_spec("stable");
        state.upsert_resource(Resource::from_spec(spec.clone()));
        let diff = state.diff(&[spec]);
        assert!(diff.is_empty());
    }

    #[tokio::test]
    async fn in_memory_store_round_trip() {
        let store = InMemoryStateStore::new();

        let mut state = store.load().await.unwrap();
        assert_eq!(state.serial, 0);

        state.upsert_resource(Resource::from_spec(sample_spec("test")));
        store.save(&mut state).await.unwrap();

        let reloaded = store.load().await.unwrap();
        assert_eq!(reloaded.serial, 1);
        assert!(reloaded.get_resource("test").is_some());
    }

    #[tokio::test]
    async fn in_memory_store_locking() {
        let store = InMemoryStateStore::new();
        store.lock().await.unwrap();

        // Second lock should fail.
        assert!(store.lock().await.is_err());

        store.unlock().await.unwrap();

        // Now it should succeed.
        store.lock().await.unwrap();
    }

    #[tokio::test]
    async fn file_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.tfstate");
        let store = FileStateStore::new(&path);

        let mut state = store.load().await.unwrap();
        assert_eq!(state.serial, 0);

        state.upsert_resource(Resource::from_spec(sample_spec("file-test")));
        store.save(&mut state).await.unwrap();

        // Reload from disk.
        let store2 = FileStateStore::new(&path);
        let reloaded = store2.load().await.unwrap();
        assert_eq!(reloaded.serial, 1);
        assert!(reloaded.get_resource("file-test").is_some());
    }

    #[test]
    fn state_serde_roundtrip() {
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(sample_spec("serde-test")));
        state
            .outputs
            .insert("url".into(), Value::String("https://example.com".into()));

        let json = serde_json::to_string(&state).unwrap();
        let back: ProvisionState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.resources.len(), 1);
        assert_eq!(back.outputs["url"], "https://example.com");
    }

    #[test]
    fn live_resources_filter() {
        let mut state = ProvisionState::new();

        let mut live = Resource::from_spec(sample_spec("live"));
        live.transition(crate::resource::ResourceState::Created);

        let planned = Resource::from_spec(sample_spec("planned"));

        state.upsert_resource(live);
        state.upsert_resource(planned);

        let live_count = state.live_resources().count();
        assert_eq!(live_count, 1);
    }
}
