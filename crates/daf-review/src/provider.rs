//! Bounded native CLI execution and provider-reported usage.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub source: String,
}
#[derive(Clone, Debug)]
pub struct ProviderRequest {
    pub role: String,
    pub prompt: String,
}
#[derive(Clone, Debug)]
pub struct ProviderAttempt {
    pub output: Option<Value>,
    pub usage: Usage,
    pub error: Option<String>,
    pub outcome_unknown: bool,
}
#[async_trait]
pub trait ReviewProvider: Send + Sync {
    fn identity(&self) -> String;
    async fn execute(&self, request: ProviderRequest) -> ProviderAttempt;
}
#[derive(Clone, Debug)]
pub struct CodexProvider {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub schema: Value,
    pub model: Option<String>,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
}
// Resolve in the caller's directory before the provider moves to its isolated cwd.
// The same selection feeds both cache identity and process launch.
fn resolve_executable(executable: &std::path::Path) -> std::io::Result<PathBuf> {
    if executable.components().count() > 1 || executable.is_absolute() {
        return std::fs::canonicalize(executable);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let candidate = directory.join(executable);
            let Ok(metadata) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return std::fs::canonicalize(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "provider executable not found in PATH",
    ))
}
fn executable_fingerprint(executable: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(resolve_executable(executable).ok()?).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().to_hex().to_string())
}
impl CodexProvider {
    pub fn new(cwd: PathBuf, schema: Value) -> Self {
        Self {
            executable: "codex".into(),
            cwd,
            schema,
            model: None,
            timeout_secs: 180,
            max_output_bytes: 4 * 1024 * 1024,
        }
    }
}
// A cancelled async task drops this guard as well as the direct child. Keep all
// provider descendants in the same process group so pipes/processes cannot leak.
#[cfg(unix)]
struct ProcessGroup(i32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: negative pid addresses the child group created by process_group(0).
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}
async fn capture<R: AsyncRead + Unpin>(
    mut reader: R,
    bytes: Arc<Mutex<Vec<u8>>>,
    cap: usize,
) -> std::io::Result<()> {
    let mut chunk = [0; 8192];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        let mut out = bytes.lock().expect("capture lock");
        if out.len().saturating_add(n) > cap {
            let remaining = cap.saturating_sub(out.len());
            out.extend_from_slice(&chunk[..remaining]);
            return Err(std::io::Error::other("provider output exceeded byte limit"));
        }
        out.extend_from_slice(&chunk[..n]);
    }
}
// Store categories only: subprocess diagnostics can contain local paths or credentials.
fn diagnostic_category(bytes: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if text.contains("operation not permitted") || text.contains("permission denied") {
        ": permission denied"
    } else if text.contains("unauthorized")
        || text.contains("authentication")
        || text.contains("401")
    {
        ": authentication failure"
    } else if text.contains("failed to connect")
        || text.contains("network")
        || text.contains("error sending request")
        || text.contains("connection")
    {
        ": connection failure"
    } else if text.contains("model") && (text.contains("unsupported") || text.contains("not found"))
    {
        ": unsupported model"
    } else {
        ""
    }
}
/// Parse only native event envelopes. Never infer usage from response prose.
pub fn parse_codex_events(bytes: &[u8]) -> ProviderAttempt {
    let mut response = ProviderAttempt {
        output: None,
        usage: Usage::default(),
        error: None,
        outcome_unknown: true,
    };
    let mut completed = false;
    let mut ambiguous_usage = false;
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => {
                response.error = Some("invalid provider JSON event".into());
                continue;
            }
        };
        match event["type"].as_str() {
            Some("turn.completed") => {
                if completed {
                    response.error = Some("multiple completed turns are unsupported".into());
                    ambiguous_usage = true;
                }
                completed = true;
                response.outcome_unknown = false;
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    response.usage = Usage {
                        input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
                        cached_input_tokens: usage
                            .get("cached_input_tokens")
                            .and_then(Value::as_u64),
                        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
                        source: "codex.turn.completed.usage".into(),
                    };
                }
            }
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                if let Some(text) = event["item"]["text"].as_str() {
                    response.output = serde_json::from_str(text).ok();
                }
            }
            Some("turn.failed") => {
                response.error = Some("provider reported failed turn".into());
                response.outcome_unknown = false;
            }
            Some("error") => response.error = Some("provider reported error".into()),
            _ => {}
        }
    }
    if ambiguous_usage {
        // Unsupported streams may contain cumulative or per-turn receipts. Do not
        // report the last receipt as the complete cost of this attempt.
        response.usage = Usage {
            source: "codex.multiple_completed_turns.unknown".into(),
            ..Usage::default()
        };
    }
    if !completed && response.error.is_none() {
        response.error = Some("provider did not complete a turn".into());
    }
    if response.output.is_none() && response.error.is_none() {
        response.error = Some("provider final response is not JSON".into());
    }
    response
}
#[async_trait]
impl ReviewProvider for CodexProvider {
    fn identity(&self) -> String {
        // Includes every configured option affecting execution or the output contract.
        serde_json::json!({"adapter":"codex-cli-v2","executable":self.executable,"executable_blake3":executable_fingerprint(&self.executable),"cwd":self.cwd,"model":self.model,"schema":self.schema,"timeout_secs":self.timeout_secs,"max_output_bytes":self.max_output_bytes,"sandbox":"read-only","ignore_user_config":true,"isolated_working_directory":true,"project_doc_max_bytes":0}).to_string()
    }
    async fn execute(&self, request: ProviderRequest) -> ProviderAttempt {
        let fail = |e: String| ProviderAttempt {
            output: None,
            usage: Usage::default(),
            error: Some(e),
            outcome_unknown: false,
        };
        let temp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => return fail(format!("create provider temp directory: {e}")),
        };
        let schema_path = temp.path().join("schema.json");
        if let Err(e) = std::fs::write(&schema_path, self.schema.to_string()) {
            return fail(format!("write provider schema: {e}"));
        }
        let executable = match resolve_executable(&self.executable) {
            Ok(path) => path,
            Err(e) => return fail(format!("resolve provider executable: {e}")),
        };
        let mut command = Command::new(executable);
        command.current_dir(temp.path());
        command
            .args([
                "exec",
                "--json",
                "--ephemeral",
                "--ignore-user-config",
                "--config",
                "project_doc_max_bytes=0",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--output-schema",
            ])
            .arg(&schema_path)
            .arg("-C")
            .arg(temp.path());
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        command
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => return fail(format!("launch provider: {e}")),
        };
        #[cfg(unix)]
        let _group = ProcessGroup(child.id().expect("spawned child pid") as i32);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let mut stdin = child.stdin.take().expect("piped stdin");
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let result = tokio::time::timeout(Duration::from_secs(self.timeout_secs), async {
            tokio::try_join!(
                async move {
                    stdin.write_all(request.prompt.as_bytes()).await?;
                    stdin.shutdown().await?;
                    drop(stdin);
                    Ok::<(), std::io::Error>(())
                },
                capture(stdout, out.clone(), self.max_output_bytes),
                capture(stderr, err.clone(), self.max_output_bytes),
                child.wait()
            )
        })
        .await;
        let process_error = match result {
            Ok(Ok((_, _, _, status))) if status.success() => None,
            Ok(Ok((_, _, _, status))) => Some(format!(
                "provider exited with {status}{}",
                diagnostic_category(&err.lock().expect("capture lock"))
            )),
            Ok(Err(e)) => {
                let _ = child.kill().await;
                Some(format!("provider IO failure: {e}"))
            }
            Err(_) => {
                let _ = child.kill().await;
                Some("provider timed out".into())
            }
        };
        let mut response = parse_codex_events(&out.lock().expect("capture lock"));
        if let Some(error) = process_error {
            response.error = Some(error);
        }
        response
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn provider_uses_isolated_cwd_and_disables_project_documents() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("provider.py");
        std::fs::write(&script, r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
assert args[args.index('--config') + 1] == 'project_doc_max_bytes=0'
root = args[args.index('-C') + 1]
assert os.path.realpath(root) == os.path.realpath(os.getcwd())
assert os.path.realpath(root) == os.path.realpath(os.path.dirname(args[args.index('--output-schema') + 1]))
assert os.path.realpath(root) != os.path.realpath(os.path.dirname(__file__))
sys.stdin.read()
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'{\"findings\":[]}'}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'cached_input_tokens':0,'output_tokens':1}}))
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut provider = CodexProvider::new(dir.path().to_owned(), serde_json::json!({}));
        provider.executable = script;
        let response = provider
            .execute(ProviderRequest {
                role: "correctness".into(),
                prompt: "Review".into(),
            })
            .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(response.usage.input_tokens, Some(1));
        assert!(provider.identity().contains("codex-cli-v2"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn provider_resolves_relative_path_before_isolation() {
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "DAF_PROVIDER_PATH_TEST";
        if let Some(executable) = std::env::var_os(CHILD) {
            let mut provider =
                CodexProvider::new(std::env::current_dir().unwrap(), serde_json::json!({}));
            provider.executable = executable.into();
            let identity: Value = serde_json::from_str(&provider.identity()).unwrap();
            assert!(identity["executable_blake3"].is_string());
            let result = provider
                .execute(ProviderRequest {
                    role: "correctness".into(),
                    prompt: "Review".into(),
                })
                .await;
            assert!(result.error.is_none(), "{:?}", result.error);
            assert_eq!(result.usage.input_tokens, Some(1));
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let script = r#"#!/usr/bin/python3
import json,sys
sys.stdin.read()
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'{\"findings\":[]}'}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'cached_input_tokens':0,'output_tokens':1}}))
"#;
        for relative in ["bin/review-provider", "review-provider"] {
            let path = dir.path().join(relative);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir(dir.path().join("non-executable")).unwrap();
        let blocked = dir.path().join("non-executable/review-provider");
        std::fs::write(&blocked, "not executable").unwrap();
        std::fs::set_permissions(blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Each child has its own PATH/cwd; parallel tests retain their environment.
        for (path, executable) in [
            ("bin", "review-provider"),
            ("non-executable:bin", "review-provider"),
            ("", "review-provider"),
            ("bin", "./bin/review-provider"),
        ] {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "provider::tests::provider_resolves_relative_path_before_isolation",
                    "--nocapture",
                ])
                .current_dir(dir.path())
                .env("PATH", path)
                .env(CHILD, executable)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    #[test]
    fn usage_is_reported_independently_of_invalid_final_json() {
        let r = parse_codex_events(
            br#"{"type":"item.completed","item":{"type":"agent_message","text":"bad"}}
{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":0,"output_tokens":3}}
"#,
        );
        assert!(r.error.is_some());
        assert_eq!(r.usage.input_tokens, Some(12));
        assert_eq!(r.usage.cached_input_tokens, Some(0));
    }
    #[test]
    fn multiple_completed_turns_cannot_claim_complete_usage() {
        let event = b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":10,\"output_tokens\":20}}\n";
        for count in [2, 3] {
            let r = parse_codex_events(&event.repeat(count));
            assert_eq!(
                r.error.as_deref(),
                Some("multiple completed turns are unsupported")
            );
            assert_eq!(r.usage.input_tokens, None);
            assert_eq!(r.usage.cached_input_tokens, None);
            assert_eq!(r.usage.output_tokens, None);
            assert_eq!(r.usage.source, "codex.multiple_completed_turns.unknown");
        }
    }
    #[test]
    fn absent_and_invalid_counters_are_unknown() {
        let r = parse_codex_events(
            br#"{"type":"item.completed","item":{"type":"agent_message","text":"{}"}}
{"type":"turn.completed","usage":{"input_tokens":-1,"output_tokens":"4"}}
"#,
        );
        assert_eq!(r.usage.input_tokens, None);
        assert_eq!(r.usage.output_tokens, None);
        assert_eq!(r.usage.cached_input_tokens, None);
    }
    #[test]
    fn valid_native_events_and_no_completion() {
        let r = parse_codex_events(br#"{"type":"item.completed","item":{"type":"agent_message","text":"{\"findings\":[]}"}}
{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":5,"output_tokens":3}}
"#);
        assert!(r.error.is_none());
        assert_eq!(r.output.unwrap()["findings"], serde_json::json!([]));
        assert!(parse_codex_events(b"").error.is_some());
    }
}
