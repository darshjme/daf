//! Terraform-style agent topology provisioning
//!
//! This example demonstrates DAF's declarative infrastructure layer — the
//! same pattern Terraform uses for cloud resources, applied to agent
//! topologies. You define the desired state (pools of agents, channels
//! between them, resource limits), DAF computes the diff against current
//! state, shows you a plan, and applies it.
//!
//! Key concepts demonstrated:
//! - Defining agent pools with scaling and capability requirements
//! - Declaring channels (DDAL links) between pools
//! - Planning: computing the diff between desired and current state
//! - Applying: creating/updating/destroying resources to match desired state
//! - State management: persisting and loading provisioned resource state
//!
//! Run with:
//!   cargo run -p daf-example-infrastructure

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::info;

// ---------------------------------------------------------------------------
// Topology definition types — the "HCL" of DAF
// ---------------------------------------------------------------------------

/// A complete topology definition describing the desired agent infrastructure.
///
/// This is the equivalent of a Terraform configuration file. It declares
/// what pools of agents should exist, how they connect, and what resources
/// they need.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Topology {
    /// Human-readable name for this topology.
    name: String,
    /// Version for change tracking.
    version: String,
    /// Agent pools to provision.
    pools: Vec<AgentPool>,
    /// Channels connecting pools.
    channels: Vec<ChannelDef>,
    /// Global resource constraints.
    global_limits: GlobalLimits,
}

/// A pool of identical agents that can be scaled horizontally.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentPool {
    /// Pool identifier (e.g., "workers", "analyzers").
    name: String,
    /// Agent kind for all members of this pool.
    kind: String,
    /// Desired number of running agents.
    replicas: u32,
    /// Minimum replicas (for autoscaling).
    min_replicas: u32,
    /// Maximum replicas (for autoscaling).
    max_replicas: u32,
    /// Required capabilities each agent in the pool must have.
    capabilities: Vec<String>,
    /// Per-agent resource limits.
    resources: PoolResources,
    /// Labels for grouping and selection.
    labels: HashMap<String, String>,
}

/// Resource allocation per agent in a pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PoolResources {
    memory_mb: u32,
    cpu_ms: u32,
    max_connections: u32,
    max_queue_depth: u32,
}

/// A communication channel between two agent pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelDef {
    /// Channel name.
    name: String,
    /// Source pool.
    from: String,
    /// Destination pool.
    to: String,
    /// Buffer capacity.
    buffer_size: u32,
    /// Whether the channel is bidirectional.
    bidirectional: bool,
}

/// Global limits for the entire topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GlobalLimits {
    max_total_agents: u32,
    max_total_memory_mb: u32,
    max_total_channels: u32,
}

// ---------------------------------------------------------------------------
// State management — tracks what has been provisioned
// ---------------------------------------------------------------------------

/// The persisted state of all provisioned resources.
///
/// Equivalent to Terraform's state file. In production this would be stored
/// in a durable backend (RocksDB, S3, etc.). Here we keep it in memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProvisionState {
    /// Currently provisioned pools.
    pools: HashMap<String, ProvisionedPool>,
    /// Currently provisioned channels.
    channels: HashMap<String, ProvisionedChannel>,
    /// Serial number incremented on every apply.
    serial: u64,
    /// Timestamp of the last apply.
    last_applied: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProvisionedPool {
    name: String,
    kind: String,
    current_replicas: u32,
    desired_replicas: u32,
    agent_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProvisionedChannel {
    name: String,
    from: String,
    to: String,
    buffer_size: u32,
}

// ---------------------------------------------------------------------------
// Plan — the diff between desired and current state
// ---------------------------------------------------------------------------

/// A single planned change to the infrastructure.
#[derive(Debug, Clone)]
enum PlanAction {
    CreatePool {
        name: String,
        kind: String,
        replicas: u32,
    },
    UpdatePool {
        name: String,
        old_replicas: u32,
        new_replicas: u32,
    },
    DestroyPool {
        name: String,
        current_replicas: u32,
    },
    CreateChannel {
        name: String,
        from: String,
        to: String,
    },
    UpdateChannel {
        name: String,
        old_buffer: u32,
        new_buffer: u32,
    },
    DestroyChannel {
        name: String,
    },
}

impl fmt::Display for PlanAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreatePool { name, kind, replicas } => {
                write!(f, "  + pool.{name} ({kind}, {replicas} replicas)")
            }
            Self::UpdatePool { name, old_replicas, new_replicas } => {
                write!(f, "  ~ pool.{name} (replicas: {old_replicas} -> {new_replicas})")
            }
            Self::DestroyPool { name, current_replicas } => {
                write!(f, "  - pool.{name} ({current_replicas} agents will be terminated)")
            }
            Self::CreateChannel { name, from, to } => {
                write!(f, "  + channel.{name} ({from} -> {to})")
            }
            Self::UpdateChannel { name, old_buffer, new_buffer } => {
                write!(f, "  ~ channel.{name} (buffer: {old_buffer} -> {new_buffer})")
            }
            Self::DestroyChannel { name } => {
                write!(f, "  - channel.{name}")
            }
        }
    }
}

