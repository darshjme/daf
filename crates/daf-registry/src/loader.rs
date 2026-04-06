//! Dynamic agent loading and manifest management.
//!
//! Provides the [`AgentLoader`] trait for pluggable loading strategies, plus
//! concrete implementations for loading agent manifests from individual files
//! ([`ManifestLoader`]) and scanning directories ([`DirectoryLoader`]).
//! Includes manifest validation and hot-reload via filesystem watching.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use daf_core::{AgentKind, AgentManifest, DafError, DafResult};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// AgentLoader trait
// ---------------------------------------------------------------------------

/// Trait for loading agent manifests from external sources.
///
/// Implementations might load from local files, remote registries, container
/// images, or in-memory definitions. The registry calls into loaders to
/// discover available agents at startup and during hot-reload.
#[async_trait::async_trait]
pub trait AgentLoader: Send + Sync {
    /// Load all available agent manifests.
    async fn load(&self) -> DafResult<Vec<AgentManifest>>;

    /// Unload (clean up) resources associated with a loaded manifest.
    async fn unload(&self, name: &str) -> DafResult<()>;

    /// Reload a specific manifest by name. Returns the updated manifest.
    async fn reload(&self, name: &str) -> DafResult<AgentManifest>;

    /// A human-readable name for this loader (for diagnostics).
    fn loader_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// ManifestFile: on-disk format
// ---------------------------------------------------------------------------

/// The on-disk representation of an agent manifest (JSON or YAML-like).
///
/// This is a simplified format that users write; the loader converts it
/// to a full [`AgentManifest`] with generated IDs and defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Agent name (required).
    pub name: String,
    /// Agent kind (required).
    pub kind: AgentKind,
    /// Semantic version of the agent.
    #[serde(default = "default_version")]
    pub version: String,
    /// List of capability declarations.
    #[serde(default)]
    pub capabilities: Vec<CapabilityDecl>,
    /// Arbitrary metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// Optional description.
    pub description: Option<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// A capability declaration in a manifest file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDecl {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validation errors found in a manifest file.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ValidationError {
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("invalid field value: {field} — {reason}")]
    InvalidField { field: String, reason: String },
    #[error("duplicate capability: {0}")]
    DuplicateCapability(String),
}

