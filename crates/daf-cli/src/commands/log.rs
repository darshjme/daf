//! `daf log` — Query and stream agent logs, conversations, and episodes.
//!
//! No live logger backend is connected. Commands fail without fabricating entries.

use crate::Cli;
use anyhow::Result;

/// Subcommands for `daf log`.
#[derive(Debug, clap::Subcommand)]
pub enum LogCommand {
    /// Live-stream all agent conversations.
    Tail(TailArgs),
    /// Search log entries by query string.
    Search(SearchArgs),
    /// Show a full conversation transcript.
    Conversation(ConversationArgs),
    /// Extract knowledge base entries from a conversation.
    Extract(ExtractArgs),
    /// List recorded episodes.
    Episodes(EpisodesArgs),
}

#[derive(Debug, clap::Args)]
pub struct TailArgs {
    /// Number of recent entries to show before streaming.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub lines: usize,

    /// Filter by agent name or ID.
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Filter by log level.
    #[arg(short, long)]
    pub level: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    /// Search query (supports regex).
    pub query: String,

    /// Maximum results to return.
    #[arg(short, long, default_value_t = 50)]
    pub max: usize,

    /// Filter by agent.
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Time window (e.g., "1h", "30m", "7d").
    #[arg(short, long)]
    pub since: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ConversationArgs {
    /// Conversation ID.
    pub id: String,

    /// Show raw message payloads.
    #[arg(long)]
    pub raw: bool,
}

#[derive(Debug, clap::Args)]
pub struct ExtractArgs {
    /// Conversation ID to extract from.
    pub conversation_id: String,

    /// Output format for extracted entries.
    #[arg(long, default_value = "yaml")]
    pub output: String,
}

#[derive(Debug, clap::Args)]
pub struct EpisodesArgs {
    /// Filter by agent.
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Maximum episodes to list.
    #[arg(short, long, default_value_t = 20)]
    pub max: usize,
}

/// Dispatch log subcommands.
pub async fn exec(_args: &LogCommand, _cli: &Cli) -> Result<()> {
    super::unavailable("log")
}
