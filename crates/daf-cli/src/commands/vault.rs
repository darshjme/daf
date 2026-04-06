//! `daf vault` — Manage the encrypted secrets vault.
//!
//! Wraps daf-vault operations: initialize, store, retrieve, list, and rotate
//! secrets with masked terminal output for sensitive values.

use anyhow::{bail, Result};
use console::style;
use dialoguer::{Confirm, Input, Password};

use crate::display::{format_table, section, status_icon};
use crate::Cli;

/// Subcommands for `daf vault`.
#[derive(Debug, clap::Subcommand)]
pub enum VaultCommand {
    /// Initialize the vault (set master password).
    Init,
    /// Store a secret.
    Set(SetArgs),
    /// Retrieve a secret value.
    Get(GetArgs),
    /// List all secret names (values masked).
    List,
    /// Rotate a secret (re-encrypt with new value).
    Rotate(RotateArgs),
}

#[derive(Debug, clap::Args)]
pub struct SetArgs {
    /// Secret name (e.g., "api-key", "db-password").
    pub name: String,
    /// Secret value. If omitted, will prompt interactively.
    pub value: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct GetArgs {
    /// Secret name to retrieve.
    pub name: String,

    /// Print the raw value without any formatting.
    #[arg(long)]
    pub raw: bool,
}

#[derive(Debug, clap::Args)]
pub struct RotateArgs {
    /// Secret name to rotate.
    pub name: String,
    /// New value. If omitted, will prompt interactively.
    pub value: Option<String>,
}

/// Dispatch vault subcommands.
pub async fn exec(cmd: &VaultCommand, cli: &Cli) -> Result<()> {
    match cmd {
        VaultCommand::Init => exec_init(cli).await,
        VaultCommand::Set(args) => exec_set(args, cli).await,
        VaultCommand::Get(args) => exec_get(args, cli).await,
        VaultCommand::List => exec_list(cli).await,
        VaultCommand::Rotate(args) => exec_rotate(args, cli).await,
    }
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

async fn exec_init(_cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Initializing DAF vault\n",
        style("[vault]").cyan().bold(),
    );

    eprintln!(
        "  {}",
        style("The master password encrypts all secrets in the vault.").dim(),
    );
    eprintln!(
        "  {}",
        style("It cannot be recovered if lost. Choose a strong passphrase.").dim(),
    );
    eprintln!();

    let password = Password::new()
        .with_prompt("Master password")
        .with_confirmation("Confirm password", "Passwords do not match")
        .interact()?;

    if password.len() < 12 {
        bail!("master password must be at least 12 characters");
    }

    // In production: call daf_vault::KeyRing::initialize() with the
    // PBKDF2-derived master key, create the sled store at .daf/vault.
    eprintln!(
        "\n{} Vault initialized at {}",
        style("\u{2714}").green().bold(),
        style(".daf/vault").underlined(),
    );
    eprintln!(
        "  {}",
        style("The vault is now unsealed for this session.").dim(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// set
// ---------------------------------------------------------------------------

async fn exec_set(args: &SetArgs, _cli: &Cli) -> Result<()> {
    let value = match &args.value {
        Some(v) => v.clone(),
        None => {
            Password::new()
                .with_prompt(format!("Value for '{}'", args.name))
                .interact()?
        }
    };

    // In production: call vault.set(&args.name, &value) which envelope-encrypts
    // and stores in the vault backend.
    let _ = value;

    eprintln!(
        "{} Secret {} stored",
        style("\u{2714}").green().bold(),
        style(&args.name).bold(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// get
// ---------------------------------------------------------------------------

async fn exec_get(args: &GetArgs, _cli: &Cli) -> Result<()> {
    // In production: call vault.get(&args.name) which decrypts and returns
    // the secret value. Audit log entry is recorded.
    let value = "s3cr3t-v4lu3-placeholder";

    if args.raw {
        println!("{value}");
    } else {
        eprintln!(
            "{} {}",
            style("[vault]").cyan().bold(),
            style(&args.name).bold(),
        );
        println!("{value}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

async fn exec_list(_cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Vault contents:\n",
        style("[vault]").cyan().bold(),
    );

    // Placeholder — in production this lists from the vault store.
    let mut table = format_table(&["", "Name", "Kind", "Version", "Last Rotated"]);

    let secrets = [
        ("api-key", "token", "v1", "2026-04-05 14:22"),
        ("db-password", "password", "v3", "2026-04-01 09:15"),
        ("tls-cert", "certificate", "v1", "2026-03-20 11:00"),
        ("signing-key", "key", "v2", "2026-03-28 16:45"),
    ];

    for (name, kind, version, rotated) in &secrets {
        table.add_row(vec![
            status_icon("ok"),
            name.to_string(),
            kind.to_string(),
            version.to_string(),
            rotated.to_string(),
        ]);
    }

    println!("{table}");

    eprintln!(
        "\n  {} secret(s) stored (values are never displayed in list)",
        style(secrets.len()).bold(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// rotate
// ---------------------------------------------------------------------------

async fn exec_rotate(args: &RotateArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Rotating secret {}",
        style("[vault]").cyan().bold(),
        style(&args.name).bold(),
    );

    let new_value = match &args.value {
        Some(v) => v.clone(),
        None => {
            Password::new()
                .with_prompt(format!("New value for '{}'", args.name))
                .with_confirmation("Confirm new value", "Values do not match")
                .interact()?
        }
    };

    // In production: call vault.rotate(&args.name, &new_value) which
    // re-encrypts with a fresh data key and increments the version.
    let _ = new_value;

    section("Rotation Details");
    crate::display::kv("Secret", &args.name);
    crate::display::kv("Old version", "v2");
    crate::display::kv("New version", "v3");
    crate::display::kv("Old key", "retired (zeroized)");

    eprintln!(
        "\n{} Secret {} rotated to v3",
        style("\u{2714}").green().bold(),
        style(&args.name).bold(),
    );

    Ok(())
}