/// Validate a manifest file for completeness and correctness.
pub fn validate_manifest(manifest: &ManifestFile) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if manifest.name.is_empty() {
        errors.push(ValidationError::MissingField("name".into()));
    }

    if manifest.name.contains(char::is_whitespace) {
        errors.push(ValidationError::InvalidField {
            field: "name".into(),
            reason: "must not contain whitespace".into(),
        });
    }

    // Check for duplicate capability names.
    let mut seen_caps = std::collections::HashSet::new();
    for cap in &manifest.capabilities {
        if !seen_caps.insert(&cap.name) {
            errors.push(ValidationError::DuplicateCapability(cap.name.clone()));
        }
        if cap.name.is_empty() {
            errors.push(ValidationError::MissingField(
                "capabilities[].name".into(),
            ));
        }
    }

    // Validate version is parseable.
    if crate::version::SemVer::from_str_checked(&manifest.version).is_err() {
        errors.push(ValidationError::InvalidField {
            field: "version".into(),
            reason: format!("'{}' is not a valid semver", manifest.version),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Convert a validated [`ManifestFile`] into a full [`AgentManifest`].
pub fn manifest_from_file(file: &ManifestFile) -> AgentManifest {
    let mut manifest = AgentManifest::new(file.kind, &file.name);
    manifest.capabilities = file
        .capabilities
        .iter()
        .map(|c| daf_core::AgentCapability::new(&c.name, &c.version, &c.description))
        .collect();
    manifest.metadata = file.metadata.clone();
    if let Some(ref desc) = file.description {
        manifest.metadata.insert("description".into(), desc.clone());
    }
    manifest
}

// ---------------------------------------------------------------------------
// ManifestLoader
// ---------------------------------------------------------------------------

/// Loads agent manifests from individual JSON files.
pub struct ManifestLoader {
    /// Paths to manifest files.
    paths: Vec<PathBuf>,
}

impl ManifestLoader {
    /// Create a loader for the given file paths.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    /// Add a file path.
    pub fn add_path(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    /// Load a single manifest file.
    fn load_file(path: &Path) -> DafResult<AgentManifest> {
        let content = std::fs::read_to_string(path)?;
        let file: ManifestFile = serde_json::from_str(&content).map_err(|e| {
            DafError::ConfigError(format!("failed to parse {}: {e}", path.display()))
        })?;

        validate_manifest(&file).map_err(|errors| {
            let msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            DafError::ConfigError(format!(
                "validation failed for {}: {}",
                path.display(),
                msgs.join("; ")
            ))
        })?;

        Ok(manifest_from_file(&file))
    }
}

#[async_trait::async_trait]
impl AgentLoader for ManifestLoader {
    async fn load(&self) -> DafResult<Vec<AgentManifest>> {
        let mut manifests = Vec::new();
        for path in &self.paths {
            match Self::load_file(path) {
                Ok(m) => {
                    info!(path = %path.display(), name = %m.name, "loaded manifest");
                    manifests.push(m);
                }
                Err(e) => {
                    error!(path = %path.display(), error = %e, "failed to load manifest");
                    return Err(e);
                }
            }
        }
        Ok(manifests)
    }

    async fn unload(&self, name: &str) -> DafResult<()> {
        debug!(agent_name = %name, "manifest loader: unload requested (no-op for file loader)");
        Ok(())
    }

    async fn reload(&self, name: &str) -> DafResult<AgentManifest> {
        for path in &self.paths {
            if let Ok(m) = Self::load_file(path) {
                if m.name == name {
                    info!(path = %path.display(), name = %name, "reloaded manifest");
                    return Ok(m);
                }
            }
        }
        Err(DafError::NotFound {
            entity: "manifest".into(),
            id: name.into(),
        })
    }

    fn loader_name(&self) -> &str {
        "manifest-file-loader"
    }
}

// ---------------------------------------------------------------------------
// DirectoryLoader
// ---------------------------------------------------------------------------

/// Scans a directory for agent manifest files (`.json`).
///
/// On load, reads every `.json` file in the directory, validates it, and
/// converts it to an [`AgentManifest`]. Supports hot-reload by re-scanning.
pub struct DirectoryLoader {
    /// The directory to scan.
    dir: PathBuf,
    /// Whether to scan subdirectories recursively.
    recursive: bool,
}

impl DirectoryLoader {
    /// Create a new directory loader.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            recursive: false,
        }
    }

    /// Enable recursive scanning of subdirectories.
    pub fn recursive(mut self, yes: bool) -> Self {
        self.recursive = yes;
        self
    }

    /// Collect all `.json` manifest files in the directory.
    fn collect_files(&self) -> DafResult<Vec<PathBuf>> {
        if !self.dir.exists() {
            return Err(DafError::ConfigError(format!(
                "manifest directory does not exist: {}",
                self.dir.display()
            )));
        }

        let mut files = Vec::new();
        self.walk_dir(&self.dir, &mut files)?;
        files.sort();
        Ok(files)
    }

    fn walk_dir(&self, dir: &Path, files: &mut Vec<PathBuf>) -> DafResult<()> {
        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && self.recursive {
                self.walk_dir(&path, files)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl AgentLoader for DirectoryLoader {
    async fn load(&self) -> DafResult<Vec<AgentManifest>> {
        let files = self.collect_files()?;
        info!(
            dir = %self.dir.display(),
            count = files.len(),
            "scanning directory for manifests"
        );

        let mut manifests = Vec::new();
        let mut errors = Vec::new();

        for path in &files {
            match ManifestLoader::load_file(path) {
                Ok(m) => {
                    debug!(path = %path.display(), name = %m.name, "loaded manifest");
                    manifests.push(m);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping invalid manifest");
                    errors.push(e);
                }
            }
        }

        if manifests.is_empty() && !errors.is_empty() {
            return Err(DafError::ConfigError(format!(
                "no valid manifests found in {}; {} errors",
                self.dir.display(),
                errors.len()
            )));
        }

        Ok(manifests)
    }

    async fn unload(&self, name: &str) -> DafResult<()> {
        debug!(agent_name = %name, "directory loader: unload (no-op)");
        Ok(())
    }

    async fn reload(&self, name: &str) -> DafResult<AgentManifest> {
        let files = self.collect_files()?;
        for path in &files {
            if let Ok(m) = ManifestLoader::load_file(path) {
                if m.name == name {
                    info!(path = %path.display(), name = %name, "reloaded manifest from directory");
                    return Ok(m);
                }
            }
        }
        Err(DafError::NotFound {
            entity: "manifest".into(),
            id: name.into(),
        })
    }

    fn loader_name(&self) -> &str {
        "directory-loader"
    }
}

// ---------------------------------------------------------------------------
// Hot-reload support
// ---------------------------------------------------------------------------

/// Represents a detected change in a manifest file.
#[derive(Debug, Clone)]
pub enum ManifestChange {
    /// A new manifest file was added.
    Added(PathBuf),
    /// An existing manifest file was modified.
    Modified(PathBuf),
    /// A manifest file was removed.
    Removed(PathBuf),
}

/// Tracks file checksums to detect changes for hot-reload.
#[derive(Debug)]
pub struct ChangeDetector {
    /// Map of file path to its blake3 hash.
    checksums: HashMap<PathBuf, String>,
}

impl ChangeDetector {
    /// Create a new change detector.
    pub fn new() -> Self {
        Self {
            checksums: HashMap::new(),
        }
    }

    /// Compute the blake3 hash of a file's contents.
    fn hash_file(path: &Path) -> DafResult<String> {
        let content = std::fs::read(path)?;
        let hash = blake3::hash(&content);
        Ok(hash.to_hex().to_string())
    }

    /// Scan a list of files and return any changes since the last scan.
    pub fn detect_changes(&mut self, current_files: &[PathBuf]) -> DafResult<Vec<ManifestChange>> {
        let mut changes = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for path in current_files {
            seen.insert(path.clone());
            let hash = Self::hash_file(path)?;

            match self.checksums.get(path) {
                None => {
                    // New file.
                    changes.push(ManifestChange::Added(path.clone()));
                    self.checksums.insert(path.clone(), hash);
                }
                Some(prev) if *prev != hash => {
                    // Modified file.
                    changes.push(ManifestChange::Modified(path.clone()));
                    self.checksums.insert(path.clone(), hash);
                }
                _ => {} // Unchanged.
            }
        }

        // Detect removals.
        let removed: Vec<PathBuf> = self
            .checksums
            .keys()
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
        for path in removed {
            self.checksums.remove(&path);
            changes.push(ManifestChange::Removed(path));
        }

        Ok(changes)
    }

    /// Current number of tracked files.
    pub fn tracked_count(&self) -> usize {
        self.checksums.len()
    }
}

impl Default for ChangeDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SemVer helper (validation-only parse)
// ---------------------------------------------------------------------------

impl crate::version::SemVer {
    /// Try to parse a version string, returning an error if invalid.
    /// This is used for validation only — does not allocate a full SemVer
    /// unless needed.
    pub fn from_str_checked(s: &str) -> Result<Self, crate::version::VersionParseError> {
        s.parse()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest_json() -> String {
        serde_json::json!({
            "name": "test-agent",
            "kind": "worker",
            "version": "1.0.0",
            "capabilities": [
                {"name": "lint", "version": "1.0.0", "description": "Lint code"}
            ],
            "metadata": {"team": "platform"},
            "description": "A test agent"
        })
        .to_string()
    }

    #[test]
    fn parse_manifest_file() {
        let json = sample_manifest_json();
        let file: ManifestFile = serde_json::from_str(&json).unwrap();
        assert_eq!(file.name, "test-agent");
        assert_eq!(file.kind, AgentKind::Worker);
        assert_eq!(file.capabilities.len(), 1);
    }

    #[test]
    fn validate_valid_manifest() {
        let json = sample_manifest_json();
        let file: ManifestFile = serde_json::from_str(&json).unwrap();
        assert!(validate_manifest(&file).is_ok());
    }

    #[test]
    fn validate_empty_name() {
        let file = ManifestFile {
            name: "".into(),
            kind: AgentKind::Worker,
            version: "1.0.0".into(),
            capabilities: Vec::new(),
            metadata: HashMap::new(),
            description: None,
        };
        let errors = validate_manifest(&file).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ValidationError::MissingField(_))));
    }

    #[test]
    fn validate_whitespace_in_name() {
        let file = ManifestFile {
            name: "bad name".into(),
            kind: AgentKind::Worker,
            version: "1.0.0".into(),
            capabilities: Vec::new(),
            metadata: HashMap::new(),
            description: None,
        };
        let errors = validate_manifest(&file).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidField { .. })));
    }

    #[test]
    fn validate_duplicate_capability() {
        let file = ManifestFile {
            name: "test".into(),
            kind: AgentKind::Worker,
            version: "1.0.0".into(),
            capabilities: vec![
                CapabilityDecl {
                    name: "lint".into(),
                    version: "1.0.0".into(),
                    description: "".into(),
                },
                CapabilityDecl {
                    name: "lint".into(),
                    version: "2.0.0".into(),
                    description: "".into(),
                },
            ],
            metadata: HashMap::new(),
            description: None,
        };
        let errors = validate_manifest(&file).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ValidationError::DuplicateCapability(_))));
    }

    #[test]
    fn manifest_from_file_conversion() {
        let json = sample_manifest_json();
        let file: ManifestFile = serde_json::from_str(&json).unwrap();
        let manifest = manifest_from_file(&file);

        assert_eq!(manifest.name, "test-agent");
        assert_eq!(manifest.kind, AgentKind::Worker);
        assert!(manifest.has_capability("lint"));
        assert_eq!(manifest.metadata.get("team").unwrap(), "platform");
        assert_eq!(manifest.metadata.get("description").unwrap(), "A test agent");
    }

    #[tokio::test]
    async fn manifest_loader_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, sample_manifest_json()).unwrap();

        let loader = ManifestLoader::new(vec![path]);
        let manifests = loader.load().await.unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "test-agent");
    }

    #[tokio::test]
    async fn directory_loader_scans() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), sample_manifest_json()).unwrap();

        // Write a second manifest with a different name.
        let m2 = serde_json::json!({
            "name": "agent-two",
            "kind": "specialist",
            "version": "2.0.0",
            "capabilities": []
        });
        std::fs::write(dir.path().join("b.json"), m2.to_string()).unwrap();

        // Non-json file should be ignored.
        std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();

        let loader = DirectoryLoader::new(dir.path());
        let manifests = loader.load().await.unwrap();
        assert_eq!(manifests.len(), 2);
    }

    #[tokio::test]
    async fn directory_loader_reload() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.json"), sample_manifest_json()).unwrap();

        let loader = DirectoryLoader::new(dir.path());
        let m = loader.reload("test-agent").await.unwrap();
        assert_eq!(m.name, "test-agent");

        let err = loader.reload("nonexistent").await;
        assert!(err.is_err());
    }

    #[test]
    fn change_detector_initial_scan() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.json");
        let p2 = dir.path().join("b.json");
        std::fs::write(&p1, "{}").unwrap();
        std::fs::write(&p2, "{}").unwrap();

        let mut detector = ChangeDetector::new();
        let changes = detector.detect_changes(&[p1.clone(), p2.clone()]).unwrap();
        assert_eq!(changes.len(), 2); // Both are "Added" on first scan.
        assert_eq!(detector.tracked_count(), 2);
    }

    #[test]
    fn change_detector_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        std::fs::write(&path, "v1").unwrap();

        let mut detector = ChangeDetector::new();
        detector.detect_changes(&[path.clone()]).unwrap();

        // No changes on second scan.
        let changes = detector.detect_changes(&[path.clone()]).unwrap();
        assert!(changes.is_empty());

        // Modify the file.
        std::fs::write(&path, "v2").unwrap();
        let changes = detector.detect_changes(&[path.clone()]).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], ManifestChange::Modified(_)));
    }

    #[test]
    fn change_detector_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        std::fs::write(&path, "{}").unwrap();

        let mut detector = ChangeDetector::new();
        detector.detect_changes(&[path.clone()]).unwrap();

        // File "removed" (not in the list anymore).
        let changes = detector.detect_changes(&[]).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], ManifestChange::Removed(_)));
        assert_eq!(detector.tracked_count(), 0);
    }
}
