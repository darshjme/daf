//! Persistent agent catalog.
//!
//! While the [`Registry`](crate::registry::Registry) tracks *live* agent
//! instances, the catalog stores *known agent types* — their manifests,
//! performance statistics, and pre-defined templates. Think of it as the
//! package index: agents come and go, but the catalog remembers what
//! types of agents exist and how well they perform.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use daf_core::{AgentCapability, AgentKind, AgentManifest};
use serde::{Deserialize, Serialize};

use crate::capability::{Capability, CapabilitySet};
use crate::version::SemVer;

// ---------------------------------------------------------------------------
// CatalogEntry
// ---------------------------------------------------------------------------

/// A catalog entry describing a known agent type with runtime statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// The canonical manifest for this agent type.
    pub manifest: AgentManifest,
    /// Registry-level capabilities (richer than the core manifest's).
    pub capabilities: CapabilitySet,
    /// When this entry was first registered in the catalog.
    pub registered_at: DateTime<Utc>,
    /// When this entry was last updated.
    pub updated_at: DateTime<Utc>,
    /// How many live instances of this agent type currently exist.
    pub instance_count: u32,
    /// Rolling average task duration across all instances.
    pub avg_task_duration: Option<Duration>,
    /// Rolling success rate (0.0..=1.0) across all instances.
    pub success_rate: Option<f64>,
    /// Arbitrary tags for filtering and organization.
    pub tags: Vec<String>,
}

impl CatalogEntry {
    /// Create a new catalog entry from a manifest.
    pub fn new(manifest: AgentManifest) -> Self {
        let capabilities = capabilities_from_manifest(&manifest);
        Self {
            manifest,
            capabilities,
            registered_at: Utc::now(),
            updated_at: Utc::now(),
            instance_count: 0,
            avg_task_duration: None,
            success_rate: None,
            tags: Vec::new(),
        }
    }

    /// Update runtime statistics.
    pub fn update_stats(&mut self, task_duration: Duration, succeeded: bool) {
        // Rolling average for duration.
        let new_dur = task_duration;
        self.avg_task_duration = Some(match self.avg_task_duration {
            Some(prev) => {
                let alpha = 0.1; // exponential moving average
                let prev_ms = prev.as_millis() as f64;
                let new_ms = new_dur.as_millis() as f64;
                let ema = prev_ms * (1.0 - alpha) + new_ms * alpha;
                Duration::from_millis(ema as u64)
            }
            None => new_dur,
        });

        // Rolling success rate.
        let result = if succeeded { 1.0 } else { 0.0 };
        self.success_rate = Some(match self.success_rate {
            Some(prev) => {
                let alpha = 0.1;
                prev * (1.0 - alpha) + result * alpha
            }
            None => result,
        });

        self.updated_at = Utc::now();
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
}

/// Convert core manifest capabilities to registry-level capabilities.
fn capabilities_from_manifest(manifest: &AgentManifest) -> CapabilitySet {
    let caps = manifest.capabilities.iter().map(|c| {
        let version = c.version.parse::<SemVer>().unwrap_or(SemVer::new(0, 0, 0));
        Capability::new(&c.name, version, &c.description)
    });
    CapabilitySet::from_capabilities(caps)
}

// ---------------------------------------------------------------------------
// AgentTemplate
// ---------------------------------------------------------------------------

/// A pre-defined agent configuration template.
///
/// Templates capture common patterns — "code reviewer", "security scanner",
/// "deployment agent" — so new agents can be spawned from known-good configs
/// without writing a manifest from scratch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTemplate {
    /// Template name (e.g. `"code-reviewer"`, `"security-scanner"`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// The kind of agent this template creates.
    pub kind: AgentKind,
    /// Default capabilities the agent will advertise.
    pub capabilities: Vec<AgentCapability>,
    /// Default metadata entries.
    pub metadata: HashMap<String, String>,
    /// Tags inherited by agents spawned from this template.
    pub tags: Vec<String>,
}

