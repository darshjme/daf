//! Actual operating-system crash acceptance; no simulated drop/reopen.
#![cfg(unix)]
use daf_runtime::durable::DurableLedger;
use daf_runtime::remote::{RemoteClient, TaskEnvelope};
use daf_sdk::task_types::SdkTaskSpec;
use serde_json::json;
use std::{
    net::SocketAddr,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout};

const TOKEN: &str = "test-only-loopback-credential-0123456789";
const PRINCIPAL: &str = "development-operator";

struct WorkerProcess {
    child: Child,
    stderr: PathBuf,
}
impl WorkerProcess {
    fn spawn(dir: &Path, gate: bool, generation: &str) -> Self {
        let stderr = dir.join(format!("{generation}.stderr"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_daf-example-remote-worker"));
        command
            .args([
                "--db",
                dir.join("ledger").to_str().unwrap(),
                "--listen",
                "127.0.0.1:0",
            ])
            .env("DAF_REMOTE_TOKEN", TOKEN)
            .env(
                "DAF_TEST_ADDRESS_FILE",
                dir.join(format!("{generation}.address")),
            )
            .env("DAF_TEST_HANDLER_LOG", dir.join("handler-invocations"))
            .env_remove("DAF_TEST_HOLD_AFTER_COMMIT_FILE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap());
        if gate {
            command.env("DAF_TEST_HOLD_AFTER_COMMIT_FILE", dir.join("committed"));
        }
        Self {
            child: command.spawn().expect("spawn actual worker process"),
            stderr,
        }
    }
    async fn address(&mut self, path: &Path) -> SocketAddr {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(value) = std::fs::read_to_string(path) {
                return value.parse().unwrap();
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "worker exited {status}: {}",
                    std::fs::read_to_string(&self.stderr).unwrap()
                );
            }
            assert!(
                Instant::now() < deadline,
                "worker did not become ready: {}",
                std::fs::read_to_string(&self.stderr).unwrap()
            );
            sleep(Duration::from_millis(10)).await;
        }
    }
    fn sigkill(&mut self) {
        self.child.kill().expect("send SIGKILL to actual child");
        let status = self.child.wait().expect("reap killed child");
        assert_eq!(status.signal(), Some(9), "test must use actual SIGKILL");
    }
}
impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn client(address: SocketAddr) -> RemoteClient {
    RemoteClient::new(address, None, Duration::from_secs(5), 1024 * 1024).unwrap()
}
fn task(values: serde_json::Value) -> SdkTaskSpec {
    SdkTaskSpec::new("sum", "prepare a durable sum effect")
        .with_inputs(json!({"values": values}))
        .with_timeout(Duration::from_secs(2))
}

#[tokio::test]
async fn sigkill_after_commit_before_reply_recovers_once_and_feeds_dependency() {
    timeout(Duration::from_secs(20), async {
        let dir = tempfile::tempdir().unwrap();
        let mut first = WorkerProcess::spawn(dir.path(), true, "first");
        let address = first.address(&dir.path().join("first.address")).await;
        let parent_task = task(json!([19, 23]));
        let parent_id = parent_task.id;
        let envelope = TaskEnvelope {
            task: parent_task.clone(),
            dependencies: vec![],
        };

        // Auth rejection is exercised against the actual worker before any handler.
        assert!(
            client(address)
                .submit("wrong-credential", envelope.clone())
                .await
                .is_err()
        );
        assert!(client(address).submit("", envelope.clone()).await.is_err());
        assert!(!dir.path().join("handler-invocations").exists());

        let remote = client(address);
        let request = remote.submit(TOKEN, envelope.clone());
        tokio::pin!(request);
        let wait_for_commit = async {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(marker) = std::fs::read_to_string(dir.path().join("committed")) {
                    assert_eq!(marker, parent_id.to_string());
                    break;
                }
                assert!(Instant::now() < deadline, "worker did not durably commit");
                sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result = &mut request => panic!("worker replied before crash gate: {result:?}"),
            () = wait_for_commit => {}
        }
        first.sigkill();
        assert!(
            request.await.is_err(),
            "connection must fail before committed result is replied"
        );

        // Reopen after SIGKILL, not graceful closure, and verify committed state.
        let ledger = DurableLedger::open(dir.path().join("ledger")).unwrap();
        let committed = ledger.lookup(PRINCIPAL, parent_id).unwrap().unwrap();
        assert_eq!(committed.output, json!({"sum": 42}));
        assert_eq!(
            ledger
                .read_effect(PRINCIPAL, &format!("task/{parent_id}"))
                .unwrap(),
            Some(json!({"sum": 42, "applied": 1}))
        );
        drop(ledger);

        let mut restarted = WorkerProcess::spawn(dir.path(), false, "restarted");
        let address = restarted
            .address(&dir.path().join("restarted.address"))
            .await;
        let recovered = client(address)
            .submit(TOKEN, envelope.clone())
            .await
            .unwrap();
        assert_eq!(recovered.task_id, parent_id);
        assert_eq!(recovered.output, json!({"sum": 42}));
        let calls = std::fs::read_to_string(dir.path().join("handler-invocations")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec![parent_id.to_string()]
        );

        // Same ID with different request must be rejected before a second effect.
        let mut conflicting = envelope;
        conflicting.task.inputs = json!({"values": [999]});
        assert!(client(address).submit(TOKEN, conflicting).await.is_err());

        // The dependency's output is recovered by the framework from durable state.
        let dependent = task(json!([8]));
        let dependent_id = dependent.id;
        let dependent = TaskEnvelope {
            task: dependent,
            dependencies: vec![parent_id],
        };
        let result = client(address)
            .submit(TOKEN, dependent.clone())
            .await
            .unwrap();
        assert_eq!(result.output, json!({"sum": 50}));
        assert_eq!(
            client(address)
                .submit(TOKEN, dependent)
                .await
                .unwrap()
                .output,
            result.output
        );
        let calls = std::fs::read_to_string(dir.path().join("handler-invocations")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec![parent_id.to_string(), dependent_id.to_string()]
        );
        restarted.sigkill();
        let ledger = DurableLedger::open(dir.path().join("ledger")).unwrap();
        assert_eq!(
            ledger
                .lookup(PRINCIPAL, dependent_id)
                .unwrap()
                .unwrap()
                .output,
            json!({"sum": 50})
        );
        assert_eq!(
            ledger
                .read_effect(PRINCIPAL, &format!("task/{parent_id}"))
                .unwrap(),
            Some(json!({"sum": 42, "applied": 1}))
        );
        assert_eq!(
            ledger
                .read_effect(PRINCIPAL, &format!("task/{dependent_id}"))
                .unwrap(),
            Some(json!({"sum": 50, "applied": 1}))
        );
    })
    .await
    .expect("OS crash/recovery acceptance exceeded twenty seconds");
}
