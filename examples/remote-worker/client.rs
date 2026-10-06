//! Persisted two-task manifest makes retries use the original task identities.
use daf_runtime::remote::{RemoteClient, TaskEnvelope};
use daf_sdk::task_types::SdkTaskSpec;
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut address: SocketAddr = "127.0.0.1:7474".parse()?;
    let mut manifest = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--connect" => address = args.next().ok_or("--connect requires address")?.parse()?,
            "--mission" => {
                manifest = Some(PathBuf::from(args.next().ok_or("--mission requires path")?))
            }
            "--help" | "-h" => {
                println!("daf-example-remote-client --mission PATH [--connect 127.0.0.1:7474]");
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if !address.ip().is_loopback() {
        return Err("development client requires loopback address".into());
    }
    let path = manifest.ok_or("--mission is required")?;
    let tasks: Vec<TaskEnvelope> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent =
                SdkTaskSpec::new("sum", "durable parent").with_inputs(json!({"values": [20,22]}));
            let child =
                SdkTaskSpec::new("sum", "durable dependent").with_inputs(json!({"values": [8]}));
            let tasks = vec![
                TaskEnvelope {
                    task: parent.clone(),
                    dependencies: vec![],
                },
                TaskEnvelope {
                    task: child,
                    dependencies: vec![parent.id],
                },
            ];
            // Persist identity before transmission. Never replace an existing manifest.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            use std::io::Write;
            file.write_all(&serde_json::to_vec_pretty(&tasks)?)?;
            file.sync_all()?;
            tasks
        }
        Err(error) => return Err(error.into()),
    };
    let token = std::env::var("DAF_REMOTE_TOKEN").map_err(|_| "set DAF_REMOTE_TOKEN")?;
    let client = RemoteClient::new(address, None, Duration::from_secs(15), 1024 * 1024)?;
    for task in tasks {
        let result = client.submit(&token, task).await?;
        println!("{}", serde_json::to_string(&result)?);
        if !result.success {
            return Err("task failed; dependent task was not submitted".into());
        }
    }
    Ok(())
}