impl AgentTemplate {
    /// Create a new template.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        kind: AgentKind,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            kind,
            capabilities: Vec::new(),
            metadata: HashMap::new(),
            tags: Vec::new(),
        }
    }

    /// Add a capability to the template.
    pub fn with_capability(mut self, cap: AgentCapability) -> Self {
        self.capabilities.push(cap);
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Create an [`AgentManifest`] from this template.
    pub fn instantiate(&self) -> AgentManifest {
        let mut manifest = AgentManifest::new(self.kind, &self.name);
        manifest.capabilities = self.capabilities.clone();
        manifest.metadata = self.metadata.clone();
        manifest
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Persistent registry of known agent types.
///
/// The catalog stores agent type definitions, templates, and performance
/// statistics. It survives registry restarts and can be imported/exported
/// as JSON for backup and migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    /// Known agent types, keyed by agent name.
    entries: HashMap<String, CatalogEntry>,
    /// Pre-defined templates, keyed by template name.
    templates: HashMap<String, AgentTemplate>,
}

impl Catalog {
    /// Create an empty catalog.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            templates: HashMap::new(),
        }
    }

    /// Add or update a catalog entry.
    pub fn upsert(&mut self, entry: CatalogEntry) {
        let name = entry.manifest.name.clone();
        self.entries.insert(name, entry);
    }

    /// Remove an entry by agent name.
    pub fn remove(&mut self, name: &str) -> Option<CatalogEntry> {
        self.entries.remove(name)
    }

    /// Look up an entry by agent name.
    pub fn get(&self, name: &str) -> Option<&CatalogEntry> {
        self.entries.get(name)
    }

    /// Get a mutable reference to an entry.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut CatalogEntry> {
        self.entries.get_mut(name)
    }

    /// List all entries, optionally filtered by kind.
    pub fn list(&self, kind_filter: Option<AgentKind>) -> Vec<&CatalogEntry> {
        self.entries
            .values()
            .filter(|e| match kind_filter {
                Some(kind) => e.manifest.kind == kind,
                None => true,
            })
            .collect()
    }

    /// Number of entries in the catalog.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // -- Templates --

    /// Register a template.
    pub fn add_template(&mut self, template: AgentTemplate) {
        self.templates.insert(template.name.clone(), template);
    }

    /// Look up a template by name.
    pub fn get_template(&self, name: &str) -> Option<&AgentTemplate> {
        self.templates.get(name)
    }

    /// List all templates.
    pub fn list_templates(&self) -> Vec<&AgentTemplate> {
        self.templates.values().collect()
    }

    // -- Import / Export --

    /// Export the entire catalog as JSON bytes.
    pub fn export_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    /// Import a catalog from JSON bytes, replacing the current contents.
    pub fn import_json(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }

    /// Merge another catalog into this one. Existing entries are overwritten
    /// by the incoming catalog on conflict.
    pub fn merge(&mut self, other: Catalog) {
        for (name, entry) in other.entries {
            self.entries.insert(name, entry);
        }
        for (name, template) in other.templates {
            self.templates.insert(name, template);
        }
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in templates
// ---------------------------------------------------------------------------

/// Create a set of built-in agent templates for common roles.
pub fn builtin_templates() -> Vec<AgentTemplate> {
    vec![
        AgentTemplate::new(
            "code-reviewer",
            "Reviews code for correctness, style, and security issues",
            AgentKind::Specialist,
        )
        .with_capability(AgentCapability::new("code_review", "1.0.0", "Static code analysis"))
        .with_capability(AgentCapability::new("security_scan", "1.0.0", "OWASP vulnerability detection")),
        AgentTemplate::new(
            "test-runner",
            "Executes test suites and reports results",
            AgentKind::Worker,
        )
        .with_capability(AgentCapability::new("test_execution", "1.0.0", "Run test suites"))
        .with_capability(AgentCapability::new("coverage_report", "1.0.0", "Generate coverage reports")),
        AgentTemplate::new(
            "deployer",
            "Handles deployment to staging and production environments",
            AgentKind::Specialist,
        )
        .with_capability(AgentCapability::new("deploy", "1.0.0", "Deploy artifacts"))
        .with_capability(AgentCapability::new("rollback", "1.0.0", "Rollback to previous version")),
        AgentTemplate::new(
            "health-monitor",
            "Watches cluster health and emits alerts",
            AgentKind::Monitor,
        )
        .with_capability(AgentCapability::new("health_check", "1.0.0", "Periodic health probes"))
        .with_capability(AgentCapability::new("alerting", "1.0.0", "Send alerts on degradation")),
        AgentTemplate::new(
            "orchestrator",
            "Top-level coordinator that decomposes goals into tasks",
            AgentKind::Orchestrator,
        )
        .with_capability(AgentCapability::new("task_decomposition", "1.0.0", "Break goals into subtasks"))
        .with_capability(AgentCapability::new("delegation", "1.0.0", "Assign tasks to specialists")),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest(name: &str, kind: AgentKind) -> AgentManifest {
        AgentManifest::new(kind, name)
            .with_capability(AgentCapability::new("test_cap", "1.0.0", "Test capability"))
    }

    #[test]
    fn catalog_entry_creation() {
        let manifest = test_manifest("test-agent", AgentKind::Worker);
        let entry = CatalogEntry::new(manifest);
        assert_eq!(entry.instance_count, 0);
        assert!(entry.avg_task_duration.is_none());
        assert!(entry.success_rate.is_none());
    }

    #[test]
    fn catalog_entry_stats_update() {
        let manifest = test_manifest("test-agent", AgentKind::Worker);
        let mut entry = CatalogEntry::new(manifest);

        entry.update_stats(Duration::from_millis(100), true);
        assert!(entry.avg_task_duration.is_some());
        assert!((entry.success_rate.unwrap() - 1.0).abs() < f64::EPSILON);

        entry.update_stats(Duration::from_millis(200), false);
        assert!(entry.success_rate.unwrap() < 1.0);
    }

    #[test]
    fn catalog_upsert_and_get() {
        let mut catalog = Catalog::new();
        let manifest = test_manifest("worker-1", AgentKind::Worker);
        catalog.upsert(CatalogEntry::new(manifest));

        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("worker-1").is_some());
        assert!(catalog.get("nonexistent").is_none());
    }

    #[test]
    fn catalog_list_with_filter() {
        let mut catalog = Catalog::new();
        catalog.upsert(CatalogEntry::new(test_manifest("w1", AgentKind::Worker)));
        catalog.upsert(CatalogEntry::new(test_manifest("s1", AgentKind::Specialist)));
        catalog.upsert(CatalogEntry::new(test_manifest("w2", AgentKind::Worker)));

        let workers = catalog.list(Some(AgentKind::Worker));
        assert_eq!(workers.len(), 2);

        let all = catalog.list(None);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn catalog_json_roundtrip() {
        let mut catalog = Catalog::new();
        catalog.upsert(CatalogEntry::new(test_manifest("agent-a", AgentKind::Worker)));
        catalog.add_template(AgentTemplate::new("tmpl-1", "Test template", AgentKind::Worker));

        let json = catalog.export_json().unwrap();
        let restored = Catalog::import_json(&json).unwrap();

        assert_eq!(restored.len(), 1);
        assert!(restored.get("agent-a").is_some());
        assert!(restored.get_template("tmpl-1").is_some());
    }

    #[test]
    fn catalog_merge() {
        let mut a = Catalog::new();
        a.upsert(CatalogEntry::new(test_manifest("agent-a", AgentKind::Worker)));

        let mut b = Catalog::new();
        b.upsert(CatalogEntry::new(test_manifest("agent-b", AgentKind::Specialist)));

        a.merge(b);
        assert_eq!(a.len(), 2);
        assert!(a.get("agent-a").is_some());
        assert!(a.get("agent-b").is_some());
    }

    #[test]
    fn template_instantiate() {
        let template = AgentTemplate::new("test-worker", "A test worker", AgentKind::Worker)
            .with_capability(AgentCapability::new("lint", "1.0.0", "Lint code"))
            .with_metadata("team", "platform");

        let manifest = template.instantiate();
        assert_eq!(manifest.name, "test-worker");
        assert_eq!(manifest.kind, AgentKind::Worker);
        assert!(manifest.has_capability("lint"));
        assert_eq!(manifest.metadata.get("team").unwrap(), "platform");
    }

    #[test]
    fn builtin_templates_exist() {
        let templates = builtin_templates();
        assert!(!templates.is_empty());
        let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"code-reviewer"));
        assert!(names.contains(&"deployer"));
        assert!(names.contains(&"orchestrator"));
    }
}