/// A complete execution plan showing all changes to be made.
#[derive(Debug)]
struct Plan {
    actions: Vec<PlanAction>,
    creates: usize,
    updates: usize,
    destroys: usize,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return writeln!(f, "No changes. Infrastructure is up-to-date.");
        }

        writeln!(f, "Execution Plan:")?;
        writeln!(f, "───────────────")?;
        for action in &self.actions {
            writeln!(f, "{action}")?;
        }
        writeln!(f, "───────────────")?;
        writeln!(
            f,
            "Plan: {creates} to add, {updates} to change, {destroys} to destroy.",
            creates = self.creates,
            updates = self.updates,
            destroys = self.destroys,
        )
    }
}

// ---------------------------------------------------------------------------
// Provisioner — computes plans and applies them
// ---------------------------------------------------------------------------

struct Provisioner {
    state: ProvisionState,
}

impl Provisioner {
    fn new() -> Self {
        Self {
            state: ProvisionState::default(),
        }
    }

    /// Load state from a previous session. In production this would read
    /// from disk or a remote backend.
    fn with_state(state: ProvisionState) -> Self {
        Self { state }
    }

    /// Compute the diff between the desired topology and current state.
    fn plan(&self, topology: &Topology) -> Plan {
        let mut actions = Vec::new();
        let mut creates = 0usize;
        let mut updates = 0usize;
        let mut destroys = 0usize;

        // --- Pool diffs ---

        // Check for new or changed pools
        for pool in &topology.pools {
            match self.state.pools.get(&pool.name) {
                None => {
                    actions.push(PlanAction::CreatePool {
                        name: pool.name.clone(),
                        kind: pool.kind.clone(),
                        replicas: pool.replicas,
                    });
                    creates += 1;
                }
                Some(existing) if existing.desired_replicas != pool.replicas => {
                    actions.push(PlanAction::UpdatePool {
                        name: pool.name.clone(),
                        old_replicas: existing.desired_replicas,
                        new_replicas: pool.replicas,
                    });
                    updates += 1;
                }
                Some(_) => {} // No change
            }
        }

        // Check for pools that should be destroyed (exist in state but not in topology)
        let desired_pool_names: Vec<&str> = topology.pools.iter().map(|p| p.name.as_str()).collect();
        for (name, pool) in &self.state.pools {
            if !desired_pool_names.contains(&name.as_str()) {
                actions.push(PlanAction::DestroyPool {
                    name: name.clone(),
                    current_replicas: pool.current_replicas,
                });
                destroys += 1;
            }
        }

        // --- Channel diffs ---

        for channel in &topology.channels {
            match self.state.channels.get(&channel.name) {
                None => {
                    actions.push(PlanAction::CreateChannel {
                        name: channel.name.clone(),
                        from: channel.from.clone(),
                        to: channel.to.clone(),
                    });
                    creates += 1;
                }
                Some(existing) if existing.buffer_size != channel.buffer_size => {
                    actions.push(PlanAction::UpdateChannel {
                        name: channel.name.clone(),
                        old_buffer: existing.buffer_size,
                        new_buffer: channel.buffer_size,
                    });
                    updates += 1;
                }
                Some(_) => {}
            }
        }

        let desired_channel_names: Vec<&str> =
            topology.channels.iter().map(|c| c.name.as_str()).collect();
        for name in self.state.channels.keys() {
            if !desired_channel_names.contains(&name.as_str()) {
                actions.push(PlanAction::DestroyChannel { name: name.clone() });
                destroys += 1;
            }
        }

        Plan {
            actions,
            creates,
            updates,
            destroys,
        }
    }

