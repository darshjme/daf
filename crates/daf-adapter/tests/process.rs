use daf_adapter::{execute, parse_request};
use serde_json::json;
fn request(code: &str, extra: serde_json::Value) -> daf_adapter::Request {
    let mut value = json!({"schema_version":1,"id":"test","argv":["python3","-c",code]});
    for (key, value_extra) in extra.as_object().unwrap() {
        value[key] = value_extra.clone();
    }
    parse_request(&serde_json::to_vec(&value).unwrap()).unwrap()
}
#[tokio::test]
async fn literal_stdin_and_nonzero_exit() {
    let result = execute(request(
        "import sys; print(sys.stdin.read()); sys.exit(7)",
        json!({"stdin":"$(touch nope); & hello"}),
    ))
    .await
    .unwrap();
    assert_eq!(result.status, "failed");
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.stdout, "$(touch nope); & hello\n");
}
#[tokio::test]
async fn caps_and_drains_both_streams() {
    let result = execute(request(
        "import sys; sys.stdout.write('x'*200000); sys.stderr.write('y'*200000)",
        json!({"max_output_bytes":31}),
    ))
    .await
    .unwrap();
    assert_eq!(result.status, "succeeded");
    assert_eq!(result.stdout.len(), 31);
    assert_eq!(result.stderr.len(), 31);
    assert!(result.stdout_truncated && result.stderr_truncated);
}
#[tokio::test]
async fn deadline_covers_stdin_and_keeps_partial_output() {
    let result = execute(request(
        "import time; print('started',flush=True); time.sleep(10)",
        json!({"stdin":"z".repeat(200000),"timeout_ms":150}),
    ))
    .await
    .unwrap();
    assert_eq!(result.status, "timed_out");
    assert_eq!(result.stdout, "started\n");
    assert_eq!(result.exit_code, None);
}
#[tokio::test]
async fn utf8_raw_truncation_is_lossy() {
    let result = execute(request("print('éé',end='')", json!({"max_output_bytes":3})))
        .await
        .unwrap();
    assert_eq!(result.stdout, "é�");
    assert!(result.stdout_truncated);
}
#[tokio::test]
async fn spawn_failure_is_structured() {
    let req = parse_request(
        br#"{"schema_version":1,"id":"missing","argv":["/this/executable/does/not/exist"]}"#,
    )
    .unwrap();
    let result = execute(req).await.unwrap();
    assert_eq!(result.status, "failed");
    assert_eq!(result.exit_code, None);
    assert!(!result.stderr.is_empty());
}
#[tokio::test]
async fn cwd_is_applied() {
    let temp = tempfile::tempdir().unwrap();
    let result = execute(request(
        "from pathlib import Path; print(Path.cwd().name)",
        json!({"cwd":temp.path()}),
    ))
    .await
    .unwrap();
    assert_eq!(
        result.stdout.trim(),
        temp.path().file_name().unwrap().to_str().unwrap()
    );
}
#[test]
fn rejects_invalid_contract() {
    for patch in [
        json!({"schema_version":2}),
        json!({"timeout_ms":0}),
        json!({"timeout_ms":true}),
        json!({"max_output_bytes":1048577}),
        json!({"id":" "}),
        json!({"argv":[]}),
        json!({"argv":["a\u{0000}b"]}),
        json!({"surprise":1}),
    ] {
        let mut value = json!({"schema_version":1,"id":"x","argv":["echo"]});
        for (k, v) in patch.as_object().unwrap() {
            value[k] = v.clone();
        }
        assert!(
            parse_request(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{value}"
        );
    }
    assert!(parse_request(&vec![b' '; 1048577]).is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn inherited_pipes_obey_deadline() {
    // Leave startup headroom on busy CI/developer machines while keeping the
    // descendant alive beyond the deadline. The adapter does not own descendants.
    let started = std::time::Instant::now();
    let result=execute(request("import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time; time.sleep(3)']); print('parent',flush=True)",json!({"timeout_ms":1000}))).await.unwrap();
    assert_eq!(result.status, "timed_out");
    assert!(started.elapsed().as_millis() < 2500);
    assert_eq!(result.stdout, "parent\n");
    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
}
#[test]
fn duplicate_keys_and_normalized_boundary() {
    assert!(parse_request(br#"{"schema_version":1,"id":"a","id":"b","argv":["echo"]}"#).is_err());
    let mut value = json!({"schema_version":1,"id":"limit","argv":["echo"],"cwd":null,"stdin":"","timeout_ms":300000,"max_output_bytes":65536});
    let overhead = serde_json::to_vec(&value).unwrap().len();
    value["stdin"] = json!("a".repeat(daf_adapter::MAX_REQUEST_BYTES - overhead));
    assert!(parse_request(&serde_json::to_vec(&value).unwrap()).is_ok());
    value["stdin"] = json!("a".repeat(daf_adapter::MAX_REQUEST_BYTES - overhead + 1));
    assert!(parse_request(&serde_json::to_vec(&value).unwrap()).is_err());
}
