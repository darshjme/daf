//! Agent inventory management.
//!
//! The inventory is the DAF equivalent of Ansible's host inventory: a
//! structured catalog of all agents available for configuration. Agents
//! can be organized into groups, tagged, and annotated with per-agent
//! variables.
//!
//! Two loading strategies are provided:
//!
//! - **Static**: read from a YAML or JSON file on disk.
//! - **Registry**: dynamically discover agents from a DAF registry service.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use daf_core::AgentKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

// ---------------------------------------------------------------------------
// AgentEntry
// ---------------------------------------------------------------------------

/// A single agent in the inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Unique identifier for this agent (typically the agent name or UUID).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Agent's role classification.
    pub kind: AgentKind,
    /// Per-agent variables that override group and global vars.
    pub vars: HashMap<String, Value>,
    /// Tags for filtering and selection.
    pub tags: Vec<String>,
}

impl AgentEntry {
    /// Create a new agent entry.
    pub fn new(id: impl Into<String>, name: impl Into<String>, kind: AgentKind) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            vars: HashMap::new(),
            tags: Vec::new(),
        }
    }

    /// Add a variable to this agent entry.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Returns `true` if this agent has the given tag.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

// ---------------------------------------------------------------------------
// Group
// ---------------------------------------------------------------------------

/// A named group of agents, optionally containing child groups.
///
/// Groups provide a way to target multiple agents at once in playbooks
/// and to assign shared variables to a set of agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    /// Group name.
    pub name: String,
    /// Agent IDs that are direct members of this group.
    pub members: Vec<String>,
    /// Names of child groups whose members are also included.
    pub children: Vec<String>,
    /// Variables applied to all agents in this group.
    pub vars: HashMap<String, Value>,
}

impl Group {
    /// Create a new empty group.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            members: Vec::new(),
            children: Vec::new(),
            vars: HashMap::new(),
        }
    }

    /// Add a member agent ID.
    pub fn with_member(mut self, id: impl Into<String>) -> Self {
        self.members.push(id.into());
        self
    }

    /// Add a child group.
    pub fn with_child(mut self, child: impl Into<String>) -> Self {
        self.children.push(child.into());
        self
    }

    /// Set a group-level variable.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

/// Complete agent inventory: agents, groups, and their relationships.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// All known agents, keyed by their ID.
    pub agents: HashMap<String, AgentEntry>,
    /// Named groups of agents.
    pub groups: HashMap<String, Group>,
}

impl Inventory {
    /// Create an empty inventory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an agent to the inventory.
    pub fn add_agent(&mut self, agent: AgentEntry) {
        self.agents.insert(agent.id.clone(), agent);
    }

    /// Add a group to the inventory.
    pub fn add_group(&mut self, group: Group) {
        self.groups.insert(group.name.clone(), group);
    }

    /// Resolve all member IDs of a group, recursively expanding child groups.
    ///
    /// Returns an empty set if the group does not exist. Handles cycles
    /// by tracking visited groups.
    pub fn resolve_group_members(&self, group_name: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        self.resolve_group_recursive(group_name, &mut result, &mut visited);
        result
    }

    fn resolve_group_recursive(
        &self,
        group_name: &str,
        result: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(group_name.to_string()) {
            return; // Cycle detected.
        }

        if let Some(group) = self.groups.get(group_name) {
            for member in &group.members {
                result.insert(member.clone());
            }
            for child in &group.children {
                self.resolve_group_recursive(child, result, visited);
            }
        }
    }

