//! Specialist routing — the gstack pattern for task-to-agent matching.
//!
//! The specialist router is the brain behind "which agent handles this task?"
//! It considers three factors when making routing decisions:
//!
//! 1. **Capability matching** — does the agent have the skills the task requires?
//! 2. **Load balancing** — distribute work evenly across agents of the same role.
//! 3. **Affinity** — prefer routing related tasks to the same agent so it retains
//!    context (fewer handoffs = faster execution).
//!
//! This module implements the gstack specialist model where each agent plays a
//! defined role (Planner, Builder, Tester, etc.) and tasks are routed to the
//! best-fit specialist rather than round-robined to generic workers.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

use daf_core::error::{DafError, DafResult};
use daf_core::{AgentCapability, AgentId, AgentManifest, AgentStatus};
use daf_graph::node::TaskSpec;

// ---------------------------------------------------------------------------
// SpecialistRole
// ---------------------------------------------------------------------------

/// Predefined specialist roles inspired by the gstack methodology.
///
/// Each role maps to a class of capabilities. The router uses roles as a
/// coarse-grained filter before doing fine-grained capability matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialistRole {
    /// Strategic planning — decomposes goals into tasks, decides approach.
    Planner,
    /// Information gathering — reads docs, searches codebases, fetches context.
    Researcher,
    /// Code generation and implementation — writes the actual code.
    Builder,
    /// Quality assurance — runs tests, verifies correctness, checks coverage.
    Tester,
    /// Code review and quality gates — reviews PRs, enforces standards.
    Reviewer,
    /// Release engineering — builds, packages, deploys artifacts.
    Deployer,
    /// Observability — watches metrics, detects anomalies, alerts on issues.
    Monitor,
    /// Custom specialist role not covered by the predefined set.
    Custom(String),
}

impl SpecialistRole {
    /// The canonical capabilities typically associated with this role.
    ///
    /// These are used as hints during routing — an agent doesn't need to
    /// match all of them, but matching more increases its routing score.
    pub fn canonical_capabilities(&self) -> Vec<&'static str> {
        match self {
            Self::Planner => vec!["planning", "decomposition", "strategy", "architecture"],
            Self::Researcher => vec!["research", "search", "analysis", "documentation"],
            Self::Builder => vec!["code_generation", "implementation", "refactoring"],
            Self::Tester => vec!["testing", "qa", "coverage", "fuzzing"],
            Self::Reviewer => vec!["code_review", "security_review", "style_check"],
            Self::Deployer => vec!["deployment", "ci_cd", "infrastructure", "packaging"],
            Self::Monitor => vec!["monitoring", "alerting", "observability", "metrics"],
            Self::Custom(_) => vec![],
        }
    }

    /// Try to infer the most likely role from a set of capability names.
    pub fn infer_from_capabilities(capabilities: &[AgentCapability]) -> Self {
        let cap_names: Vec<&str> = capabilities.iter().map(|c| c.name.as_str()).collect();

        let roles = [
            Self::Planner,
            Self::Researcher,
            Self::Builder,
            Self::Tester,
            Self::Reviewer,
            Self::Deployer,
            Self::Monitor,
        ];

        let mut best_role = Self::Builder; // default
        let mut best_score = 0usize;

        for role in &roles {
            let score = role
                .canonical_capabilities()
                .iter()
                .filter(|&&c| cap_names.contains(&c))
                .count();
            if score > best_score {
                best_score = score;
                best_role = role.clone();
            }
        }

        best_role
    }
}

