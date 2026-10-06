//! Bounded specialist plans executed through registered orchestrator workers.
//! Plans describe work; they do not create agents, infer intelligence or provide
//! durable exactly-once execution. Current mission phases execute sequentially.
//!
//! Example JSON (workers with both required capabilities must be registered):
//! ```json
//! {"name":"review-change","timeout_secs":60,"assignments":[
//!   {"name":"prepare","task_type":"rust.prepare","params":{},
//!    "timeout_secs":20,"required_capabilities":["rust"],"depends_on":[]},
//!   {"name":"review","task_type":"review.check","params":{},
//!    "timeout_secs":20,"required_capabilities":["review"],"depends_on":["prepare"]}
//! ]}
//! ```
//! Parse with `serde_json::from_str::<SpecialistPlan>`, then call
//! `orchestrator.run_specialist_plan(&plan, &PlanLimits::default()).await`.
//! The API validates structure and capability coverage; it does not verify that
//! declared work is safe or that worker-generated outputs satisfy an objective.
use crate::{Mission, Phase, RetryPolicy};
use daf_core::error::{DafError, DafResult};
use daf_graph::node::TaskSpec;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};

/// Conservative configurable admission limits; enforced before dispatch.
#[derive(Debug, Clone)]
pub struct PlanLimits {
    pub max_assignments: usize,
    pub max_bytes: usize,
    pub max_timeout_secs: u64,
    pub max_retries: u32,
}
impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_assignments: 32,
            max_bytes: 1_048_576,
            max_timeout_secs: 3600,
            max_retries: 3,
        }
    }
}
/// One named capability-constrained unit of specialist work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistAssignment {
    pub name: String,
    pub task_type: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub timeout_secs: u64,
    #[serde(default)]
    pub max_retries: u32,
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}
/// JSON-friendly plan with explicit deadlines and dependency names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistPlan {
    pub name: String,
    pub timeout_secs: u64,
    pub assignments: Vec<SpecialistAssignment>,
}
impl SpecialistPlan {
    /// Reject oversized, ambiguous and cyclic plans before worker invocation.
    pub fn to_mission(&self, limits: &PlanLimits) -> DafResult<Mission> {
        let invalid = |message: &str| DafError::ConfigError(format!("specialist plan: {message}"));
        if limits.max_assignments == 0
            || limits.max_assignments > 1024
            || limits.max_bytes == 0
            || limits.max_bytes > 16_777_216
            || limits.max_timeout_secs == 0
            || limits.max_timeout_secs > 86_400
            || limits.max_retries > 10
        {
            return Err(invalid("admission limits outside supported bounds"));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.assignments.is_empty()
            || self.assignments.len() > limits.max_assignments
        {
            return Err(invalid("invalid name or assignment count"));
        }
        if self.timeout_secs == 0 || self.timeout_secs > limits.max_timeout_secs {
            return Err(invalid("mission deadline exceeds limits"));
        }
        if serde_json::to_vec(self)
            .map_err(|e| invalid(&e.to_string()))?
            .len()
            > limits.max_bytes
        {
            return Err(invalid("serialized plan exceeds byte limit"));
        }
        let mut names = HashSet::new();
        let mut mission = Mission::builder(&self.name)
            .timeout(Duration::from_secs(self.timeout_secs))
            .retry_policy(RetryPolicy::none());
        for assignment in &self.assignments {
            if assignment.name.trim().is_empty()
                || assignment.name.len() > 256
                || !names.insert(&assignment.name)
                || assignment.task_type.trim().is_empty()
                || assignment.task_type.len() > 256
            {
                return Err(invalid(
                    "empty, duplicate or oversized assignment name/task type",
                ));
            }
            if assignment.timeout_secs == 0
                || assignment.timeout_secs > self.timeout_secs
                || assignment.max_retries > limits.max_retries
            {
                return Err(invalid("task deadline or retry budget exceeds limits"));
            }
            if assignment.required_capabilities.is_empty()
                || assignment.required_capabilities.len() > 32
            {
                return Err(invalid("each specialist needs 1..32 capabilities"));
            }
            let mut capabilities = HashSet::new();
            if assignment
                .required_capabilities
                .iter()
                .any(|cap| cap.trim().is_empty() || cap.len() > 256 || !capabilities.insert(cap))
            {
                return Err(invalid("invalid or duplicate capability"));
            }
            let mut dependencies = HashSet::new();
            if assignment.depends_on.len() > limits.max_assignments
                || assignment
                    .depends_on
                    .iter()
                    .any(|name| name == &assignment.name || !dependencies.insert(name))
            {
                return Err(invalid("duplicate or self dependency"));
            }
            let task = TaskSpec {
                task_type: assignment.task_type.clone(),
                params: assignment.params.clone(),
                timeout: Some(Duration::from_secs(assignment.timeout_secs)),
                max_retries: assignment.max_retries,
            };
            let mut phase = Phase::new(&assignment.name).task(task).retry(RetryPolicy {
                max_retries: assignment.max_retries,
                ..RetryPolicy::none()
            });
            phase.dependencies = assignment.depends_on.clone();
            phase.required_capabilities = assignment.required_capabilities.clone();
            mission = mission.phase(phase);
        }
        let mission = mission.build();
        mission.resolve_execution_order()?;
        Ok(mission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Orchestrator, orchestrator::WorkerHandler};
    use daf_core::{
        AgentId,
        agent::{AgentCapability, AgentKind, AgentManifest},
    };
    use std::sync::{Arc, Mutex};
    fn assignment(name: &str, capability: &str, dependencies: &[&str]) -> SpecialistAssignment {
        SpecialistAssignment {
            name: name.into(),
            task_type: name.into(),
            params: serde_json::json!({"payload":name}),
            timeout_secs: 1,
            max_retries: 0,
            required_capabilities: vec![capability.into()],
            depends_on: dependencies.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn plan() -> SpecialistPlan {
        SpecialistPlan {
            name: "review".into(),
            timeout_secs: 5,
            assignments: vec![
                assignment("prepare", "rust", &[]),
                assignment("review", "review", &["prepare"]),
            ],
        }
    }
    struct Record {
        calls: Arc<Mutex<Vec<String>>>,
        fail: bool,
    }
    #[async_trait::async_trait]
    impl WorkerHandler for Record {
        async fn execute(&self, _: AgentId, task: TaskSpec) -> DafResult<()> {
            self.calls.lock().unwrap().push(task.task_type);
            if self.fail {
                Err(DafError::Internal("controlled failure".into()))
            } else {
                Ok(())
            }
        }
    }
    fn register(orch: &Orchestrator, cap: &str, calls: Arc<Mutex<Vec<String>>>, fail: bool) {
        let manifest = AgentManifest::new(AgentKind::Worker, cap)
            .with_capability(AgentCapability::new(cap, "1.0.0", ""));
        let id = orch.spawn_agent(manifest).unwrap();
        orch.register_worker(id, Arc::new(Record { calls, fail }))
            .unwrap();
    }
    #[test]
    fn validates_bounds_cycles_and_json_seconds() {
        let mut p = plan();
        assert!(p.to_mission(&PlanLimits::default()).is_ok());
        assert_eq!(serde_json::to_value(&p).unwrap()["timeout_secs"], 5);
        p.assignments[0].depends_on = vec!["review".into()];
        assert!(p.to_mission(&PlanLimits::default()).is_err());
        p = plan();
        p.assignments[1].depends_on = vec!["missing".into()];
        assert!(p.to_mission(&PlanLimits::default()).is_err());
        p = plan();
        p.assignments[1].required_capabilities.clear();
        assert!(p.to_mission(&PlanLimits::default()).is_err());
        p = plan();
        p.assignments[0].max_retries = 4;
        assert!(p.to_mission(&PlanLimits::default()).is_err());
        p = plan();
        p.timeout_secs = 0;
        assert!(p.to_mission(&PlanLimits::default()).is_err());
        let limits = PlanLimits {
            max_assignments: 1,
            ..Default::default()
        };
        assert!(plan().to_mission(&limits).is_err());
    }
    #[test]
    fn configurable_limits_cannot_disable_absolute_admission_bounds() {
        for limits in [
            PlanLimits {
                max_assignments: 1025,
                ..Default::default()
            },
            PlanLimits {
                max_bytes: 16_777_217,
                ..Default::default()
            },
            PlanLimits {
                max_timeout_secs: u64::MAX,
                ..Default::default()
            },
            PlanLimits {
                max_retries: 11,
                ..Default::default()
            },
        ] {
            assert!(plan().to_mission(&limits).is_err());
        }
        assert!(
            plan()
                .to_mission(&PlanLimits {
                    max_assignments: 1024,
                    max_bytes: 16_777_216,
                    max_timeout_secs: 86_400,
                    max_retries: 10
                })
                .is_ok()
        );
    }

    #[tokio::test]
    async fn preflight_prevents_partial_work_then_executes_registered_specialists() {
        let orch = Orchestrator::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        register(&orch, "rust", calls.clone(), false);
        // Metadata-only agents must never substitute for an executable worker.
        orch.spawn_agent(
            AgentManifest::new(AgentKind::Worker, "ghost")
                .with_capability(AgentCapability::new("review", "1.0.0", "")),
        )
        .unwrap();
        assert!(
            orch.run_specialist_plan(&plan(), &PlanLimits::default())
                .await
                .is_err()
        );
        assert!(calls.lock().unwrap().is_empty());
        register(&orch, "review", calls.clone(), false);
        let result = orch
            .run_specialist_plan(&plan(), &PlanLimits::default())
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["prepare", "review"]);
        assert!(result.phase_results.iter().all(|phase| phase.succeeded));
        for id in orch.agent_ids() {
            assert_eq!(orch.router().active_tasks(&id), 0);
            assert_eq!(orch.get_agent(&id).unwrap().active_tasks, 0);
        }
    }
    #[tokio::test]
    async fn failed_predecessor_blocks_dependent_worker() {
        let orch = Orchestrator::builder().fail_fast(false).build();
        let calls = Arc::new(Mutex::new(Vec::new()));
        register(&orch, "rust", calls.clone(), true);
        register(&orch, "review", calls.clone(), false);
        let result = orch
            .run_specialist_plan(&plan(), &PlanLimits::default())
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["prepare"]);
        assert_eq!(result.phase_results[1].tasks_skipped, 1);
        assert!(!result.phase_results[0].succeeded);
    }
}
