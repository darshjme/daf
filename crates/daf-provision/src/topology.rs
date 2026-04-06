//! Topology definitions for agent infrastructure.
//!
//! A topology describes the desired shape of an agent deployment: which agent
//! pools to create, how they communicate, and what pipelines connect them.
//! Topologies can be authored as Rust structs or parsed from YAML configuration
//! files.
//!
//! The topology module sits between the operator's intent ("I want 5 researcher
//! agents feeding into 3 builder agents") and the low-level [`ResourceSpec`]s
//! that the planner consumes.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::resource::{ResourceSpec, ResourceType};

// ---------------------------------------------------------------------------
// AgentSpec
// ---------------------------------------------------------------------------

/// Specification for an agent pool within a topology.
///
/// An `AgentSpec` declares a logical pool of homogeneous agents. The provisioner
/// translates each `AgentSpec` into an `AgentPool` resource with the appropriate
/// provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Logical name for this agent pool (e.g., `"researchers"`).
    pub name: String,
    /// Number of agents to provision in this pool.
    pub count: u32,
    /// The agent kind (maps to `AgentKind` in daf-core).
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Capabilities this agent pool advertises.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Maximum memory bytes per agent.
    pub max_memory_bytes: Option<u64>,
    /// Additional provider-specific configuration.
    #[serde(default)]
    pub extra: HashMap<String, Value>,
}

fn default_kind() -> String {
    "worker".to_string()
}

impl AgentSpec {
    /// Create a minimal agent spec.
    pub fn new(name: impl Into<String>, count: u32) -> Self {
        Self {
            name: name.into(),
            count,
            kind: "worker".to_string(),
            capabilities: Vec::new(),
            max_memory_bytes: None,
            extra: HashMap::new(),
        }
    }

    /// Builder: set the agent kind.
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }

    /// Builder: add a capability.
    pub fn with_capability(mut self, cap: impl Into<String>) -> Self {
        self.capabilities.push(cap.into());
        self
    }

    /// Convert to a [`ResourceSpec`] for the planner.
    pub fn to_resource_spec(&self) -> ResourceSpec {
        let mut config = serde_json::json!({
            "count": self.count,
            "kind": self.kind,
            "capabilities": self.capabilities,
        });

        if let Some(mem) = self.max_memory_bytes {
            config["max_memory_bytes"] = serde_json::json!(mem);
        }

        // Merge extra fields into the config object.
        if let Some(obj) = config.as_object_mut() {
            for (k, v) in &self.extra {
                obj.insert(k.clone(), v.clone());
            }
        }

        ResourceSpec::new(ResourceType::AgentPool, &self.name, "agent_pool").with_config(config)
    }
}

// ---------------------------------------------------------------------------
// ChannelSpec
// ---------------------------------------------------------------------------

/// Specification for a communication channel between agent pools.
///
/// Channels define the edges in the agent topology graph. Each channel connects
/// a source pool (or agent) to a target, optionally specifying the protocol
/// and buffer sizing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSpec {
    /// Logical name for this channel.
    pub name: String,
    /// Source agent pool or agent name.
    pub from: String,
    /// Target agent pool or agent name.
    pub to: String,
    /// Communication protocol (e.g., `"direct"`, `"pubsub"`, `"broadcast"`).
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// Message buffer size.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,
}

fn default_protocol() -> String {
    "direct".to_string()
}

fn default_buffer_size() -> u32 {
    1024
}

impl ChannelSpec {
    /// Create a new channel between two endpoints.
    pub fn new(name: impl Into<String>, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            from: from.into(),
            to: to.into(),
            protocol: "direct".to_string(),
            buffer_size: 1024,
        }
    }

    /// Builder: set the protocol.
    pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
        self.protocol = protocol.into();
        self
    }

    /// Builder: set the buffer size.
    pub fn with_buffer_size(mut self, size: u32) -> Self {
        self.buffer_size = size;
        self
    }

    /// Convert to a [`ResourceSpec`] for the planner.
    ///
    /// The returned spec automatically depends on both the `from` and `to`
    /// agent pools so that channels are created after their endpoints.
    pub fn to_resource_spec(&self) -> ResourceSpec {
        let config = serde_json::json!({
            "from": self.from,
            "to": self.to,
            "protocol": self.protocol,
            "buffer_size": self.buffer_size,
        });

        ResourceSpec::new(ResourceType::Channel, &self.name, "channel")
            .with_config(config)
            .with_dependency(&self.from)
            .with_dependency(&self.to)
    }
}

