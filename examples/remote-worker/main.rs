//! Loopback development worker with deterministic, prepare-only handlers.
//! The ledger commits returned effects; handlers perform no external action.
use async_trait::async_trait;
use daf_core::{AgentContext, DafError, DafResult};
use daf_runtime::durable::{DurableEffect, DurableLedger, DurableTaskOutput};
use daf_runtime::remote::{
    DurableTaskExecutor, RemoteWorker, RemoteWorkerConfig, ReplyObserver, StaticCredential,
    TaskEnvelope,
};
use daf_sdk::{
    AgentBuilder,
    handler::TaskHandler,
    task_types::{SdkTaskResult, SdkTaskSpec},
};
use serde_json::json;
use std::{io::Write, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

struct Prepare;
#[async_trait]
impl TaskHandler for Prepare {
    async fn handle_task(&self, task: SdkTaskSpec, _: &AgentContext) -> DafResult<SdkTaskResult> {
        // Optional invocation evidence for OS-crash acceptance only.
        if let Some(path) = std::env::var_os("DAF_TEST_HANDLER_LOG") {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(file, "{}", task.id)?;
            file.sync_all()?;
        }
        let mut total = 0i64;
        if let Some(values) = task.inputs.get("values").and_then(|v| v.as_array()) {
            for value in values {
                total = total
                    .checked_add(value.as_i64().ok_or_else(|| {
                        DafError::Internal("values must be signed integers".into())
                    })?)
                    .ok_or_else(|| DafError::Internal("sum overflow".into()))?;
            }
        }
        if let Some(results) = task
            .inputs
            .get("dependency_results")
            .and_then(|v| v.as_object())
        {
            for result in results.values() {
                total = total
                    .checked_add(
                        result["sum"]
                            .as_i64()
                            .ok_or_else(|| DafError::Internal("dependency has no sum".into()))?,
                    )
                    .ok_or_else(|| DafError::Internal("dependency sum overflow".into()))?;
            }
        }
        let output = DurableTaskOutput {
            output: json!({"sum": total}),
            effects: vec![DurableEffect {
                key: format!("task/{}", task.id),
                value: json!({"sum": total, "applied": 1}),
            }],
        };
        Ok(SdkTaskResult::success(
            task.id,
            serde_json::to_value(output).map_err(|e| DafError::Internal(e.to_string()))?,
            Duration::ZERO,
        ))
    }
}

// Publish complete readiness markers atomically so observers never see an empty file.
fn publish_marker(path: &std::path::Path, value: &str) -> std::io::Result<()> {
    let temporary = path.with_extension("pending");
    std::fs::write(&temporary, value)?;
    std::fs::rename(temporary, path)
}

struct TestCommitGate(PathBuf);
#[async_trait]
impl ReplyObserver for TestCommitGate {
    async fn before_reply(&self, _: &str, task: &TaskEnvelope, _: &SdkTaskResult) {
        // Test-only: marker is written after the executor's durable commit and
        // before reply transmission. The acceptance parent SIGKILLs this process.
        publish_marker(&self.0, &task.task.id.to_string()).expect("write test commit marker");
        std::future::pending::<()>().await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = None;
    let mut address: SocketAddr = "127.0.0.1:7474".parse()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = Some(PathBuf::from(args.next().ok_or("--db requires a path")?)),
            "--listen" => address = args.next().ok_or("--listen requires address")?.parse()?,
            "--help" | "-h" => {
                println!(
                    "cargo run -p daf-example-remote-worker -- --db PATH [--listen 127.0.0.1:7474]\nSet DAF_REMOTE_TOKEN to a development credential of at least 32 bytes. Loopback plaintext only."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if !address.ip().is_loopback() {
        return Err("development worker requires loopback address".into());
    }
    let token = std::env::var("DAF_REMOTE_TOKEN").map_err(|_| "set DAF_REMOTE_TOKEN")?;
    if token.is_empty() {
        return Err("DAF_REMOTE_TOKEN cannot be empty".into());
    }
    let ledger = Arc::new(DurableLedger::open(db.ok_or("--db is required")?)?);
    let agent = Arc::new(
        AgentBuilder::new("durable-development-worker")
            .on_task("sum", Prepare)
            .build()?,
    );
    agent.start().await?;
    let mut worker = RemoteWorker::bind(
        RemoteWorkerConfig {
            bind_address: address,
            tls: None,
            max_sessions: 16,
            max_frame_bytes: 1024 * 1024,
            session_timeout: Duration::from_secs(10),
        },
        Arc::new(StaticCredential::new(
            &token,
            "development-operator",
            vec!["sum".into()],
        )?),
        Arc::new(DurableTaskExecutor::new(ledger, agent.clone())),
    )
    .await?;
    if let Some(path) = std::env::var_os("DAF_TEST_HOLD_AFTER_COMMIT_FILE") {
        worker = worker.with_observer(Arc::new(TestCommitGate(PathBuf::from(path))));
    }
    let address = worker.local_addr();
    println!("Development worker listening on {address}");
    if let Some(path) = std::env::var_os("DAF_TEST_ADDRESS_FILE") {
        publish_marker(std::path::Path::new(&path), &address.to_string())?;
    }
    worker
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    agent.stop().await?;
    Ok(())
}