    /// Find agents matching a pattern.
    ///
    /// Supported patterns:
    /// - `"all"` — every agent in the inventory
    /// - `"group:name"` — all agents in the named group (recursive)
    /// - `"kind:worker"` — all agents of the given kind
    /// - `"tag:value"` — all agents with the given tag
    /// - `"name1,name2"` — specific agents by ID
    /// - `"*pattern*"` — glob-style name matching (basic)
    pub fn select(&self, pattern: &str) -> Vec<&AgentEntry> {
        let pattern = pattern.trim();

        if pattern == "all" || pattern == "*" {
            return self.agents.values().collect();
        }

        if let Some(group_name) = pattern.strip_prefix("group:") {
            let members = self.resolve_group_members(group_name);
            return self
                .agents
                .values()
                .filter(|a| members.contains(&a.id))
                .collect();
        }

        if let Some(kind_str) = pattern.strip_prefix("kind:") {
            let target_kind = match kind_str {
                "orchestrator" => Some(AgentKind::Orchestrator),
                "specialist" => Some(AgentKind::Specialist),
                "worker" => Some(AgentKind::Worker),
                "monitor" => Some(AgentKind::Monitor),
                "router" => Some(AgentKind::Router),
                _ => None,
            };
            if let Some(kind) = target_kind {
                return self.agents.values().filter(|a| a.kind == kind).collect();
            }
            return Vec::new();
        }

        if let Some(tag) = pattern.strip_prefix("tag:") {
            return self.agents.values().filter(|a| a.has_tag(tag)).collect();
        }

        // Comma-separated list of IDs.
        if pattern.contains(',') {
            let ids: Vec<&str> = pattern.split(',').map(|s| s.trim()).collect();
            return self
                .agents
                .values()
                .filter(|a| ids.contains(&a.id.as_str()))
                .collect();
        }

        // Simple glob matching.
        if pattern.contains('*') {
            let pat = pattern.replace('*', "");
            return self
                .agents
                .values()
                .filter(|a| {
                    if pattern.starts_with('*') && pattern.ends_with('*') {
                        a.name.contains(&pat)
                    } else if pattern.starts_with('*') {
                        a.name.ends_with(&pat)
                    } else if pattern.ends_with('*') {
                        a.name.starts_with(&pat)
                    } else {
                        a.name.contains(&pat)
                    }
                })
                .collect();
        }

        // Exact ID match.
        self.agents.get(pattern).into_iter().collect()
    }

    /// Get variables for an agent, merging group vars (group vars are
    /// lower precedence, agent vars override).
    pub fn agent_vars(&self, agent_id: &str) -> HashMap<String, Value> {
        let mut merged = HashMap::new();

        // Collect group vars for groups this agent belongs to.
        for group in self.groups.values() {
            let members = self.resolve_group_members(&group.name);
            if members.contains(agent_id) {
                for (k, v) in &group.vars {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }

        // Agent vars override.
        if let Some(agent) = self.agents.get(agent_id) {
            for (k, v) in &agent.vars {
                merged.insert(k.clone(), v.clone());
            }
        }

        merged
    }

    /// Number of agents in the inventory.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Number of groups.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
}

// ---------------------------------------------------------------------------
// InventoryLoader trait
// ---------------------------------------------------------------------------

/// Strategy for populating an [`Inventory`].
#[async_trait]
pub trait InventoryLoader: Send + Sync {
    /// Load inventory data, returning a fully populated [`Inventory`].
    async fn load(&self) -> Result<Inventory, InventoryError>;
}

/// Errors that can occur during inventory loading.
#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("discovery error: {0}")]
    Discovery(String),
}

// ---------------------------------------------------------------------------
// StaticInventory
// ---------------------------------------------------------------------------

/// Loads inventory from a JSON string (or file contents).
///
/// The expected format mirrors the [`Inventory`] struct:
///
/// ```json
/// {
///   "agents": {
///     "agent-1": { "id": "agent-1", "name": "Researcher", "kind": "specialist", "vars": {}, "tags": [] }
///   },
///   "groups": {
///     "researchers": { "name": "researchers", "members": ["agent-1"], "children": [], "vars": {} }
///   }
/// }
/// ```
pub struct StaticInventory {
    source: String,
}

impl StaticInventory {
    /// Create from raw JSON string.
    pub fn from_json(json: impl Into<String>) -> Self {
        Self {
            source: json.into(),
        }
    }

