//! `daf vault` — Manage the encrypted secrets vault.
//!
//! Wraps daf-vault operations: initialize, store, retrieve, list, and rotate
//! secrets with masked terminal output for sensitive values.

use anyhow::{Result, bail};
use dialoguer::Password;

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

/// Persist secrets through the encrypted Sled vault; no sample values.
pub async fn exec(cmd: &VaultCommand, _cli: &Cli) -> Result<()> {
    use daf_vault::{AccessPolicy, AuditLog, SecretKind, SledVaultStore, VaultStore};
    use std::{path::PathBuf, sync::Arc};
    let path = std::env::var_os("DAF_VAULT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".daf/vault"));
    if !matches!(cmd, VaultCommand::Init) && !path.exists() {
        bail!("Vault not initialized; run daf vault init");
    }
    if matches!(cmd, VaultCommand::Init) && path.exists() {
        bail!("Vault path already exists; refusing to overwrite it");
    }
    let password = match std::env::var("DAF_VAULT_PASSWORD") {
        Ok(value) => value,
        Err(_) => {
            let prompt = Password::new().with_prompt("Master password");
            if matches!(cmd, VaultCommand::Init) {
                prompt
                    .with_confirmation("Confirm password", "Passwords do not match")
                    .interact()?
            } else {
                prompt.interact()?
            }
        }
    };
    if password.len() < 12 {
        bail!("Master password must be at least 12 characters");
    }
    let store = SledVaultStore::open(&path, Arc::new(AuditLog::new()))?;
    if matches!(cmd, VaultCommand::Init) {
        store.initialize(password.as_bytes())?;
        eprintln!("Vault initialized at {}", path.display());
        return Ok(());
    }
    store.unseal(password.as_bytes())?;
    match cmd {
        VaultCommand::Set(args) => {
            let value = match &args.value {
                Some(value) => value.clone(),
                None => Password::new().with_prompt("Secret value").interact()?,
            };
            store
                .store_secret(
                    &args.name,
                    SecretKind::Custom("cli".into()),
                    value.as_bytes(),
                    AccessPolicy::default(),
                )
                .await?;
            eprintln!("Stored {}", args.name);
        }
        VaultCommand::Get(args) => {
            let secret = store.get_secret(&args.name).await?;
            if args.raw {
                use std::io::Write;
                std::io::stdout().write_all(&secret.encrypted_value)?;
            } else {
                println!("{}: [hidden; use --raw to reveal]", args.name);
            }
        }
        VaultCommand::List => {
            for secret in store.list_secrets().await? {
                println!("{} v{}", secret.name, secret.version);
            }
        }
        VaultCommand::Rotate(args) => {
            let value = match &args.value {
                Some(value) => value.clone(),
                None => Password::new().with_prompt("New value").interact()?,
            };
            store.rotate_secret(&args.name, value.as_bytes()).await?;
            eprintln!("Rotated {}", args.name);
        }
        VaultCommand::Init => unreachable!(),
    }
    store.seal();
    Ok(())
}
