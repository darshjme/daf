//! Execution planning for provisioning changes.
//!
//! The planner compares desired resource specifications against the current
//! provision state and produces a [`Plan`] — an ordered list of changes
//! that must be applied to converge the infrastructure to the desired state.
//!
//! This is the `terraform plan` equivalent for DAF agent topologies.

use std::collections::{HashMap, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info};

use daf_core::{DafError, DafResult};

use crate::resource::ResourceSpec;
use crate::state::ProvisionState;

// ---------------------------------------------------------------------------
// Action
// ---------------------------------------------------------------------------

/// The type of change to apply to a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Create a new resource that doesn't exist yet.
    Create,
    /// Update an existing resource in-place.
    Update,
    /// Destroy an existing resource.
    Destroy,
    /// Destroy and recreate a resource (when in-place update is impossible).
    Replace,
    /// No changes needed — resource matches desired state.
    NoOp,
}

impl Action {
    /// Display symbol for plan output.
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Create => "+",
            Self::Update => "~",
            Self::Destroy => "-",
            Self::Replace => "-/+",
            Self::NoOp => " ",
        }
    }

    /// Returns `true` if this action modifies infrastructure.
    pub fn is_mutating(&self) -> bool {
        !matches!(self, Self::NoOp)
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create => write!(f, "create"),
            Self::Update => write!(f, "update"),
            Self::Destroy => write!(f, "destroy"),
            Self::Replace => write!(f, "replace"),
            Self::NoOp => write!(f, "no-op"),
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceChange
// ---------------------------------------------------------------------------

/// A single planned change to a resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceChange {
    /// The logical name of the resource being changed.
    pub resource_name: String,

    /// What action to take.
    pub action: Action,

    /// The resource configuration before the change (`None` for creates).
    pub before: Option<Value>,

    /// The resource configuration after the change (`None` for destroys).
    pub after: Option<Value>,

    /// Human-readable explanation of why this change is needed.
    pub reason: String,
}

impl ResourceChange {
    /// Create a new change record.
    pub fn new(
        resource_name: impl Into<String>,
        action: Action,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            resource_name: resource_name.into(),
            action,
            before: None,
            after: None,
            reason: reason.into(),
        }
    }

    /// Builder: set the before snapshot.
    pub fn with_before(mut self, before: Value) -> Self {
        self.before = Some(before);
        self
    }

    /// Builder: set the after snapshot.
    pub fn with_after(mut self, after: Value) -> Self {
        self.after = Some(after);
        self
    }
}

impl fmt::Display for ResourceChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "  {} {} ({})",
            self.action.symbol(),
            self.resource_name,
            self.reason,
        )
    }
}

// ---------------------------------------------------------------------------
// PlanSummary
// ---------------------------------------------------------------------------

/// Aggregate counts of planned actions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanSummary {
    /// Number of resources to create.
    pub creates: usize,
    /// Number of resources to update.
    pub updates: usize,
    /// Number of resources to destroy.
    pub destroys: usize,
    /// Number of resources to replace.
    pub replaces: usize,
    /// Number of resources with no changes.
    pub no_ops: usize,
}

impl PlanSummary {
    /// Total number of mutating changes.
    pub fn total_changes(&self) -> usize {
        self.creates + self.updates + self.destroys + self.replaces
    }

    /// Returns `true` if the plan has no mutating changes.
    pub fn is_empty(&self) -> bool {
        self.total_changes() == 0
    }
}

impl fmt::Display for PlanSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Plan: {} to add, {} to change, {} to destroy, {} to replace",
            self.creates, self.updates, self.destroys, self.replaces,
        )
    }
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// A complete execution plan describing all changes needed to converge
/// infrastructure to the desired state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Ordered list of resource changes. The order respects dependency
    /// constraints: resources are created after their dependencies and
    /// destroyed before their dependents.
    pub changes: Vec<ResourceChange>,

    /// Non-fatal warnings generated during planning (e.g., deprecated
    /// config fields, ignore_changes masking drift).
    pub warnings: Vec<String>,

    /// Aggregate counts.
    pub summary: PlanSummary,
}

