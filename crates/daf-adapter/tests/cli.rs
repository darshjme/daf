use std::{
    io::Write,
    process::{Command, Stdio},
};
fn invoke(request: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_daf-adapter"))
        .args(["run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn machine_contract_and_exit_codes() {
    let good = invoke(r#"{"schema_version":1,"id":"cli","argv":["python3","-c","print('ok')"]}"#);
    assert!(good.status.success());
    let value: serde_json::Value = serde_json::from_slice(&good.stdout).unwrap();
    assert_eq!(value["stdout"], "ok\n");
    let failed =
        invoke(r#"{"schema_version":1,"id":"cli","argv":["python3","-c","raise SystemExit(3)"]}"#);
    assert_eq!(failed.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(value["exit_code"], 3);
    let invalid = invoke(r#"{"schema_version":2,"id":"cli","argv":["python3"]}"#);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());
}
#[test]
fn capabilities_are_json() {
    let out = Command::new(env!("CARGO_BIN_EXE_daf-adapter"))
        .arg("capabilities")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["protocol"], "daf.process.v1");
    assert_eq!(value["shell"], false);
}
