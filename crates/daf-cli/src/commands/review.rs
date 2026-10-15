//! Reusable changed-file review backed by DAF specialist orchestration.
use anyhow::Result;
use clap::Args;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Args)]
pub struct ReviewArgs {
    /// Git working tree to snapshot (tracked changes, including staged changes).
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Commit against which to compare the working tree.
    #[arg(long, default_value = "HEAD")]
    pub base: String,
    /// Durable run directory. Reuse with identical inputs to resume.
    #[arg(long)]
    pub state_dir: PathBuf,
    /// Codex executable (also supports a compatible JSONL fixture).
    #[arg(long, default_value = "codex")]
    pub provider_executable: PathBuf,
    /// Provider model; omitted uses the provider default.
    #[arg(long)]
    pub model: Option<String>,
    /// Deadline per specialist attempt in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,
    /// Additional attempts per reviewer, cumulative across resumes.
    #[arg(long, default_value_t = 1)]
    pub max_retries: u32,
    /// Permit another potentially billed call after interrupted, unknown usage.
    #[arg(long)]
    pub allow_unknown_retry: bool,
}

pub async fn exec(args: &ReviewArgs, _: &crate::Cli) -> Result<()> {
    let options = daf_review::ReviewOptions {
        repo: args.repo.clone(),
        base: args.base.clone(),
        state_dir: args.state_dir.clone(),
        timeout_secs: args.timeout_secs,
        max_retries: args.max_retries,
        allow_unknown_retry: args.allow_unknown_retry,
    };
    let mut provider = daf_review::provider::CodexProvider::new(
        args.repo.canonicalize()?,
        daf_review::validation::review_schema(),
    );
    provider.executable = args.provider_executable.clone();
    provider.model = args.model.clone();
    provider.timeout_secs = args.timeout_secs;
    let report = daf_review::run_review(options, Arc::new(provider)).await?;
    let mut output = serde_json::to_value(&report)?;
    output["usage_summary"] = serde_json::to_value(report.usage_summary())?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
