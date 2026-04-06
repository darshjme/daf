//! Resource definitions for declarative infrastructure provisioning.
//!
//! Resources are the fundamental building blocks of a DAF provision
//! configuration — analogous to Terraform resources. Each resource describes
//! a desired piece of infrastructure (an agent pool, a communication channel,
//! a pipeline, etc.) and transitions through a well-defined state machine
//! as the provisioner creates, updates, or destroys it.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;


// ---------------------------------------------------------------------------
// ResourceType
// ---------------------------------------------------------------------------

/// The kind of infrastructure a resource represents.
///
/// Built-in types cover the most common agent topologies. The `Custom` variant
/// allows third-party providers to introduce their own resource kinds without
/// modifying the core enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    /// A pool of homogeneous agents sharing the same configuration.
    AgentPool,
    /// A communication channel connecting two or more agents.
    Channel,
    /// An ordered multi-stage task pipeline.
    Pipeline,
    /// A complete workflow definition (DAG of tasks).
    Workflow,
    /// A persistent storage volume attached to agents.
    Storage,
    /// A virtual network segment isolating agent traffic.
    Network,
    /// An extension resource type defined by a third-party provider.
    Custom(String),
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentPool => write!(f, "agent_pool"),
            Self::Channel => write!(f, "channel"),
            Self::Pipeline => write!(f, "pipeline"),
            Self::Workflow => write!(f, "workflow"),
            Self::Storage => write!(f, "storage"),
            Self::Network => write!(f, "network"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// LifecyclePolicy
// ---------------------------------------------------------------------------

/// Controls how a resource is managed during plan/apply cycles.
///
/// These flags mirror Terraform's `lifecycle` meta-argument, giving operators
/// fine-grained control over destructive operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecyclePolicy {
    /// When replacing a resource, create the new one before destroying the old.
    /// This prevents downtime for resources that must always have at least one
    /// live instance.
    pub create_before_destroy: bool,

    /// Prevent this resource from being destroyed. Useful for stateful
    /// resources like storage volumes where data loss is unacceptable.
    pub prevent_destroy: bool,

    /// List of config keys whose changes should be ignored during planning.
    /// Changes to these fields will not trigger an update or replace action.
    pub ignore_changes: Vec<String>,
}

impl Default for LifecyclePolicy {
    fn default() -> Self {
        Self {
            create_before_destroy: false,
            prevent_destroy: false,
            ignore_changes: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceSpec
// ---------------------------------------------------------------------------

/// Declarative specification of a desired resource.
///
/// This is the "config file" representation — what the operator writes. The
/// provisioner compares a `ResourceSpec` against the current [`ResourceState`]
/// to determine what actions are needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSpec {
    /// The kind of resource to provision.
    #[serde(rename = "type")]
    pub type_: ResourceType,

    /// Unique logical name within the provision configuration. Used as the
    /// key in state maps and as a human-readable reference in plans.
    pub name: String,

    /// The provider responsible for managing this resource's lifecycle.
    /// Must match a registered [`Provider`](crate::provider::Provider) name.
    pub provider: String,

    /// Provider-specific configuration. The provider's schema validates
    /// this blob before any CRUD operation.
    pub config: Value,

    /// Logical names of other resources this one depends on. The planner
    /// uses these to build the execution graph and determine ordering.
    pub depends_on: Vec<String>,

    /// Lifecycle management policy.
    pub lifecycle: LifecyclePolicy,
}

impl ResourceSpec {
    /// Create a new resource spec with minimal fields.
    pub fn new(type_: ResourceType, name: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            type_,
            name: name.into(),
            provider: provider.into(),
            config: Value::Object(serde_json::Map::new()),
            depends_on: Vec::new(),
            lifecycle: LifecyclePolicy::default(),
        }
    }

    /// Builder: set the config blob.
    pub fn with_config(mut self, config: Value) -> Self {
        self.config = config;
        self
    }

    /// Builder: add a dependency.
    pub fn with_dependency(mut self, dep: impl Into<String>) -> Self {
        self.depends_on.push(dep.into());
        self
    }

    /// Builder: set lifecycle policy.
    pub fn with_lifecycle(mut self, lifecycle: LifecyclePolicy) -> Self {
        self.lifecycle = lifecycle;
        self
    }
}

// ---------------------------------------------------------------------------
// ResourceState
// ---------------------------------------------------------------------------

/// Lifecycle state of a provisioned resource.
///
/// ```text
///  Planned ──▶ Creating ──▶ Created ──▶ Updating ──▶ Created
///                  │                        │
///                  ▼                        ▼
///                Error                   Error
///
///  Created ──▶ Destroying ──▶ Destroyed
///                  │
///                  ▼
///                Error
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceState {
    /// Exists in the plan but has not been acted on yet.
    Planned,
    /// Currently being created by the provider.
    Creating,
    /// Successfully provisioned and operational.
    Created,
    /// Being updated in-place by the provider.
    Updating,
    /// Being torn down by the provider.
    Destroying,
    /// Successfully removed.
    Destroyed,
    /// A provider operation failed. The resource may be in a partial state.
    Error,
}

impl ResourceState {
    /// Returns `true` if the resource is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Created | Self::Destroyed | Self::Error)
    }

    /// Returns `true` if the resource is live and operational.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Created)
    }
}