impl Plan {
    /// Create a plan from a list of changes, computing the summary.
    pub fn from_changes(changes: Vec<ResourceChange>, warnings: Vec<String>) -> Self {
        let mut summary = PlanSummary::default();
        for change in &changes {
            match change.action {
                Action::Create => summary.creates += 1,
                Action::Update => summary.updates += 1,
                Action::Destroy => summary.destroys += 1,
                Action::Replace => summary.replaces += 1,
                Action::NoOp => summary.no_ops += 1,
            }
        }
        Self {
            changes,
            warnings,
            summary,
        }
    }

    /// Returns `true` if the plan has no mutating changes.
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
    }

    /// Return only the mutating changes (excluding NoOps).
    pub fn mutating_changes(&self) -> Vec<&ResourceChange> {
        self.changes
            .iter()
            .filter(|c| c.action.is_mutating())
            .collect()
    }

    /// Format the plan as a human-readable string similar to `terraform plan`.
    pub fn display_plan(&self) -> String {
        let mut out = String::new();

        if self.is_empty() {
            out.push_str("No changes. Infrastructure is up-to-date.\n");
            return out;
        }

        out.push_str("DAF Provision will perform the following actions:\n\n");

        for change in &self.changes {
            if change.action == Action::NoOp {
                continue;
            }
            out.push_str(&format!("{change}\n"));
        }

        out.push('\n');
        out.push_str(&format!("{}\n", self.summary));

        if !self.warnings.is_empty() {
            out.push_str("\nWarnings:\n");
            for w in &self.warnings {
                out.push_str(&format!("  ! {w}\n"));
            }
        }

        out
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_plan())
    }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

/// Compares desired resource specifications against current state and
/// produces an execution [`Plan`].
pub struct Planner;

