//! Versioned, bounded process execution for coding agents. This is not a sandbox.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub const MAX_REQUEST_BYTES: usize = 1_048_576;
pub const MAX_OUTPUT_BYTES: usize = 1_048_576;
fn default_timeout() -> u64 {
    300_000
}
fn default_output() -> usize {
    65_536
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_output")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub schema_version: u32,
    pub id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub fn parse_request(bytes: &[u8]) -> Result<Request> {
    if bytes.len() > MAX_REQUEST_BYTES {
        bail!("request exceeds 1048576 bytes");
    }
    let request: Request = serde_json::from_slice(bytes).context("invalid adapter request")?;
    request.validate()?;
    Ok(request)
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        if serde_json::to_vec(self)?.len() > MAX_REQUEST_BYTES {
            bail!("request exceeds 1048576 bytes");
        }
        if self.schema_version != 1 {
            bail!("unsupported schema_version");
        }
        if self.id.trim().is_empty() || self.id.len() > 128 {
            bail!("id must contain 1..128 UTF-8 bytes");
        }
        if self.argv.is_empty()
            || self.argv[0].is_empty()
            || self.argv.len() > 4096
            || self.argv.iter().any(|s| s.contains('\0'))
        {
            bail!("argv must contain a nonempty executable and at most 4096 NUL-free arguments");
        }
        if !(1..=3_600_000).contains(&self.timeout_ms) {
            bail!("timeout_ms must be 1..3600000");
        }
        if !(1..=MAX_OUTPUT_BYTES).contains(&self.max_output_bytes) {
            bail!("max_output_bytes must be 1..1048576");
        }
        if let Some(cwd) = &self.cwd {
            if cwd.as_os_str().is_empty() || !cwd.is_dir() {
                bail!("cwd must be an existing directory");
            }
        }
        Ok(())
    }
}

/// Limits are raw bytes per stream; invalid/truncated UTF-8 becomes U+FFFD.
async fn capture(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
    retained: &mut Vec<u8>,
    truncated: &mut bool,
) -> Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(limit - retained.len());
        retained.extend_from_slice(&buffer[..keep]);
        *truncated |= keep < count;
    }
    Ok(())
}

pub fn capabilities() -> serde_json::Value {
    serde_json::json!({"schema_version":1,"protocol":"daf.process.v1","shell":false,"retries":false,
        "max_request_bytes":MAX_REQUEST_BYTES,"max_output_bytes":MAX_OUTPUT_BYTES,
        "max_timeout_ms":3600000,"default_timeout_ms":300000,"default_max_output_bytes":65536,
        "output_encoding":"utf-8-lossy","termination_scope":"direct-child",
        "cwd_base":"caller","environment":"inherited","sandbox":false})
}

/// Execute once, concurrently draining both streams and feeding stdin.
/// The deadline includes stdin writing, process execution, and pipe draining.
pub async fn execute(request: Request) -> Result<Outcome> {
    request.validate()?;
    let mut outcome = Outcome {
        schema_version: 1,
        id: request.id.clone(),
        status: "failed".into(),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
    };
    let mut command = tokio::process::Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = error.to_string();
            let bytes = message.as_bytes();
            outcome.stderr =
                String::from_utf8_lossy(&bytes[..bytes.len().min(request.max_output_bytes)])
                    .into_owned();
            outcome.stderr_truncated = bytes.len() > request.max_output_bytes;
            return Ok(outcome);
        }
    };
    let mut input = child.stdin.take().context("missing child stdin")?;
    let output = child.stdout.take().context("missing child stdout")?;
    let error = child.stderr.take().context("missing child stderr")?;
    // Keep capture buffers in this future; no detached tasks survive cancellation.
    let mut out_bytes = Vec::new();
    let mut err_bytes = Vec::new();
    let work = async {
        let write = async {
            if let Some(data) = request.stdin.as_ref() {
                // A child may intentionally exit without consuming all input.
                if let Err(error) = input.write_all(data.as_bytes()).await {
                    if error.kind() != std::io::ErrorKind::BrokenPipe {
                        return Err(error.into());
                    }
                }
            }
            drop(input);
            Ok::<_, anyhow::Error>(())
        };
        let out = capture(
            output,
            request.max_output_bytes,
            &mut out_bytes,
            &mut outcome.stdout_truncated,
        );
        let err = capture(
            error,
            request.max_output_bytes,
            &mut err_bytes,
            &mut outcome.stderr_truncated,
        );
        let wait = async { Ok::<_, anyhow::Error>(child.wait().await?) };
        let (_, _, _, status) = tokio::try_join!(write, out, err, wait)?;
        Ok::<_, anyhow::Error>(status)
    };
    match tokio::time::timeout(Duration::from_millis(request.timeout_ms), work).await {
        Ok(Ok(status)) => {
            outcome.exit_code = status.code();
            if status.success() {
                outcome.status = "succeeded".into();
            }
        }
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let message = error.to_string();
            let room = request.max_output_bytes - err_bytes.len();
            let keep = room.min(message.len());
            err_bytes.extend_from_slice(&message.as_bytes()[..keep]);
            outcome.stderr_truncated |= keep < message.len();
        }
        Err(_) => {
            outcome.status = "timed_out".into();
            let _ = child.kill().await;
        }
    }
    outcome.stdout = String::from_utf8_lossy(&out_bytes).into_owned();
    outcome
        .stderr
        .push_str(&String::from_utf8_lossy(&err_bytes));
    Ok(outcome)
}

/// Bounded file/stdin input. Reading one byte past the cap detects oversize requests.
pub async fn read_request(path: &str) -> Result<Request> {
    let mut bytes = Vec::new();
    if path == "-" {
        tokio::io::stdin()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
    } else {
        tokio::fs::File::open(path)
            .await?
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
    }
    parse_request(&bytes)
}