impl fmt::Display for SpecialistRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planner => write!(f, "planner"),
            Self::Researcher => write!(f, "researcher"),
            Self::Builder => write!(f, "builder"),
            Self::Tester => write!(f, "tester"),
            Self::Reviewer => write!(f, "reviewer"),
            Self::Deployer => write!(f, "deployer"),
            Self::Monitor => write!(f, "monitor"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentLoad
// ---------------------------------------------------------------------------

/// Tracks the current load on a single agent for routing decisions.
#[derive(Debug)]
struct AgentLoad {
    /// Number of tasks currently assigned to this agent.
    active_tasks: AtomicU64,
    /// Total tasks this agent has completed (for historical load balancing).
    total_completed: AtomicU64,
    /// When the agent was last assigned a task.
    last_assigned: RwLock<DateTime<Utc>>,
}

impl AgentLoad {
    fn new() -> Self {
        Self {
            active_tasks: AtomicU64::new(0),
            total_completed: AtomicU64::new(0),
            last_assigned: RwLock::new(Utc::now()),
        }
    }

    fn active(&self) -> u64 {
        self.active_tasks.load(Ordering::Relaxed)
    }

    fn assign(&self) {
        self.active_tasks.fetch_add(1, Ordering::Relaxed);
        *self.last_assigned.write() = Utc::now();
    }

    fn complete(&self) {
        self.active_tasks.fetch_sub(1, Ordering::Relaxed);
        self.total_completed.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// AffinityEntry
// ---------------------------------------------------------------------------

/// Records a task-context affinity: this agent has previously worked on tasks
/// with this context key, so routing similar tasks to it preserves continuity.
#[derive(Debug, Clone)]
struct AffinityEntry {
    agent_id: AgentId,
    #[allow(dead_code)]
    context_key: String,
    last_used: DateTime<Utc>,
    strength: f64, // 0.0..1.0, decays over time
}

// ---------------------------------------------------------------------------
// RoutingDecision
// ---------------------------------------------------------------------------

/// The outcome of a routing decision — which agent was selected and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// The selected agent.
    pub agent_id: AgentId,
    /// The role the agent is filling for this task.
    pub role: SpecialistRole,
    /// Composite routing score (higher = better fit).
    pub score: f64,
    /// Breakdown of the score components.
    pub score_breakdown: ScoreBreakdown,
}

/// Individual components of the routing score for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    /// Score from capability matching (0.0..1.0).
    pub capability_score: f64,
    /// Score from load balancing (0.0..1.0, higher = less loaded).
    pub load_score: f64,
    /// Score from context affinity (0.0..1.0).
    pub affinity_score: f64,
}

// ---------------------------------------------------------------------------
// SpecialistRouter
// ---------------------------------------------------------------------------

/// Routes tasks to the best available specialist agent.
///
/// The router maintains an internal registry of agents, their capabilities,
/// their current load, and their context affinities. When a task arrives,
/// it scores all eligible agents and picks the best one.
///
/// # Scoring Formula
///
/// ```text
/// score = (capability_weight * capability_score)
///       + (load_weight * load_score)
///       + (affinity_weight * affinity_score)
/// ```
///
/// Weights are configurable. The default weighting prioritizes capability
/// match (0.5) over load balance (0.3) and affinity (0.2).
pub struct SpecialistRouter {
    /// Registered agents and their manifests.
    agents: DashMap<AgentId, AgentManifest>,
    /// Current load per agent.
    load: DashMap<AgentId, AgentLoad>,
    /// Agent status tracking.
    status: DashMap<AgentId, AgentStatus>,
    /// Context affinity table: context_key -> Vec<AffinityEntry>.
    affinities: DashMap<String, Vec<AffinityEntry>>,
    /// Role-to-agent index for fast lookups.
    role_index: DashMap<SpecialistRole, Vec<AgentId>>,
    /// Scoring weights.
    capability_weight: f64,
    load_weight: f64,
    affinity_weight: f64,
}

impl SpecialistRouter {
    /// Create a new router with default scoring weights.
    pub fn new() -> Self {
        Self {
            agents: DashMap::new(),
            load: DashMap::new(),
            status: DashMap::new(),
            affinities: DashMap::new(),
            role_index: DashMap::new(),
            capability_weight: 0.5,
            load_weight: 0.3,
            affinity_weight: 0.2,
        }
    }

    /// Create a router with custom scoring weights.
    ///
    /// Weights are normalized internally, so `(1.0, 1.0, 1.0)` is equivalent
    /// to `(0.33, 0.33, 0.33)`.
    pub fn with_weights(capability: f64, load: f64, affinity: f64) -> Self {
        let total = capability + load + affinity;
        let (cw, lw, aw) = if total > 0.0 {
            (capability / total, load / total, affinity / total)
        } else {
            (0.5, 0.3, 0.2)
        };
        Self {
            capability_weight: cw,
            load_weight: lw,
            affinity_weight: aw,
            ..Self::new()
        }
    }

    /// Register an agent with the router. The router extracts capabilities
    /// from the manifest and indexes the agent by inferred role.
    pub fn register_agent(&self, manifest: AgentManifest) {
        let agent_id = manifest.id;
        let role = SpecialistRole::infer_from_capabilities(&manifest.capabilities);

        debug!(
            agent = %agent_id,
            role = %role,
            caps = ?manifest.capabilities.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "registering agent with specialist router"
        );

        self.role_index
            .entry(role)
            .or_default()
            .push(agent_id);

        self.load.insert(agent_id, AgentLoad::new());
        self.status.insert(agent_id, AgentStatus::Idle);
        self.agents.insert(agent_id, manifest);
    }

    /// Remove an agent from the router.
    pub fn deregister_agent(&self, agent_id: &AgentId) {
        self.agents.remove(agent_id);
        self.load.remove(agent_id);
        self.status.remove(agent_id);

        // Clean up role index.
        for mut entry in self.role_index.iter_mut() {
            entry.value_mut().retain(|id| id != agent_id);
        }
    }

    /// Update an agent's status.
    pub fn update_status(&self, agent_id: &AgentId, status: AgentStatus) {
        self.status.insert(*agent_id, status);
    }

    /// Record that an agent completed a task, updating load counters.
    pub fn record_task_completion(&self, agent_id: &AgentId) {
        if let Some(load) = self.load.get(agent_id) {
            load.complete();
        }
    }

    /// Record a context affinity — this agent worked on a task with the
    /// given context key, so future similar tasks should prefer it.
    pub fn record_affinity(&self, agent_id: AgentId, context_key: String) {
        let entry = AffinityEntry {
            agent_id,
            context_key: context_key.clone(),
            last_used: Utc::now(),
            strength: 1.0,
        };

        self.affinities
            .entry(context_key)
            .or_default()
            .push(entry);
    }

    /// Route a task to the best available specialist.
    ///
    /// Returns the routing decision with full score breakdown, or an error
    /// if no eligible agent is available.
    #[instrument(skip(self, task), fields(task_type = %task.task_type))]
    pub fn route_task(
        &self,
        task: &TaskSpec,
        required_capabilities: &[String],
        context_key: Option<&str>,
    ) -> DafResult<RoutingDecision> {
        let mut candidates: Vec<RoutingDecision> = Vec::new();

        for entry in self.agents.iter() {
            let agent_id = *entry.key();
            let manifest = entry.value();

            // Skip agents that aren't idle or executing (can take more work).
            if let Some(status) = self.status.get(&agent_id) {
                if status.is_terminal() {
                    continue;
                }
            }

            // Capability score: what fraction of required capabilities does
            // this agent provide?
            let cap_score = if required_capabilities.is_empty() {
                1.0
            } else {
                let matched = required_capabilities
                    .iter()
                    .filter(|req| manifest.has_capability(req))
                    .count();
                matched as f64 / required_capabilities.len() as f64
            };

            // Skip agents that don't match any required capability.
            if !required_capabilities.is_empty() && cap_score == 0.0 {
                continue;
            }

            // Load score: inversely proportional to current task count.
            let load_score = if let Some(load) = self.load.get(&agent_id) {
                let active = load.active();
                1.0 / (1.0 + active as f64)
            } else {
                0.5
            };

            // Affinity score: does this agent have context for related work?
            let affinity_score = if let Some(key) = context_key {
                if let Some(entries) = self.affinities.get(key) {
                    entries
                        .iter()
                        .filter(|e| e.agent_id == agent_id)
                        .map(|e| {
                            // Decay affinity over time (half-life: 1 hour).
                            let age_hours = (Utc::now() - e.last_used)
                                .num_seconds()
                                .max(0) as f64
                                / 3600.0;
                            e.strength * (-age_hours / 1.0).exp()
                        })
                        .sum::<f64>()
                        .min(1.0)
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let score = self.capability_weight * cap_score
                + self.load_weight * load_score
                + self.affinity_weight * affinity_score;

            let role = SpecialistRole::infer_from_capabilities(&manifest.capabilities);

            candidates.push(RoutingDecision {
                agent_id,
                role,
                score,
                score_breakdown: ScoreBreakdown {
                    capability_score: cap_score,
                    load_score,
                    affinity_score,
                },
            });
        }

        if candidates.is_empty() {
            return Err(DafError::NotFound {
                entity: "specialist".into(),
                id: format!(
                    "no agent available for task '{}' requiring {:?}",
                    task.task_type, required_capabilities
                ),
            });
        }

        // Sort by score descending, pick the best.
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        let winner = candidates.into_iter().next().unwrap();

        // Update load tracking for the selected agent.
        if let Some(load) = self.load.get(&winner.agent_id) {
            load.assign();
        }

        debug!(
            agent = %winner.agent_id,
            role = %winner.role,
            score = winner.score,
            cap = winner.score_breakdown.capability_score,
            load = winner.score_breakdown.load_score,
            affinity = winner.score_breakdown.affinity_score,
            "routed task to specialist"
        );

        Ok(winner)
    }

    /// Get the number of active tasks for an agent.
    pub fn active_tasks(&self, agent_id: &AgentId) -> u64 {
        self.load
            .get(agent_id)
            .map(|l| l.active())
            .unwrap_or(0)
    }

    /// Get the total number of registered agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Get all agents registered for a specific role.
    pub fn agents_for_role(&self, role: &SpecialistRole) -> Vec<AgentId> {
        self.role_index
            .get(role)
            .map(|ids| ids.clone())
            .unwrap_or_default()
    }

    /// Find all agents that have a specific capability.
    pub fn agents_with_capability(&self, capability: &str) -> Vec<AgentId> {
        self.agents
            .iter()
            .filter(|entry| entry.value().has_capability(capability))
            .map(|entry| *entry.key())
            .collect()
    }
}

impl Default for SpecialistRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use daf_core::{AgentKind, AgentManifest};

    fn make_agent(name: &str, capabilities: &[&str]) -> AgentManifest {
        let mut m = AgentManifest::new(AgentKind::Specialist, name);
        for cap in capabilities {
            m.capabilities
                .push(AgentCapability::new(*cap, "1.0.0", format!("{cap} capability")));
        }
        m
    }

    fn sample_task(name: &str) -> TaskSpec {
        TaskSpec {
            task_type: name.into(),
            params: serde_json::json!({}),
            timeout: None,
            max_retries: 0,
        }
    }

    #[test]
    fn role_inference() {
        let caps = vec![
            AgentCapability::new("code_generation", "1.0", ""),
            AgentCapability::new("implementation", "1.0", ""),
        ];
        assert_eq!(
            SpecialistRole::infer_from_capabilities(&caps),
            SpecialistRole::Builder
        );

        let caps = vec![
            AgentCapability::new("testing", "1.0", ""),
            AgentCapability::new("qa", "1.0", ""),
        ];
        assert_eq!(
            SpecialistRole::infer_from_capabilities(&caps),
            SpecialistRole::Tester
        );
    }

    #[test]
    fn register_and_route() {
        let router = SpecialistRouter::new();
        let agent = make_agent("builder-1", &["code_generation", "implementation"]);
        let agent_id = agent.id;
        router.register_agent(agent);

        let task = sample_task("write.code");
        let decision = router
            .route_task(&task, &["code_generation".into()], None)
            .unwrap();

        assert_eq!(decision.agent_id, agent_id);
        assert!(decision.score > 0.0);
        assert_eq!(decision.score_breakdown.capability_score, 1.0);
    }

    #[test]
    fn route_no_agents_returns_error() {
        let router = SpecialistRouter::new();
        let task = sample_task("unknown.task");
        let result = router.route_task(&task, &["nonexistent".into()], None);
        assert!(result.is_err());
    }

    #[test]
    fn load_balancing() {
        let router = SpecialistRouter::new();

        let a1 = make_agent("builder-1", &["code_generation"]);
        let a2 = make_agent("builder-2", &["code_generation"]);
        let _id1 = a1.id;
        let _id2 = a2.id;
        router.register_agent(a1);
        router.register_agent(a2);

        // Route first task — either agent is fine.
        let d1 = router
            .route_task(&sample_task("build"), &["code_generation".into()], None)
            .unwrap();

        // The routed agent now has load=1, the other has load=0.
        // Route second task — should go to the less loaded agent.
        let d2 = router
            .route_task(&sample_task("build"), &["code_generation".into()], None)
            .unwrap();

        // They should go to different agents (load balancing).
        assert_ne!(d1.agent_id, d2.agent_id);
    }

    #[test]
    fn affinity_routing() {
        let router = SpecialistRouter::new();

        let a1 = make_agent("builder-1", &["code_generation"]);
        let a2 = make_agent("builder-2", &["code_generation"]);
        let id1 = a1.id;
        router.register_agent(a1);
        router.register_agent(a2);

        // Record affinity for agent 1 on the "auth-module" context.
        router.record_affinity(id1, "auth-module".into());

        // Route a task with the same context — should prefer agent 1.
        let decision = router
            .route_task(
                &sample_task("refactor"),
                &["code_generation".into()],
                Some("auth-module"),
            )
            .unwrap();

        assert_eq!(decision.agent_id, id1);
        assert!(decision.score_breakdown.affinity_score > 0.0);
    }

    #[test]
    fn deregister_agent() {
        let router = SpecialistRouter::new();
        let agent = make_agent("temp", &["testing"]);
        let agent_id = agent.id;
        router.register_agent(agent);
        assert_eq!(router.agent_count(), 1);

        router.deregister_agent(&agent_id);
        assert_eq!(router.agent_count(), 0);
    }

    #[test]
    fn skip_terminal_agents() {
        let router = SpecialistRouter::new();
        let agent = make_agent("dying", &["code_generation"]);
        let agent_id = agent.id;
        router.register_agent(agent);

        // Mark agent as failed.
        router.update_status(&agent_id, AgentStatus::Failed);

        let result = router.route_task(
            &sample_task("build"),
            &["code_generation".into()],
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn agents_with_capability_lookup() {
        let router = SpecialistRouter::new();
        router.register_agent(make_agent("a", &["testing", "qa"]));
        router.register_agent(make_agent("b", &["code_generation"]));
        router.register_agent(make_agent("c", &["testing"]));

        let testers = router.agents_with_capability("testing");
        assert_eq!(testers.len(), 2);

        let builders = router.agents_with_capability("code_generation");
        assert_eq!(builders.len(), 1);
    }

    #[test]
    fn specialist_role_display() {
        assert_eq!(SpecialistRole::Builder.to_string(), "builder");
        assert_eq!(
            SpecialistRole::Custom("infra".into()).to_string(),
            "custom:infra"
        );
    }

    #[test]
    fn custom_weights() {
        let router = SpecialistRouter::with_weights(1.0, 0.0, 0.0);
        // All weight on capability, none on load or affinity.
        assert!((router.capability_weight - 1.0).abs() < f64::EPSILON);
        assert!((router.load_weight - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn task_completion_updates_load() {
        let router = SpecialistRouter::new();
        let agent = make_agent("worker", &["code_generation"]);
        let agent_id = agent.id;
        router.register_agent(agent);

        // Route a task (increments active_tasks).
        let _ = router
            .route_task(&sample_task("build"), &["code_generation".into()], None)
            .unwrap();
        assert_eq!(router.active_tasks(&agent_id), 1);

        // Complete the task.
        router.record_task_completion(&agent_id);
        assert_eq!(router.active_tasks(&agent_id), 0);
    }
}
