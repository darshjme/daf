//! Role system for reusable agent configuration units.
//!
//! Roles are the DAF equivalent of Ansible roles: self-contained bundles
//! of tasks, handlers, variables, and defaults that can be shared across
//! playbooks. A role encapsulates a complete behavioral profile (e.g.
//! "security-auditor", "code-reviewer") with all its configuration.
//!
//! # Directory layout
//!
//! ```text
//! roles/
//!   security-auditor/
//!     tasks/main.json         — task definitions
//!     handlers/main.json      — handler definitions
//!     defaults/main.json      — default variables (lowest precedence)
//!     vars/main.json          — role variables (higher precedence)
//!     meta/main.json          — dependencies and metadata
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::condition::Condition;
use crate::handler::Handler;
use crate::playbook::TaskDef;

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// A reusable configuration unit containing tasks, handlers, and variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    /// Role name (must be unique within the roles directory).
    pub name: String,
    /// Optional description of what this role configures.
    pub description: Option<String>,
    /// Tasks to execute when the role is applied.
    pub tasks: Vec<TaskDef>,
    /// Handlers that tasks in this role can notify.
    pub handlers: Vec<Handler>,
    /// Default variables — lowest precedence, easily overridden.
    pub defaults: HashMap<String, Value>,
    /// Role variables — higher precedence than defaults, lower than
    /// play-level vars.
    pub vars: HashMap<String, Value>,
    /// Other roles that must be applied before this one.
    pub dependencies: Vec<RoleDependency>,
    /// Arbitrary metadata.
    pub metadata: HashMap<String, Value>,
}

impl Role {
    /// Create a new empty role.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            tasks: Vec::new(),
            handlers: Vec::new(),
            defaults: HashMap::new(),
            vars: HashMap::new(),
            dependencies: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a task to the role.
    pub fn with_task(mut self, task: TaskDef) -> Self {
        self.tasks.push(task);
        self
    }

    /// Add a handler.
    pub fn with_handler(mut self, handler: Handler) -> Self {
        self.handlers.push(handler);
        self
    }

    /// Set a default variable.
    pub fn with_default(mut self, key: impl Into<String>, value: Value) -> Self {
        self.defaults.insert(key.into(), value);
        self
    }

    /// Set a role variable.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }

    /// Add a dependency.
    pub fn with_dependency(mut self, dep: RoleDependency) -> Self {
        self.dependencies.push(dep);
        self
    }
}

// ---------------------------------------------------------------------------
// RoleDependency
// ---------------------------------------------------------------------------

/// A dependency on another role, optionally with variable overrides and
/// a condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDependency {
    /// Name of the required role.
    pub role_name: String,
    /// Variable overrides passed to the dependency.
    pub vars: HashMap<String, Value>,
    /// Optional condition — if false, the dependency is skipped.
    pub when: Option<Condition>,
}

impl RoleDependency {
    /// Create an unconditional dependency on another role.
    pub fn new(role_name: impl Into<String>) -> Self {
        Self {
            role_name: role_name.into(),
            vars: HashMap::new(),
            when: None,
        }
    }

    /// Add variable overrides for the dependency.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }

    /// Make the dependency conditional.
    pub fn with_condition(mut self, condition: Condition) -> Self {
        self.when = Some(condition);
        self
    }
}

// ---------------------------------------------------------------------------
// RoleLoader
// ---------------------------------------------------------------------------

/// Loads roles from a directory hierarchy.
///
/// Each subdirectory of the roles root represents a role. The loader
/// reads the conventional files (`tasks/main.json`, `handlers/main.json`,
/// etc.) and assembles a [`Role`] struct.
pub struct RoleLoader {
    /// Root directory containing role subdirectories.
    root: PathBuf,
}

