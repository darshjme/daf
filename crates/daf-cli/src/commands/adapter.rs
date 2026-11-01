//! Portable process adapter; valid operations emit a single JSON document.
#[derive(Debug, clap::Subcommand)]
pub enum AdapterCommand {
    /// Describe protocol version and enforced bounds.
    Capabilities,
    /// Execute an explicit argv request once; use - to read JSON from stdin.
    Run { request: String },
}
pub async fn exec(command: &AdapterCommand) -> anyhow::Result<bool> {
    match command {
        AdapterCommand::Capabilities => println!("{}", daf_adapter::capabilities()),
        AdapterCommand::Run { request } => {
            let outcome = daf_adapter::execute(daf_adapter::read_request(request).await?).await?;
            println!("{}", serde_json::to_string(&outcome)?);
            if outcome.status != "succeeded" {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