impl fmt::Display for ResourceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Planned => "planned",
            Self::Creating => "creating",
            Self::Created => "created",
            Self::Updating => "updating",
            Self::Destroying => "destroying",
            Self::Destroyed => "destroyed",
            Self::Error => "error",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// Resource
// ---------------------------------------------------------------------------

/// A fully-tracked resource: spec + runtime state + provider outputs.
///
/// After a resource is created by a provider, it receives a `physical_id`
/// (the provider's own identifier for the actual resource) and may produce
/// outputs that other resources can reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    /// The declarative specification.
    pub spec: ResourceSpec,

    /// Current lifecycle state.
    pub state: ResourceState,

    /// Provider-assigned identifier for the physical resource. `None` until
    /// the resource has been successfully created.
    pub physical_id: Option<String>,

    /// Key-value outputs produced by the provider after creation or update.
    /// Other resources can reference these via `${resource.name.key}` syntax.
    pub outputs: HashMap<String, Value>,

    /// When the resource was first created.
    pub created_at: DateTime<Utc>,

    /// When the resource was last modified.
    pub updated_at: DateTime<Utc>,
}

impl Resource {
    /// Create a new resource from a spec in `Planned` state.
    pub fn from_spec(spec: ResourceSpec) -> Self {
        let now = Utc::now();
        Self {
            spec,
            state: ResourceState::Planned,
            physical_id: None,
            outputs: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Transition to a new state, updating the `updated_at` timestamp.
    pub fn transition(&mut self, new_state: ResourceState) {
        self.state = new_state;
        self.updated_at = Utc::now();
    }

    /// Assign a physical ID after provider creation.
    pub fn set_physical_id(&mut self, id: impl Into<String>) {
        self.physical_id = Some(id.into());
        self.updated_at = Utc::now();
    }

    /// Insert an output value.
    pub fn set_output(&mut self, key: impl Into<String>, value: Value) {
        self.outputs.insert(key.into(), value);
        self.updated_at = Utc::now();
    }

    /// Look up an output by key.
    pub fn get_output(&self, key: &str) -> Option<&Value> {
        self.outputs.get(key)
    }

    /// The resource's logical name (delegated from spec).
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The resource's type (delegated from spec).
    pub fn resource_type(&self) -> &ResourceType {
        &self.spec.type_
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{} ({}) [{}]",
            self.spec.type_, self.spec.name, self.spec.provider, self.state,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_type_display() {
        assert_eq!(ResourceType::AgentPool.to_string(), "agent_pool");
        assert_eq!(ResourceType::Channel.to_string(), "channel");
        assert_eq!(
            ResourceType::Custom("gpu_cluster".into()).to_string(),
            "custom:gpu_cluster"
        );
    }

    #[test]
    fn resource_spec_builder() {
        let spec = ResourceSpec::new(ResourceType::AgentPool, "researchers", "agent_pool")
            .with_config(serde_json::json!({"count": 5}))
            .with_dependency("network.main");

        assert_eq!(spec.name, "researchers");
        assert_eq!(spec.depends_on, vec!["network.main"]);
        assert_eq!(spec.config["count"], 5);
    }

    #[test]
    fn resource_lifecycle() {
        let spec = ResourceSpec::new(ResourceType::Channel, "data-pipe", "channel");
        let mut res = Resource::from_spec(spec);

        assert_eq!(res.state, ResourceState::Planned);
        assert!(res.physical_id.is_none());

        res.transition(ResourceState::Creating);
        assert_eq!(res.state, ResourceState::Creating);

        res.transition(ResourceState::Created);
        res.set_physical_id("chan-abc123");
        res.set_output("endpoint", Value::String("tcp://localhost:9090".into()));

        assert_eq!(res.state, ResourceState::Created);
        assert_eq!(res.physical_id.as_deref(), Some("chan-abc123"));
        assert_eq!(
            res.get_output("endpoint").unwrap(),
            &Value::String("tcp://localhost:9090".into())
        );
    }

    #[test]
    fn resource_state_predicates() {
        assert!(ResourceState::Created.is_live());
        assert!(!ResourceState::Planned.is_live());
        assert!(ResourceState::Destroyed.is_terminal());
        assert!(ResourceState::Error.is_terminal());
        assert!(!ResourceState::Creating.is_terminal());
    }

    #[test]
    fn resource_display() {
        let spec = ResourceSpec::new(ResourceType::Pipeline, "etl", "pipeline");
        let res = Resource::from_spec(spec);
        let display = res.to_string();
        assert!(display.contains("pipeline.etl"));
        assert!(display.contains("planned"));
    }

    #[test]
    fn lifecycle_policy_defaults() {
        let policy = LifecyclePolicy::default();
        assert!(!policy.create_before_destroy);
        assert!(!policy.prevent_destroy);
        assert!(policy.ignore_changes.is_empty());
    }

    #[test]
    fn resource_serde_roundtrip() {
        let spec = ResourceSpec::new(ResourceType::Storage, "logs", "storage")
            .with_config(serde_json::json!({"size_gb": 100}));
        let res = Resource::from_spec(spec);

        let json = serde_json::to_string(&res).unwrap();
        let back: Resource = serde_json::from_str(&json).unwrap();
        assert_eq!(back.spec.name, "logs");
        assert_eq!(back.spec.config["size_gb"], 100);
    }
}