impl RoleLoader {
    /// Create a loader that reads from the given roles directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Load a single role by name.
    pub async fn load(&self, name: &str) -> Result<Role, RoleError> {
        let role_dir = self.root.join(name);
        if !role_dir.is_dir() {
            return Err(RoleError::NotFound(name.to_string()));
        }

        debug!(role = name, path = %role_dir.display(), "loading role");

        let mut role = Role::new(name);

        // tasks/main.json
        if let Some(tasks) = self.read_json_file::<Vec<TaskDef>>(&role_dir.join("tasks/main.json")).await? {
            role.tasks = tasks;
        }

        // handlers/main.json
        if let Some(handlers) = self.read_json_file::<Vec<Handler>>(&role_dir.join("handlers/main.json")).await? {
            role.handlers = handlers;
        }

        // defaults/main.json
        if let Some(defaults) = self.read_json_file::<HashMap<String, Value>>(&role_dir.join("defaults/main.json")).await? {
            role.defaults = defaults;
        }

        // vars/main.json
        if let Some(vars) = self.read_json_file::<HashMap<String, Value>>(&role_dir.join("vars/main.json")).await? {
            role.vars = vars;
        }

        // meta/main.json
        if let Some(meta) = self.read_json_file::<RoleMeta>(&role_dir.join("meta/main.json")).await? {
            role.dependencies = meta.dependencies;
            role.description = meta.description;
            role.metadata = meta.metadata;
        }

        Ok(role)
    }

    /// Load all roles found in the root directory.
    pub async fn load_all(&self) -> Result<Vec<Role>, RoleError> {
        let mut roles = Vec::new();

        if !self.root.exists() {
            return Ok(roles);
        }

        let mut entries = tokio::fs::read_dir(&self.root)
            .await
            .map_err(|e| RoleError::Io(e.to_string()))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| RoleError::Io(e.to_string()))?
        {
            if entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false)
            {
                let name = entry.file_name().to_string_lossy().to_string();
                match self.load(&name).await {
                    Ok(role) => roles.push(role),
                    Err(e) => warn!(role = %name, error = %e, "failed to load role"),
                }
            }
        }

        Ok(roles)
    }

    /// Read and deserialize a JSON file, returning `None` if the file
    /// does not exist.
    async fn read_json_file<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
    ) -> Result<Option<T>, RoleError> {
        if !path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| RoleError::Io(format!("{}: {e}", path.display())))?;

        let value: T = serde_json::from_str(&content)
            .map_err(|e| RoleError::Parse(format!("{}: {e}", path.display())))?;

        Ok(Some(value))
    }
}