impl Planner {
    /// Generate a plan by diffing `desired` specs against `current` state.
    ///
    /// The returned plan respects dependency ordering: creates are
    /// topologically sorted so dependencies come first, and destroys are
    /// reverse-sorted so dependents are removed first.
    pub fn plan(current: &ProvisionState, desired: &[ResourceSpec]) -> DafResult<Plan> {
        let mut changes = Vec::new();
        let mut warnings = Vec::new();

        let desired_map: HashMap<&str, &ResourceSpec> =
            desired.iter().map(|s| (s.name.as_str(), s)).collect();

        // Phase 1: Detect creates and updates.
        for spec in desired {
            match current.resources.get(&spec.name) {
                None => {
                    debug!(resource = %spec.name, "planned: create");
                    let change =
                        ResourceChange::new(&spec.name, Action::Create, "resource does not exist")
                            .with_after(spec.config.clone());
                    changes.push(change);
                }
                Some(existing) => {
                    // Apply ignore_changes filter.
                    let mut effective_old = existing.spec.config.clone();
                    let mut effective_new = spec.config.clone();

                    for key in &spec.lifecycle.ignore_changes {
                        if let (Some(old_obj), Some(new_obj)) =
                            (effective_old.as_object_mut(), effective_new.as_object_mut())
                        {
                            old_obj.remove(key);
                            new_obj.remove(key);
                            warnings.push(format!(
                                "ignoring changes to '{key}' on resource '{}'",
                                spec.name
                            ));
                        }
                    }

                    if effective_old != effective_new
                        || existing.spec.type_ != spec.type_
                        || existing.spec.provider != spec.provider
                    {
                        // Check if the type or provider changed — that requires replace.
                        let action = if existing.spec.type_ != spec.type_
                            || existing.spec.provider != spec.provider
                        {
                            Action::Replace
                        } else {
                            Action::Update
                        };

                        debug!(resource = %spec.name, ?action, "planned: change");
                        let change =
                            ResourceChange::new(&spec.name, action, "configuration changed")
                                .with_before(existing.spec.config.clone())
                                .with_after(spec.config.clone());
                        changes.push(change);
                    } else {
                        changes.push(ResourceChange::new(
                            &spec.name,
                            Action::NoOp,
                            "no changes detected",
                        ));
                    }
                }
            }
        }

        // Phase 2: Detect destroys (resources in state but not in desired).
        for (name, existing) in &current.resources {
            if !desired_map.contains_key(name.as_str()) {
                if existing.spec.lifecycle.prevent_destroy {
                    warnings.push(format!(
                        "resource '{}' is absent from config but has prevent_destroy=true, skipping",
                        name
                    ));
                    continue;
                }

                debug!(resource = %name, "planned: destroy");
                let change =
                    ResourceChange::new(name, Action::Destroy, "resource removed from config")
                        .with_before(existing.spec.config.clone());
                changes.push(change);
            }
        }

        // Phase 3: Topological sort for dependency ordering.
        let sorted = topological_sort(&changes, desired)?;
        let plan = Plan::from_changes(sorted, warnings);

        info!("{}", plan.summary);

        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// Topological sort
// ---------------------------------------------------------------------------

/// Sort resource changes respecting dependency order.
///
/// Creates/updates are ordered so that dependencies come first.
/// Destroys are placed after all creates/updates, in reverse dependency order.
fn topological_sort(
    changes: &[ResourceChange],
    desired: &[ResourceSpec],
) -> DafResult<Vec<ResourceChange>> {
    let spec_map: HashMap<&str, &ResourceSpec> =
        desired.iter().map(|s| (s.name.as_str(), s)).collect();

    // Separate creates/updates from destroys.
    let creates_updates: Vec<&ResourceChange> = changes
        .iter()
        .filter(|c| {
            matches!(
                c.action,
                Action::Create | Action::Update | Action::Replace | Action::NoOp
            )
        })
        .collect();

    let destroys: Vec<&ResourceChange> = changes
        .iter()
        .filter(|c| c.action == Action::Destroy)
        .collect();

    // Build adjacency for creates/updates based on depends_on.
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for change in &creates_updates {
        in_degree.entry(change.resource_name.as_str()).or_insert(0);
    }

    for change in &creates_updates {
        if let Some(spec) = spec_map.get(change.resource_name.as_str()) {
            for dep in &spec.depends_on {
                // Only count edges to resources that are in the change set.
                if in_degree.contains_key(dep.as_str()) {
                    adj.entry(dep.as_str())
                        .or_default()
                        .push(change.resource_name.as_str());
                    *in_degree.entry(change.resource_name.as_str()).or_insert(0) += 1;
                }
            }
        }
    }

    // Kahn's algorithm.
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(name, _)| *name)
        .collect();

    let mut sorted_names: Vec<&str> = Vec::new();

    while let Some(name) = queue.pop_front() {
        sorted_names.push(name);
        if let Some(neighbors) = adj.get(name) {
            for &neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    if sorted_names.len() != creates_updates.len() {
        return Err(DafError::ConfigError(
            "circular dependency detected in resource graph".into(),
        ));
    }

    // Build the final ordered list.
    let change_map: HashMap<&str, &ResourceChange> = changes
        .iter()
        .map(|c| (c.resource_name.as_str(), c))
        .collect();

    let mut result: Vec<ResourceChange> = sorted_names
        .iter()
        .filter_map(|name| change_map.get(name))
        .map(|c| (*c).clone())
        .collect();

    // Destroys go last, in reverse dependency order (dependents first).
    // For simplicity, reverse the destroys list.
    let mut destroy_list: Vec<ResourceChange> = destroys.into_iter().cloned().collect();
    destroy_list.reverse();
    result.extend(destroy_list);

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{Resource, ResourceType};

    fn pool_spec(name: &str, count: u32) -> ResourceSpec {
        ResourceSpec::new(ResourceType::AgentPool, name, "agent_pool")
            .with_config(serde_json::json!({"count": count}))
    }

    #[test]
    fn action_symbols() {
        assert_eq!(Action::Create.symbol(), "+");
        assert_eq!(Action::Update.symbol(), "~");
        assert_eq!(Action::Destroy.symbol(), "-");
        assert_eq!(Action::Replace.symbol(), "-/+");
        assert_eq!(Action::NoOp.symbol(), " ");
    }

    #[test]
    fn plan_empty_to_desired() {
        let state = ProvisionState::new();
        let desired = vec![pool_spec("workers", 5)];

        let plan = Planner::plan(&state, &desired).unwrap();
        assert_eq!(plan.summary.creates, 1);
        assert_eq!(plan.summary.total_changes(), 1);
        assert!(!plan.is_empty());
    }

    #[test]
    fn plan_no_changes() {
        let spec = pool_spec("workers", 5);
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(spec.clone()));

        let plan = Planner::plan(&state, &[spec]).unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.summary.no_ops, 1);
    }

    #[test]
    fn plan_update_on_config_change() {
        let spec = pool_spec("workers", 5);
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(spec));

        let updated = pool_spec("workers", 10);
        let plan = Planner::plan(&state, &[updated]).unwrap();
        assert_eq!(plan.summary.updates, 1);
    }

