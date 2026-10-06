//! Run a reviewed JSON plan through real registered specialist handlers.
use async_trait::async_trait;
use daf_core::{
    AgentId,
    agent::{AgentCapability, AgentKind, AgentManifest},
    error::DafResult,
};
use daf_graph::node::TaskSpec;
use daf_orchestrator::{Orchestrator, PlanLimits, SpecialistPlan, orchestrator::WorkerHandler};
use std::sync::Arc;

struct Report;
#[async_trait]
impl WorkerHandler for Report {
    async fn execute(&self, agent: AgentId, task: TaskSpec) -> DafResult<()> {
        println!("{agent}: {} {}", task.task_type, task.params);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let orchestrator = Orchestrator::new();
    for capability in ["rust", "review", "verification"] {
        let agent = orchestrator.spawn_agent(
            AgentManifest::new(AgentKind::Worker, capability).with_capability(
                AgentCapability::new(capability, "1.0.0", "Example reporter"),
            ),
        )?;
        orchestrator.register_worker(agent, Arc::new(Report))?;
    }
    let plan: SpecialistPlan = serde_json::from_str(
        r#"{"name":"reviewed-change","timeout_secs":30,"assignments":[
          {"name":"prepare","task_type":"rust.prepare","params":{"scope":"example"},
           "timeout_secs":5,"required_capabilities":["rust"]},
          {"name":"review","task_type":"review.check","params":{},
           "timeout_secs":5,"required_capabilities":["review"],"depends_on":["prepare"]},
          {"name":"verify","task_type":"verification.check","params":{},
           "timeout_secs":5,"required_capabilities":["verification"],"depends_on":["review"]}
        ]}"#,
    )?;
    let result = orchestrator
        .run_specialist_plan(&plan, &PlanLimits::default())
        .await?;
    for phase in result.phase_results {
        if !phase.succeeded {
            return Err("specialist phase failed".into());
        }
    }
    println!("All three registered specialists completed in dependency order.");
    Ok(())
}