/// Intermediate struct for deserializing `meta/main.json`.
#[derive(Debug, Deserialize)]
struct RoleMeta {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    dependencies: Vec<RoleDependency>,
    #[serde(default)]
    metadata: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// Dependency resolution
// ---------------------------------------------------------------------------

/// Resolve role dependencies in topological order, detecting cycles.
///
/// Given a set of roles, returns them in an order such that every role
/// appears after all of its dependencies.
pub fn resolve_dependency_order(roles: &[Role]) -> Result<Vec<String>, RoleError> {
    let role_map: HashMap<&str, &Role> = roles.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut order = Vec::new();
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();

    for role in roles {
        if !visited.contains(role.name.as_str()) {
            visit_role(
                &role.name,
                &role_map,
                &mut visited,
                &mut in_stack,
                &mut order,
            )?;
        }
    }

    Ok(order)
}

fn visit_role<'a>(
    name: &'a str,
    role_map: &HashMap<&'a str, &'a Role>,
    visited: &mut HashSet<&'a str>,
    in_stack: &mut HashSet<&'a str>,
    order: &mut Vec<String>,
) -> Result<(), RoleError> {
    if in_stack.contains(name) {
        return Err(RoleError::CyclicDependency(name.to_string()));
    }
    if visited.contains(name) {
        return Ok(());
    }

    in_stack.insert(name);

    if let Some(role) = role_map.get(name) {
        for dep in &role.dependencies {
            visit_role(&dep.role_name, role_map, visited, in_stack, order)?;
        }
    }

    in_stack.remove(name);
    visited.insert(name);
    order.push(name.to_string());

    Ok(())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from role loading and dependency resolution.
#[derive(Debug, thiserror::Error)]
pub enum RoleError {
    #[error("role not found: {0}")]
    NotFound(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("cyclic dependency detected involving role: {0}")]
    CyclicDependency(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn role_builder() {
        let role = Role::new("security-auditor")
            .with_default("scan_depth", json!(3))
            .with_var("strict_mode", json!(true))
            .with_dependency(RoleDependency::new("base-config"));

        assert_eq!(role.name, "security-auditor");
        assert_eq!(role.defaults.get("scan_depth"), Some(&json!(3)));
        assert_eq!(role.vars.get("strict_mode"), Some(&json!(true)));
        assert_eq!(role.dependencies.len(), 1);
        assert_eq!(role.dependencies[0].role_name, "base-config");
    }

    #[test]
    fn dependency_with_vars_and_condition() {
        let dep = RoleDependency::new("logging")
            .with_var("log_level", json!("debug"))
            .with_condition(Condition::Equals("env".into(), json!("production")));

        assert_eq!(dep.role_name, "logging");
        assert_eq!(dep.vars.get("log_level"), Some(&json!("debug")));
        assert!(dep.when.is_some());
    }

    #[test]
    fn dependency_resolution_simple() {
        let roles = vec![
            Role::new("base"),
            Role::new("logging").with_dependency(RoleDependency::new("base")),
            Role::new("security")
                .with_dependency(RoleDependency::new("base"))
                .with_dependency(RoleDependency::new("logging")),
        ];

        let order = resolve_dependency_order(&roles).unwrap();
        let base_pos = order.iter().position(|r| r == "base").unwrap();
        let logging_pos = order.iter().position(|r| r == "logging").unwrap();
        let security_pos = order.iter().position(|r| r == "security").unwrap();

        assert!(base_pos < logging_pos);
        assert!(logging_pos < security_pos);
    }

    #[test]
    fn dependency_resolution_cycle_detected() {
        let roles = vec![
            Role::new("a").with_dependency(RoleDependency::new("b")),
            Role::new("b").with_dependency(RoleDependency::new("a")),
        ];

        let result = resolve_dependency_order(&roles);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RoleError::CyclicDependency(_)));
    }

    #[test]
    fn dependency_resolution_diamond() {
        // Diamond: D depends on B and C, both depend on A.
        let roles = vec![
            Role::new("A"),
            Role::new("B").with_dependency(RoleDependency::new("A")),
            Role::new("C").with_dependency(RoleDependency::new("A")),
            Role::new("D")
                .with_dependency(RoleDependency::new("B"))
                .with_dependency(RoleDependency::new("C")),
        ];

        let order = resolve_dependency_order(&roles).unwrap();
        assert_eq!(order.len(), 4);

        let a_pos = order.iter().position(|r| r == "A").unwrap();
        let b_pos = order.iter().position(|r| r == "B").unwrap();
        let c_pos = order.iter().position(|r| r == "C").unwrap();
        let d_pos = order.iter().position(|r| r == "D").unwrap();

        assert!(a_pos < b_pos);
        assert!(a_pos < c_pos);
        assert!(b_pos < d_pos);
        assert!(c_pos < d_pos);
    }

    #[test]
    fn role_serde_roundtrip() {
        let role = Role::new("test")
            .with_default("x", json!(1))
            .with_dependency(RoleDependency::new("base"));

        let json = serde_json::to_string(&role).unwrap();
        let back: Role = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "test");
        assert_eq!(back.defaults.get("x"), Some(&json!(1)));
        assert_eq!(back.dependencies.len(), 1);
    }

    #[test]
    fn no_roles_resolves_empty() {
        let roles: Vec<Role> = vec![];
        let order = resolve_dependency_order(&roles).unwrap();
        assert!(order.is_empty());
    }
}