// ---------------------------------------------------------------------------
// PipelineSpec
// ---------------------------------------------------------------------------

/// Specification for an ordered multi-stage pipeline.
///
/// Pipelines define sequential processing: each stage is backed by an agent pool
/// and stages execute in declared order. Stages can optionally run in parallel
/// within themselves (fan-out).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSpec {
    /// Logical name for this pipeline.
    pub name: String,
    /// Ordered list of stages.
    pub stages: Vec<StageSpec>,
}

/// A single stage within a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSpec {
    /// Stage name.
    pub name: String,
    /// The agent pool that executes this stage.
    pub agent_pool: String,
    /// Whether the stage runs tasks in parallel across pool agents.
    #[serde(default)]
    pub parallel: bool,
}

impl PipelineSpec {
    /// Create a new pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            stages: Vec::new(),
        }
    }

    /// Add a stage to the pipeline.
    pub fn with_stage(
        mut self,
        name: impl Into<String>,
        agent_pool: impl Into<String>,
        parallel: bool,
    ) -> Self {
        self.stages.push(StageSpec {
            name: name.into(),
            agent_pool: agent_pool.into(),
            parallel,
        });
        self
    }

    /// Convert to a [`ResourceSpec`] for the planner.
    ///
    /// The returned spec depends on all agent pools referenced by stages.
    pub fn to_resource_spec(&self) -> ResourceSpec {
        let stages_json: Vec<Value> = self
            .stages
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "agent_pool": s.agent_pool,
                    "parallel": s.parallel,
                })
            })
            .collect();

        let config = serde_json::json!({ "stages": stages_json });

        let mut spec =
            ResourceSpec::new(ResourceType::Pipeline, &self.name, "pipeline").with_config(config);

        // Depend on each referenced agent pool.
        for stage in &self.stages {
            spec = spec.with_dependency(&stage.agent_pool);
        }

        spec
    }
}

// ---------------------------------------------------------------------------
// Topology
// ---------------------------------------------------------------------------

/// A complete topology declaration describing an agent deployment.
///
/// A topology bundles agent pools, channels, and pipelines into a single
/// deployable unit. The [`Topology::to_resource_specs`] method flattens
/// the topology into a list of [`ResourceSpec`]s that the planner can diff
/// against the current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    /// Human-readable name for this topology.
    pub name: String,
    /// Optional description.
    #[serde(default)]
    pub description: String,
    /// Agent pool specifications.
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
    /// Channel specifications.
    #[serde(default)]
    pub channels: Vec<ChannelSpec>,
    /// Pipeline specifications.
    #[serde(default)]
    pub pipelines: Vec<PipelineSpec>,
}

