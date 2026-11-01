use clap::{Parser, Subcommand};
#[derive(Parser)]
#[command(about = "DAF portable coding-agent process adapter", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Capabilities,
    Run {
        /// JSON request file, or - for stdin.
        request: String,
    },
}
async fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Capabilities => println!("{}", daf_adapter::capabilities()),
        Command::Run { request } => {
            let outcome = daf_adapter::execute(daf_adapter::read_request(&request).await?).await?;
            println!("{}", serde_json::to_string(&outcome)?);
            if outcome.status != "succeeded" {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(2);
    }
}
