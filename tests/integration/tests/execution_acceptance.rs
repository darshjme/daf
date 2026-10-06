//! Acceptance checks exercise the exported framework types, never stand-ins.
use async_trait::async_trait;
use daf_core::agent::{AgentKind, AgentManifest};
use daf_core::error::{DafError, DafResult};
use daf_core::message::{Message, MessageKind};
use daf_core::{AgentContext, AgentId};
use daf_graph::{
    dag::ExecutionGraph,
    edge::{Edge, EdgeCondition, EdgeKind},
    executor::{GraphExecutor, TaskHandler},
    node::{Node, NodeKind, NodeState, TaskSpec},
};
use daf_orchestrator::{
    mission::{Mission, MissionState, Phase, RetryPolicy},
    orchestrator::{Orchestrator, WorkerHandler},
};
use daf_sdk::{
    builder::AgentBuilder,
    handler::{MessageHandler, TaskHandler as SdkTaskHandler},
    lifecycle::AgentInstance,
    middleware::AuthMiddleware,
    task_types::{SdkTaskResult, SdkTaskSpec},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Execute(Arc<AtomicUsize>);
#[async_trait]
impl SdkTaskHandler for Execute {
    async fn handle_task(&self, task: SdkTaskSpec, _: &AgentContext) -> DafResult<SdkTaskResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(SdkTaskResult::success(
            task.id,
            serde_json::json!({"executed": true}),
            Duration::ZERO,
        ))
    }
}
struct Echo;
#[async_trait]
impl MessageHandler for Echo {
    async fn handle_message(&self, msg: Message, _: &AgentContext) -> DafResult<Option<Message>> {
        Ok(Some(msg))
    }
}
struct SdkWorker(Arc<AgentInstance>);
#[async_trait]
impl WorkerHandler for SdkWorker {
    async fn execute(&self, _: AgentId, task: TaskSpec) -> DafResult<()> {
        let result = self
            .0
            .process_task(SdkTaskSpec::new(task.task_type, "real SDK work"))
            .await?;
        if result.success {
            Ok(())
        } else {
            Err(DafError::Internal("SDK task failed".into()))
        }
    }
}

#[tokio::test]
async fn recovery_graph_executes_actual_mission_worker_and_sdk_handlers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sdk = Arc::new(
        AgentBuilder::new("acceptance-worker")
            .on_task("evaluate", Execute(calls.clone()))
            .on_message("*", Echo)
            .with_middleware(AuthMiddleware::new(
                "authorization",
                vec!["test-token".into()],
            ))
            .build()
            .unwrap(),
    );
    sdk.start().await.unwrap();
    let denied = Message::builder(MessageKind::Request, AgentId::new()).build();
    assert!(matches!(
        sdk.process_message(denied).await,
        Err(DafError::Unauthorized { .. })
    ));
    let allowed = Message::builder(MessageKind::Request, AgentId::new())
        .header("authorization", "test-token")
        .build();
    assert!(sdk.process_message(allowed).await.unwrap().is_some());

    let orch = Arc::new(Orchestrator::new());
    let agent = orch
        .spawn_agent(AgentManifest::new(AgentKind::Worker, "sdk-worker"))
        .unwrap();
    orch.register_worker(agent, Arc::new(SdkWorker(sdk.clone())))
        .unwrap();
    let mission = Arc::new(
        Mission::builder("recovery-evaluation")
            .retry_policy(RetryPolicy::none())
            .phase(Phase::new("evaluate").task(TaskSpec {
                task_type: "evaluate".into(),
                params: serde_json::Value::Null,
                timeout: Some(Duration::from_secs(1)),
                max_retries: 0,
            }))
            .build(),
    );
    let mut graph = ExecutionGraph::new("acceptance-recovery");
    let initial = graph.add_node(Node::new(NodeKind::Task, "controlled-failure"));
    let recovery = graph.add_node(Node::new(NodeKind::Task, "real-recovery"));
    graph
        .add_edge(
            Edge::new(initial, recovery, EdgeKind::Conditional)
                .with_condition(EdgeCondition::OnFailure),
        )
        .unwrap();
    let worker_orch = orch.clone();
    let handler: TaskHandler = Arc::new(move |id| {
        let orch = worker_orch.clone();
        let mission = mission.clone();
        Box::pin(async move {
            if id == initial {
                return Err("controlled failure".into());
            }
            let result = orch
                .run_mission_async(&mission)
                .await
                .map_err(|e| e.to_string())?;
            if result.state == MissionState::Completed {
                Ok(())
            } else {
                Err("mission did not complete".into())
            }
        })
    });
    let executor = GraphExecutor::new(graph, handler).with_global_timeout(Duration::from_secs(2));
    let progress = executor.execute().await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "actual SDK handler must execute"
    );
    assert_eq!((progress.nodes_completed, progress.nodes_failed), (2, 1));
    assert_eq!(
        executor.graph().read().get_node(recovery).unwrap().state,
        NodeState::Succeeded
    );
    assert_eq!(orch.get_agent(&agent).unwrap().active_tasks, 0);
    assert_eq!(orch.router().active_tasks(&agent), 0);
    sdk.stop().await.unwrap();
}