impl Topology {
    /// Create a new empty topology.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            agents: Vec::new(),
            channels: Vec::new(),
            pipelines: Vec::new(),
        }
    }

    /// Add an agent pool specification.
    pub fn with_agent(mut self, agent: AgentSpec) -> Self {
        self.agents.push(agent);
        self
    }

    /// Add a channel specification.
    pub fn with_channel(mut self, channel: ChannelSpec) -> Self {
        self.channels.push(channel);
        self
    }

    /// Add a pipeline specification.
    pub fn with_pipeline(mut self, pipeline: PipelineSpec) -> Self {
        self.pipelines.push(pipeline);
        self
    }

    /// Flatten the topology into a list of [`ResourceSpec`]s for the planner.
    ///
    /// The order is: agents first, then channels, then pipelines. Dependencies
    /// within each spec ensure correct topological ordering during plan
    /// execution.
    pub fn to_resource_specs(&self) -> Vec<ResourceSpec> {
        let mut specs = Vec::new();

        for agent in &self.agents {
            specs.push(agent.to_resource_spec());
        }

        for channel in &self.channels {
            specs.push(channel.to_resource_spec());
        }

        for pipeline in &self.pipelines {
            specs.push(pipeline.to_resource_spec());
        }

        specs
    }

    /// Validate the topology for internal consistency.
    ///
    /// Checks that all channel endpoints and pipeline stage pools reference
    /// agent pools that exist in this topology. Returns a list of error
    /// messages (empty = valid).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let agent_names: std::collections::HashSet<&str> =
            self.agents.iter().map(|a| a.name.as_str()).collect();

        for channel in &self.channels {
            if !agent_names.contains(channel.from.as_str()) {
                errors.push(format!(
                    "channel '{}': source '{}' is not a known agent pool",
                    channel.name, channel.from
                ));
            }
            if !agent_names.contains(channel.to.as_str()) {
                errors.push(format!(
                    "channel '{}': target '{}' is not a known agent pool",
                    channel.name, channel.to
                ));
            }
        }

        for pipeline in &self.pipelines {
            for stage in &pipeline.stages {
                if !agent_names.contains(stage.agent_pool.as_str()) {
                    errors.push(format!(
                        "pipeline '{}' stage '{}': agent_pool '{}' is not a known agent pool",
                        pipeline.name, stage.name, stage.agent_pool
                    ));
                }
            }
        }

        // Check for duplicate names across all resource types.
        let mut seen = std::collections::HashSet::new();
        for name in self
            .agents
            .iter()
            .map(|a| &a.name)
            .chain(self.channels.iter().map(|c| &c.name))
            .chain(self.pipelines.iter().map(|p| &p.name))
        {
            if !seen.insert(name) {
                errors.push(format!("duplicate resource name: '{name}'"));
            }
        }

        errors
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_spec_to_resource() {
        let spec = AgentSpec::new("workers", 5)
            .with_kind("specialist")
            .with_capability("code_review");

        let resource = spec.to_resource_spec();
        assert_eq!(resource.name, "workers");
        assert_eq!(resource.provider, "agent_pool");
        assert_eq!(resource.config["count"], 5);
        assert_eq!(resource.config["kind"], "specialist");
    }

    #[test]
    fn channel_spec_to_resource() {
        let spec = ChannelSpec::new("data-pipe", "researchers", "builders").with_protocol("pubsub");

        let resource = spec.to_resource_spec();
        assert_eq!(resource.name, "data-pipe");
        assert_eq!(resource.provider, "channel");
        assert_eq!(resource.config["from"], "researchers");
        assert_eq!(resource.config["protocol"], "pubsub");
        assert_eq!(resource.depends_on, vec!["researchers", "builders"]);
    }

    #[test]
    fn pipeline_spec_to_resource() {
        let spec = PipelineSpec::new("etl")
            .with_stage("extract", "scrapers", false)
            .with_stage("transform", "processors", true)
            .with_stage("load", "writers", false);

        let resource = spec.to_resource_spec();
        assert_eq!(resource.name, "etl");
        assert_eq!(resource.config["stages"].as_array().unwrap().len(), 3);
        assert!(resource.depends_on.contains(&"scrapers".to_string()));
        assert!(resource.depends_on.contains(&"processors".to_string()));
    }

    #[test]
    fn topology_to_resource_specs() {
        let topo = Topology::new("test-cluster")
            .with_agent(AgentSpec::new("researchers", 3))
            .with_agent(AgentSpec::new("builders", 2))
            .with_channel(ChannelSpec::new("pipe", "researchers", "builders"));

        let specs = topo.to_resource_specs();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "researchers");
        assert_eq!(specs[1].name, "builders");
        assert_eq!(specs[2].name, "pipe");
    }

    #[test]
    fn topology_validate_valid() {
        let topo = Topology::new("valid")
            .with_agent(AgentSpec::new("a", 1))
            .with_agent(AgentSpec::new("b", 1))
            .with_channel(ChannelSpec::new("c", "a", "b"));

        let errors = topo.validate();
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    #[test]
    fn topology_validate_missing_endpoint() {
        let topo = Topology::new("invalid")
            .with_agent(AgentSpec::new("a", 1))
            .with_channel(ChannelSpec::new("c", "a", "missing"));

        let errors = topo.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("missing"));
    }

    #[test]
    fn topology_validate_duplicate_names() {
        let topo = Topology::new("dups")
            .with_agent(AgentSpec::new("x", 1))
            .with_channel(ChannelSpec::new("x", "x", "x"));

        let errors = topo.validate();
        assert!(errors.iter().any(|e| e.contains("duplicate")));
    }

    #[test]
    fn topology_serde_roundtrip() {
        let topo = Topology::new("serde-test")
            .with_agent(AgentSpec::new("pool", 3))
            .with_channel(ChannelSpec::new("ch", "pool", "pool"));

        let json = serde_json::to_string(&topo).unwrap();
        let back: Topology = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "serde-test");
        assert_eq!(back.agents.len(), 1);
        assert_eq!(back.channels.len(), 1);
    }
}