    /// Apply the plan, mutating the provisioned state.
    fn apply(&mut self, plan: &Plan) -> Result<(), String> {
        for action in &plan.actions {
            match action {
                PlanAction::CreatePool { name, kind, replicas } => {
                    info!(pool = name, replicas, "Creating agent pool");
                    let agent_ids: Vec<String> = (0..*replicas)
                        .map(|i| format!("{name}-{i}"))
                        .collect();
                    self.state.pools.insert(
                        name.clone(),
                        ProvisionedPool {
                            name: name.clone(),
                            kind: kind.clone(),
                            current_replicas: *replicas,
                            desired_replicas: *replicas,
                            agent_ids,
                        },
                    );
                }
                PlanAction::UpdatePool { name, new_replicas, .. } => {
                    info!(pool = name, new_replicas, "Scaling agent pool");
                    if let Some(pool) = self.state.pools.get_mut(name) {
                        // Scale up: add new agent IDs
                        while (pool.agent_ids.len() as u32) < *new_replicas {
                            let idx = pool.agent_ids.len();
                            pool.agent_ids.push(format!("{name}-{idx}"));
                        }
                        // Scale down: remove excess agent IDs
                        pool.agent_ids.truncate(*new_replicas as usize);
                        pool.current_replicas = *new_replicas;
                        pool.desired_replicas = *new_replicas;
                    }
                }
                PlanAction::DestroyPool { name, .. } => {
                    info!(pool = name, "Destroying agent pool");
                    self.state.pools.remove(name);
                }
                PlanAction::CreateChannel { name, from, to } => {
                    info!(channel = name, from, to, "Creating channel");
                    self.state.channels.insert(
                        name.clone(),
                        ProvisionedChannel {
                            name: name.clone(),
                            from: from.clone(),
                            to: to.clone(),
                            buffer_size: 64, // default
                        },
                    );
                }
                PlanAction::UpdateChannel { name, new_buffer, .. } => {
                    info!(channel = name, new_buffer, "Updating channel");
                    if let Some(ch) = self.state.channels.get_mut(name) {
                        ch.buffer_size = *new_buffer;
                    }
                }
                PlanAction::DestroyChannel { name } => {
                    info!(channel = name, "Destroying channel");
                    self.state.channels.remove(name);
                }
            }
        }

        self.state.serial += 1;
        self.state.last_applied = Some(chrono::Utc::now().to_rfc3339());

        Ok(())
    }

    /// Serialize state for persistence.
    fn export_state(&self) -> String {
        serde_json::to_string_pretty(&self.state).unwrap()
    }
}

