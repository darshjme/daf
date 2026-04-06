//! # DAF Provision
//!
//! Declarative infrastructure provisioning for agent topologies.
//!
//! This crate provides a Terraform-inspired workflow for declaring agent
//! deployments — pools of agents, communication channels, and multi-stage
//! pipelines — and having DAF provision them automatically.
//!
//! # Workflow
//!
//! ```text
//!  Topology ──▶ ResourceSpecs ──▶ Planner ──▶ Plan ──▶ Applier ──▶ State
//!    (YAML)       (desired)        (diff)    (changes)  (execute)  (actual)
//! ```
//!
//! 1. Define a [`Topology`] describing the agent deployment you want.
//! 2. Convert it to [`ResourceSpec`]s with [`Topology::to_resource_specs`].
//! 3. Run the [`Planner`] to diff desired specs against current [`ProvisionState`].
//! 4. Review the [`Plan`] (creates, updates, destroys).
//! 5. Execute with the [`Applier`] to converge infrastructure to desired state.
//! 6. Collect [`Output`]s from provisioned resources.
//!
//! # Example
//!
//! ```rust,ignore
//! use daf_provision::topology::{Topology, AgentSpec, ChannelSpec, PipelineSpec};
//! use daf_provision::plan::Planner;
//! use daf_provision::apply::{Applier, ApplyOptions};
//! use daf_provision::state::InMemoryStateStore;
//! use daf_provision::provider::ProviderRegistry;
//!
//! // Declare the topology.
//! let topo = Topology::new("research-pipeline")
//!     .with_agent(AgentSpec::new("researchers", 5).with_kind("specialist"))
//!     .with_agent(AgentSpec::new("builders", 3))
//!     .with_channel(ChannelSpec::new("findings", "researchers", "builders"))
//!     .with_pipeline(
//!         PipelineSpec::new("main")
//!             .with_stage("research", "researchers", false)
//!             .with_stage("build", "builders", true),
//!     );
//!
//! // Convert to resource specs and plan.
//! let specs = topo.to_resource_specs();
//! let state = store.load().await?;
//! let plan = Planner::plan(&state, &specs)?;
//! println!("{plan}");
//!
//! // Apply the plan.
//! let registry = ProviderRegistry::with_builtins();
//! let applier = Applier::with_defaults(Arc::new(registry));
//! let result = applier.apply(&plan, &mut state, &store).await?;
//! ```

pub mod apply;
pub mod output;
pub mod plan;
pub mod provider;
pub mod resource;
pub mod state;
pub mod topology;

// Re-export the most commonly used types at crate root.
pub use apply::{Applier, ApplyOptions, ApplyResult, ApplyStatus};
pub use output::{Output, OutputCollector};
pub use plan::{Action, Plan, Planner, ResourceChange};
pub use provider::{
    AgentPoolProvider, ChannelProvider, PipelineProvider, Provider, ProviderRegistry,
    ProviderResult, ProviderSchema,
};
pub use resource::{LifecyclePolicy, Resource, ResourceSpec, ResourceState, ResourceType};
pub use state::{FileStateStore, InMemoryStateStore, ProvisionState, StateStore};
pub use topology::{AgentSpec, ChannelSpec, PipelineSpec, Topology};