    /// Create from a file path. The file is read at load time.
    pub fn from_file(path: impl Into<String>) -> Self {
        Self {
            source: path.into(),
        }
    }
}

#[async_trait]
impl InventoryLoader for StaticInventory {
    async fn load(&self) -> Result<Inventory, InventoryError> {
        let content = if self.source.trim_start().starts_with('{') {
            // Inline JSON.
            self.source.clone()
        } else {
            // File path.
            debug!(path = %self.source, "loading static inventory from file");
            tokio::fs::read_to_string(&self.source)
                .await
                .map_err(InventoryError::Io)?
        };

        serde_json::from_str::<Inventory>(&content)
            .map_err(|e| InventoryError::Parse(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// RegistryInventory
// ---------------------------------------------------------------------------

/// Discovers agents dynamically from a DAF registry endpoint.
///
/// This is a placeholder implementation — the actual registry client
/// lives in `daf-registry`. This loader demonstrates the interface.
pub struct RegistryInventory {
    /// Registry endpoint URL.
    pub endpoint: String,
}

impl RegistryInventory {
    /// Create a registry-based inventory loader.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl InventoryLoader for RegistryInventory {
    async fn load(&self) -> Result<Inventory, InventoryError> {
        debug!(endpoint = %self.endpoint, "discovering agents from registry");
        // In production this would query the registry service.
        // For now we return an empty inventory as a no-op.
        Ok(Inventory::new())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_inventory() -> Inventory {
        let mut inv = Inventory::new();

        inv.add_agent(
            AgentEntry::new("r1", "Researcher Alpha", AgentKind::Specialist)
                .with_tag("gpu")
                .with_var("model", json!("gpt-4")),
        );
        inv.add_agent(
            AgentEntry::new("r2", "Researcher Beta", AgentKind::Specialist)
                .with_tag("gpu")
                .with_tag("fast"),
        );
        inv.add_agent(AgentEntry::new("w1", "Worker One", AgentKind::Worker).with_tag("cpu"));
        inv.add_agent(AgentEntry::new("m1", "Monitor", AgentKind::Monitor));

        inv.add_group(
            Group::new("researchers")
                .with_member("r1")
                .with_member("r2")
                .with_var("timeout", json!(120)),
        );
        inv.add_group(
            Group::new("all_compute")
                .with_child("researchers")
                .with_member("w1"),
        );

        inv
    }

    #[test]
    fn select_all() {
        let inv = test_inventory();
        assert_eq!(inv.select("all").len(), 4);
        assert_eq!(inv.select("*").len(), 4);
    }

    #[test]
    fn select_by_group() {
        let inv = test_inventory();
        let selected = inv.select("group:researchers");
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn select_by_kind() {
        let inv = test_inventory();
        let selected = inv.select("kind:specialist");
        assert_eq!(selected.len(), 2);

        let workers = inv.select("kind:worker");
        assert_eq!(workers.len(), 1);
    }

    #[test]
    fn select_by_tag() {
        let inv = test_inventory();
        let selected = inv.select("tag:gpu");
        assert_eq!(selected.len(), 2);

        let cpu = inv.select("tag:cpu");
        assert_eq!(cpu.len(), 1);
    }

    #[test]
    fn select_by_comma_list() {
        let inv = test_inventory();
        let selected = inv.select("r1, w1");
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn select_by_exact_id() {
        let inv = test_inventory();
        let selected = inv.select("m1");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "Monitor");
    }

    #[test]
    fn select_by_glob() {
        let inv = test_inventory();
        let selected = inv.select("*Researcher*");
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn resolve_group_recursive() {
        let inv = test_inventory();
        let members = inv.resolve_group_members("all_compute");
        assert!(members.contains("r1"));
        assert!(members.contains("r2"));
        assert!(members.contains("w1"));
        assert_eq!(members.len(), 3);
    }

    #[test]
    fn resolve_group_cycle_safety() {
        let mut inv = Inventory::new();
        inv.add_group(Group::new("a").with_child("b"));
        inv.add_group(Group::new("b").with_child("a").with_member("x"));

        // Should not stack overflow.
        let members = inv.resolve_group_members("a");
        assert!(members.contains("x"));
    }

    #[test]
    fn agent_vars_merge() {
        let inv = test_inventory();
        let vars = inv.agent_vars("r1");
        // Group var.
        assert_eq!(vars.get("timeout"), Some(&json!(120)));
        // Agent var.
        assert_eq!(vars.get("model"), Some(&json!("gpt-4")));
    }

    #[test]
    fn serde_roundtrip() {
        let inv = test_inventory();
        let json = serde_json::to_string(&inv).unwrap();
        let back: Inventory = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_count(), 4);
        assert_eq!(back.group_count(), 2);
    }

    #[tokio::test]
    async fn static_inventory_from_json() {
        let inv = test_inventory();
        let json = serde_json::to_string(&inv).unwrap();

        let loader = StaticInventory::from_json(json);
        let loaded = loader.load().await.unwrap();
        assert_eq!(loaded.agent_count(), 4);
    }

    #[tokio::test]
    async fn registry_inventory_returns_empty() {
        let loader = RegistryInventory::new("http://localhost:9999");
        let loaded = loader.load().await.unwrap();
        assert_eq!(loaded.agent_count(), 0);
    }
}