// ---------------------------------------------------------------------------
// main — define topology, plan, show diff, apply
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("=== DAF Infrastructure Provisioning Example ===");

    // -----------------------------------------------------------------------
    // 1. Define the desired topology.
    // -----------------------------------------------------------------------

    let topology = Topology {
        name: "research-pipeline".into(),
        version: "1.0.0".into(),
        pools: vec![
            AgentPool {
                name: "researchers".into(),
                kind: "specialist".into(),
                replicas: 3,
                min_replicas: 1,
                max_replicas: 10,
                capabilities: vec!["research".into(), "web_scraping".into()],
                resources: PoolResources {
                    memory_mb: 256,
                    cpu_ms: 300_000,
                    max_connections: 32,
                    max_queue_depth: 128,
                },
                labels: HashMap::from([
                    ("stage".into(), "1".into()),
                    ("team".into(), "data".into()),
                ]),
            },
            AgentPool {
                name: "analyzers".into(),
                kind: "specialist".into(),
                replicas: 2,
                min_replicas: 1,
                max_replicas: 5,
                capabilities: vec!["analysis".into(), "risk_assessment".into()],
                resources: PoolResources {
                    memory_mb: 512,
                    cpu_ms: 600_000,
                    max_connections: 16,
                    max_queue_depth: 64,
                },
                labels: HashMap::from([
                    ("stage".into(), "2".into()),
                    ("team".into(), "ml".into()),
                ]),
            },
            AgentPool {
                name: "reporters".into(),
                kind: "worker".into(),
                replicas: 1,
                min_replicas: 1,
                max_replicas: 3,
                capabilities: vec!["reporting".into()],
                resources: PoolResources {
                    memory_mb: 128,
                    cpu_ms: 120_000,
                    max_connections: 8,
                    max_queue_depth: 32,
                },
                labels: HashMap::from([
                    ("stage".into(), "3".into()),
                    ("team".into(), "product".into()),
                ]),
            },
        ],
        channels: vec![
            ChannelDef {
                name: "research-to-analysis".into(),
                from: "researchers".into(),
                to: "analyzers".into(),
                buffer_size: 64,
                bidirectional: false,
            },
            ChannelDef {
                name: "analysis-to-report".into(),
                from: "analyzers".into(),
                to: "reporters".into(),
                buffer_size: 32,
                bidirectional: false,
            },
        ],
        global_limits: GlobalLimits {
            max_total_agents: 20,
            max_total_memory_mb: 4096,
            max_total_channels: 10,
        },
    };

    info!(
        name = topology.name,
        pools = topology.pools.len(),
        channels = topology.channels.len(),
        "Topology defined"
    );

    // -----------------------------------------------------------------------
    // 2. Initial provision — everything is new.
    // -----------------------------------------------------------------------

    let mut provisioner = Provisioner::new();

    info!("\n--- Initial Plan (fresh state) ---");
    let plan = provisioner.plan(&topology);
    println!("\n{plan}");

    info!("Applying initial plan...");
    provisioner.apply(&plan)?;
    info!(serial = provisioner.state.serial, "State updated");

    println!("\nProvisioned State:");
    println!("{}", provisioner.export_state());

    // -----------------------------------------------------------------------
    // 3. Modify the topology — scale up researchers, remove reporters.
    // -----------------------------------------------------------------------

    info!("\n--- Updating topology ---");

    let mut updated_topology = topology.clone();

    // Scale researchers from 3 to 5
    updated_topology.pools[0].replicas = 5;

    // Remove the reporters pool entirely
    updated_topology.pools.retain(|p| p.name != "reporters");

    // Remove the channel to reporters
    updated_topology.channels.retain(|c| c.name != "analysis-to-report");

    // Add a new pool: reviewers
    updated_topology.pools.push(AgentPool {
        name: "reviewers".into(),
        kind: "specialist".into(),
        replicas: 2,
        min_replicas: 1,
        max_replicas: 4,
        capabilities: vec!["review".into(), "approval".into()],
        resources: PoolResources {
            memory_mb: 256,
            cpu_ms: 180_000,
            max_connections: 16,
            max_queue_depth: 32,
        },
        labels: HashMap::from([("stage".into(), "3".into())]),
    });

    // Add a channel to reviewers
    updated_topology.channels.push(ChannelDef {
        name: "analysis-to-review".into(),
        from: "analyzers".into(),
        to: "reviewers".into(),
        buffer_size: 32,
        bidirectional: true,
    });

    let plan = provisioner.plan(&updated_topology);
    println!("\n{plan}");

    info!("Applying updated plan...");
    provisioner.apply(&plan)?;
    info!(serial = provisioner.state.serial, "State updated");

    println!("\nFinal State:");
    println!("{}", provisioner.export_state());

    // -----------------------------------------------------------------------
    // 4. No-op plan — verify that re-planning with same config shows no diff.
    // -----------------------------------------------------------------------

    info!("\n--- No-op Plan (should be empty) ---");
    let noop_plan = provisioner.plan(&updated_topology);
    println!("\n{noop_plan}");
    assert!(noop_plan.is_empty(), "Expected no changes on re-plan");

    info!("=== Infrastructure provisioning example complete ===");

    Ok(())
}