    #[test]
    fn plan_destroy_removed_resource() {
        let spec = pool_spec("old", 3);
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(spec));

        let plan = Planner::plan(&state, &[]).unwrap();
        assert_eq!(plan.summary.destroys, 1);
    }

    #[test]
    fn plan_prevent_destroy() {
        let mut spec = pool_spec("critical", 3);
        spec.lifecycle.prevent_destroy = true;

        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(spec));

        let plan = Planner::plan(&state, &[]).unwrap();
        assert_eq!(plan.summary.destroys, 0);
        assert!(!plan.warnings.is_empty());
    }

    #[test]
    fn plan_replace_on_type_change() {
        let spec = ResourceSpec::new(ResourceType::AgentPool, "flex", "agent_pool")
            .with_config(serde_json::json!({}));
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(spec));

        // Same name, different provider.
        let new_spec = ResourceSpec::new(ResourceType::AgentPool, "flex", "custom_pool")
            .with_config(serde_json::json!({}));
        let plan = Planner::plan(&state, &[new_spec]).unwrap();
        assert_eq!(plan.summary.replaces, 1);
    }

    #[test]
    fn plan_ignore_changes() {
        let spec = pool_spec("workers", 5);
        let mut state = ProvisionState::new();
        state.upsert_resource(Resource::from_spec(spec));

        let mut updated = pool_spec("workers", 999);
        updated.lifecycle.ignore_changes = vec!["count".into()];

        let plan = Planner::plan(&state, &[updated]).unwrap();
        assert!(plan.is_empty());
        assert!(!plan.warnings.is_empty());
    }

    #[test]
    fn plan_dependency_ordering() {
        let mut downstream = pool_spec("builders", 3);
        downstream.depends_on = vec!["researchers".into()];

        let upstream = pool_spec("researchers", 5);

        // Pass downstream first to verify reordering.
        let state = ProvisionState::new();
        let plan = Planner::plan(&state, &[downstream, upstream]).unwrap();

        let create_names: Vec<&str> = plan
            .changes
            .iter()
            .filter(|c| c.action == Action::Create)
            .map(|c| c.resource_name.as_str())
            .collect();

        let researchers_idx = create_names
            .iter()
            .position(|n| *n == "researchers")
            .unwrap();
        let builders_idx = create_names.iter().position(|n| *n == "builders").unwrap();
        assert!(
            researchers_idx < builders_idx,
            "upstream must come before downstream"
        );
    }

    #[test]
    fn plan_display_format() {
        let state = ProvisionState::new();
        let desired = vec![pool_spec("alpha", 2), pool_spec("beta", 3)];
        let plan = Planner::plan(&state, &desired).unwrap();
        let output = plan.display_plan();

        assert!(output.contains("+"));
        assert!(output.contains("alpha"));
        assert!(output.contains("beta"));
        assert!(output.contains("to add"));
    }

    #[test]
    fn plan_summary_display() {
        let summary = PlanSummary {
            creates: 2,
            updates: 1,
            destroys: 0,
            replaces: 0,
            no_ops: 3,
        };
        let display = summary.to_string();
        assert!(display.contains("2 to add"));
        assert!(display.contains("1 to change"));
    }

    #[test]
    fn plan_circular_dependency_detected() {
        let mut a = pool_spec("a", 1);
        a.depends_on = vec!["b".into()];
        let mut b = pool_spec("b", 1);
        b.depends_on = vec!["a".into()];

        let state = ProvisionState::new();
        let result = Planner::plan(&state, &[a, b]);
        assert!(result.is_err());
    }

    #[test]
    fn mutating_changes_filter() {
        let state = ProvisionState::new();
        let spec = pool_spec("only", 1);
        let plan = Planner::plan(&state, &[spec]).unwrap();

        let mutating = plan.mutating_changes();
        assert_eq!(mutating.len(), 1);
        assert_eq!(mutating[0].action, Action::Create);
    }
}
