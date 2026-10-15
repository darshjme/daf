//! End-to-end tests use Git, production orchestration and executable JSONL providers.
use daf_review::{ReviewOptions, provider::CodexProvider, run_review, validation::review_schema};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path, process::Command, sync::Arc};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("repo")).unwrap();
        let repo = root.join("repo");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.invalid"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(repo.join("a.rs"), "fn answer() -> u32 {\n  1\n}\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-qm", "fixture"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(repo.join("a.rs"), "fn answer() -> u32 {\n  2\n}\n").unwrap();
        let script = format!(
            r#"#!/usr/bin/python3
import sys,json,pathlib,time,os
root=pathlib.Path(__file__).parent
prompt=sys.stdin.read()
log=root/'calls'
count=len(log.read_text().splitlines()) if log.exists() else 0
with log.open('a') as f: f.write('call\n'); f.flush()
mode={mode:?}
if mode=='block' and count==1:
 (root/'blocked').write_text('ready')
 parent=os.getppid()
 for _ in range(300):
  if os.getppid()!=parent: sys.exit(1)
  time.sleep(0.1)
finding={{'path':'a.rs','line':2,'severity':'high','title':'Changed answer','body':'The return value changed.'}}
if mode=='invalid-first' and count==0: finding['path']='unrelated.rs'
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':json.dumps({{'findings':[finding]}})}}}}))
usage={{'input_tokens':10,'cached_input_tokens':3,'output_tokens':5}}
print(json.dumps({{'type':'turn.completed',**({{}} if mode=='unknown' else {{'usage':usage}})}}))
if mode=='failed': sys.exit(2)
"#
        );
        fs::write(root.join("provider"), script).unwrap();
        #[cfg(unix)]
        fs::set_permissions(root.join("provider"), fs::Permissions::from_mode(0o755)).unwrap();
        Self { temp }
    }
    fn root(&self) -> &Path {
        self.temp.path()
    }
    fn options(&self) -> ReviewOptions {
        ReviewOptions {
            repo: self.root().join("repo"),
            base: "HEAD".into(),
            state_dir: self.root().join("state"),
            timeout_secs: 10,
            max_retries: 1,
            allow_unknown_retry: false,
        }
    }
    fn provider(&self) -> Arc<CodexProvider> {
        let mut p = CodexProvider::new(self.root().join("repo"), review_schema());
        p.executable = self.root().join("provider");
        p.timeout_secs = 5;
        Arc::new(p)
    }
    fn calls(&self) -> usize {
        fs::read_to_string(self.root().join("calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
#[tokio::test]
async fn durable_resume_skips_provider_and_changed_bytes_reject_reuse() {
    let f = Fixture::new("success");
    let first = run_review(f.options(), f.provider()).await.unwrap();
    assert!(!first.findings.is_empty());
    let calls = f.calls();
    assert!(calls >= 2);
    let second = run_review(f.options(), f.provider()).await.unwrap();
    assert_eq!(f.calls(), calls);
    assert_eq!(first.fingerprint, second.fingerprint);
    assert_eq!(first.attempts.len(), second.attempts.len());
    fs::write(f.root().join("repo/a.rs"), "fn answer() -> u32 {\n  3\n}\n").unwrap();
    assert!(run_review(f.options(), f.provider()).await.is_err());
    assert_eq!(f.calls(), calls);
}
#[tokio::test]
async fn invalid_output_retries_and_preserves_both_attempt_usage() {
    let f = Fixture::new("invalid-first");
    let result = run_review(f.options(), f.provider()).await.unwrap();
    assert!(result.attempts.iter().any(|a| a.status == "invalid"));
    assert!(
        result
            .attempts
            .iter()
            .all(|a| a.usage.input_tokens == Some(10))
    );
    assert!(
        result
            .attempts
            .iter()
            .all(|a| a.usage.cached_input_tokens == Some(3))
    );
    assert!(
        result
            .attempts
            .iter()
            .all(|a| a.usage.output_tokens == Some(5))
    );
    assert_eq!(result.attempts.len(), f.calls());
    let summary = result.usage_summary();
    assert_eq!(summary.attempts, 3);
    assert_eq!(summary.known_input_tokens, 30);
    assert_eq!(summary.known_cached_input_tokens, 9);
    assert_eq!(summary.known_output_tokens, 15);
    assert_eq!(summary.attempts_with_unknown_usage, 0);
    assert!(result.findings.iter().all(|a| a.path == "a.rs"));
}
#[tokio::test]
async fn missing_usage_remains_unknown_after_resume() {
    let f = Fixture::new("unknown");
    let result = run_review(f.options(), f.provider()).await.unwrap();
    let summary = result.usage_summary();
    assert_eq!(summary.attempts_with_unknown_usage, 2);
    assert_eq!(summary.known_input_tokens, 0);
    assert_eq!(summary.known_cached_input_tokens, 0);
    assert_eq!(summary.known_output_tokens, 0);
    assert!(
        result
            .attempts
            .iter()
            .all(|a| a.usage.input_tokens.is_none())
    );
    assert!(
        result
            .attempts
            .iter()
            .all(|a| a.usage.output_tokens.is_none())
    );
    let calls = f.calls();
    let result = run_review(f.options(), f.provider()).await.unwrap();
    assert_eq!(f.calls(), calls);
    assert!(
        result
            .attempts
            .iter()
            .all(|a| a.usage.input_tokens.is_none())
    );
}

// Invoked in a separate test-harness process by interruption test below.
#[test]
fn subprocess_review_helper() {
    let Some(root) = std::env::var_os("DAF_TEST_RESUME_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let options = ReviewOptions {
        repo: root.join("repo"),
        base: "HEAD".into(),
        state_dir: root.join("state"),
        timeout_secs: 10,
        max_retries: 1,
        allow_unknown_retry: false,
    };
    let mut provider = CodexProvider::new(root.join("repo"), review_schema());
    provider.executable = root.join("provider");
    provider.timeout_secs = 5;
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(run_review(options, Arc::new(provider)))
        .unwrap();
}
#[tokio::test]
async fn killed_process_preserves_completed_work_and_unknown_attempt() {
    let f = Fixture::new("block");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_review_helper", "--nocapture"])
        .env("DAF_TEST_RESUME_ROOT", f.root())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !f.root().join("blocked").exists() && start.elapsed() < std::time::Duration::from_secs(15)
    {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("helper exited early: {status}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let ready = f.root().join("blocked").exists();
    if ready {
        let state_path = f.root().join("state/state.json");
        let before_state = fs::read(&state_path).unwrap();
        let concurrent = run_review(f.options(), f.provider()).await.unwrap_err();
        assert!(concurrent.to_string().contains("state lock"));
        assert_eq!(f.calls(), 2);
        assert_eq!(fs::read(state_path).unwrap(), before_state);
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(ready, "provider did not reach second call");
    let before = f.calls();
    assert_eq!(before, 2);
    assert!(run_review(f.options(), f.provider()).await.is_err());
    assert_eq!(f.calls(), before);
    let mut options = f.options();
    options.allow_unknown_retry = true;
    let result = run_review(options, f.provider()).await.unwrap();
    assert!(result.attempts.iter().any(|a| a.status == "interrupted"));
    let summary = result.usage_summary();
    assert_eq!(summary.attempts_with_unknown_usage, 1);
    assert_eq!(summary.known_input_tokens, 20);
    assert_eq!(summary.known_cached_input_tokens, 6);
    assert_eq!(summary.known_output_tokens, 10);
    assert_eq!(
        result
            .attempts
            .iter()
            .filter(|a| a.status == "succeeded")
            .count(),
        result.completed_roles.len()
    );
    assert!(
        result
            .attempts
            .iter()
            .filter(|a| a.status == "interrupted")
            .all(|a| a.usage.input_tokens.is_none())
    );
    // First successful role is reused; only the interrupted and remaining roles run.
    assert_eq!(f.calls(), result.completed_roles.len() + 1);
}
#[tokio::test]
async fn provider_configuration_change_and_corrupt_cache_fail_closed() {
    let f = Fixture::new("success");
    run_review(f.options(), f.provider()).await.unwrap();
    let calls = f.calls();
    let mut changed = CodexProvider::new(f.root().join("repo"), review_schema());
    changed.executable = f.root().join("provider");
    changed.timeout_secs = 5;
    changed.model = Some("different-model".into());
    assert!(run_review(f.options(), Arc::new(changed)).await.is_err());
    assert_eq!(f.calls(), calls);
    let state_path = f.root().join("state/state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["outputs"]["correctness"][0]["path"] = serde_json::json!("../outside.rs");
    fs::write(state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(run_review(f.options(), f.provider()).await.is_err());
    assert_eq!(f.calls(), calls);
}

#[tokio::test]
async fn nonzero_exit_exhausts_bounded_retries_and_retains_usage() {
    let f = Fixture::new("failed");
    assert!(run_review(f.options(), f.provider()).await.is_err());
    let calls = f.calls();
    assert_eq!(calls, 2);
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root().join("state/state.json")).unwrap()).unwrap();
    let attempts = state["report"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(
        attempts
            .iter()
            .all(|a| a["status"] == "failed" && a["usage"]["input_tokens"] == 10)
    );
    assert!(
        state["report"]["completed_roles"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(run_review(f.options(), f.provider()).await.is_err());
    assert_eq!(f.calls(), calls);
}
#[tokio::test]
async fn binary_changed_file_is_rejected_before_provider() {
    let f = Fixture::new("success");
    fs::write(f.root().join("repo/a.rs"), [0xff, 0, 0xfe]).unwrap();
    assert!(run_review(f.options(), f.provider()).await.is_err());
    assert_eq!(f.calls(), 0);
}
#[tokio::test]
async fn normalized_git_patch_cannot_hide_changed_file_bytes() {
    let f = Fixture::new("success");
    assert!(
        Command::new("git")
            .args(["config", "core.autocrlf", "true"])
            .current_dir(f.root().join("repo"))
            .status()
            .unwrap()
            .success()
    );
    run_review(f.options(), f.provider()).await.unwrap();
    let calls = f.calls();
    let before = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(f.root().join("repo"))
        .output()
        .unwrap()
        .stdout;
    let file = f.root().join("repo/a.rs");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(file, text.replace('\n', "\r\n")).unwrap();
    let after = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(f.root().join("repo"))
        .output()
        .unwrap()
        .stdout;
    assert_eq!(before, after, "fixture must preserve normalized patch");
    assert!(run_review(f.options(), f.provider()).await.is_err());
    assert_eq!(f.calls(), calls);
}
#[tokio::test]
async fn provider_output_limit_and_timeout_are_unknown_not_free_retries() {
    for script in [
        "#!/usr/bin/python3\nimport sys\nsys.stdin.read()\nprint('x'*10000)\n",
        "#!/usr/bin/python3\nimport sys,time\nsys.stdin.read()\ntime.sleep(10)\n",
    ] {
        let f = Fixture::new("success");
        fs::write(f.root().join("provider"), script).unwrap();
        let mut provider = CodexProvider::new(f.root().join("repo"), review_schema());
        provider.executable = f.root().join("provider");
        provider.timeout_secs = 1;
        provider.max_output_bytes = 1024;
        assert!(run_review(f.options(), Arc::new(provider)).await.is_err());
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(f.root().join("state/state.json")).unwrap()).unwrap();
        let attempts = state["report"]["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["status"], "interrupted");
        assert!(attempts[0]["usage"]["input_tokens"].is_null());
    }
}
#[tokio::test]
async fn terminal_usage_survives_provider_hang_after_completion() {
    use daf_review::provider::{ProviderRequest, ReviewProvider};
    let f = Fixture::new("success");
    let script = fs::read_to_string(f.root().join("provider")).unwrap();
    fs::write(
        f.root().join("provider"),
        format!("{script}\nsys.stdout.flush()\ntime.sleep(30)\n"),
    )
    .unwrap();
    let mut provider = CodexProvider::new(f.root().join("repo"), review_schema());
    provider.executable = f.root().join("provider");
    // Allow interpreter startup under parallel test load before the receipt.
    provider.timeout_secs = 5;
    let response = provider
        .execute(ProviderRequest {
            role: "correctness".into(),
            prompt: "fixture".into(),
        })
        .await;
    assert!(response.error.is_some());
    assert!(!response.outcome_unknown);
    assert_eq!(response.usage.input_tokens, Some(10));
    assert_eq!(response.usage.cached_input_tokens, Some(3));
    assert_eq!(response.usage.output_tokens, Some(5));
}

#[cfg(unix)]
#[tokio::test]
async fn changed_symlinks_are_rejected_before_provider_even_when_dangling() {
    for target in ["missing-target", "existing-target"] {
        let f = Fixture::new("success");
        let repo = f.root().join("repo");
        fs::write(repo.join("existing-target"), "target contents\n").unwrap();
        fs::remove_file(repo.join("a.rs")).unwrap();
        std::os::unix::fs::symlink(target, repo.join("a.rs")).unwrap();
        let error = run_review(f.options(), f.provider()).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("changed path must be regular file"),
            "{target}: {error}"
        );
        assert_eq!(
            f.calls(),
            0,
            "{target} must fail before provider invocation"
        );
    }
}
