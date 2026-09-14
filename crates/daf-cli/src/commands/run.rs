//! Execute a validated dependency graph of local commands.
use crate::Cli;
use anyhow::{Context, Result, bail};
use dialoguer::Confirm;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    pub mission: PathBuf,
    #[arg(short, long)]
    pub yes: bool,
    #[arg(long, default_value_t = 8)]
    pub parallelism: usize,
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
}
#[derive(Debug, Deserialize)]
struct MissionFile {
    mission: MissionSpec,
}
#[derive(Debug, Deserialize)]
struct MissionSpec {
    name: String,
    #[serde(default)]
    tasks: Vec<TaskSpec>,
}
#[derive(Debug, Clone, Deserialize)]
struct TaskSpec {
    name: String,
    agent: String,
    #[serde(default)]
    depends_on: Vec<String>,
    params: Params,
}
#[derive(Debug, Clone, Deserialize)]
struct Params {
    command: Vec<String>,
    cwd: Option<PathBuf>,
}

fn validate(tasks: &[TaskSpec]) -> Result<()> {
    if tasks.is_empty() {
        bail!("Mission has no tasks");
    }
    let mut names = HashSet::new();
    for task in tasks {
        if task.name.is_empty() || !names.insert(task.name.clone()) {
            bail!("Task names must be unique and nonempty");
        }
        if task.agent != "local" {
            bail!(
                "Agent '{}' has no connected dispatcher; use agent: local with params.command argv",
                task.agent
            );
        }
        if task.params.command.is_empty() || task.params.command[0].is_empty() {
            bail!("Task '{}' needs a nonempty command argv", task.name);
        }
    }
    let mut visited = HashSet::new();
    loop {
        let count = visited.len();
        for task in tasks {
            if task.depends_on.iter().all(|d| visited.contains(d)) {
                visited.insert(task.name.clone());
            }
        }
        if visited.len() == tasks.len() {
            return Ok(());
        }
        if visited.len() == count {
            bail!("Mission has a dependency cycle or unknown dependency");
        }
    }
}

async fn execute(task: TaskSpec, timeout: u64, root: PathBuf) -> (String, bool) {
    let mut command = tokio::process::Command::new(&task.params.command[0]);
    command
        .args(&task.params.command[1..])
        .current_dir(
            task.params
                .cwd
                .as_ref()
                .map(|p| root.join(p))
                .unwrap_or(root),
        )
        .kill_on_drop(true);
    eprintln!("[running] {}", task.name);
    let ok = match tokio::time::timeout(Duration::from_secs(timeout), command.status()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(error)) => {
            eprintln!("{}: {error}", task.name);
            false
        }
        Err(_) => {
            eprintln!("{}: timed out after {timeout}s", task.name);
            false
        }
    };
    eprintln!("[{}] {}", if ok { "passed" } else { "failed" }, task.name);
    (task.name, ok)
}

pub async fn exec(args: &RunArgs, _cli: &Cli) -> Result<()> {
    if args.parallelism == 0 || args.parallelism > 64 || args.timeout == 0 {
        bail!("parallelism must be 1..64 and timeout must be positive");
    }
    let path = args
        .mission
        .canonicalize()
        .context("Cannot locate mission file")?;
    let raw = std::fs::read_to_string(&path)?;
    let mission = serde_yaml_ng::from_str::<MissionFile>(&raw)
        .context("Invalid mission YAML")?
        .mission;
    validate(&mission.tasks)?;
    eprintln!(
        "Mission: {} ({} local commands)",
        mission.name,
        mission.tasks.len()
    );
    for task in &mission.tasks {
        eprintln!(
            "  {}: {:?} after {:?}",
            task.name, task.params.command, task.depends_on
        );
    }
    if !args.yes
        && !Confirm::new()
            .with_prompt("Execute these commands?")
            .default(false)
            .interact()?
    {
        bail!("Mission cancelled");
    }
    let root = path
        .parent()
        .context("Mission has no directory")?
        .to_path_buf();
    let mut pending = mission.tasks;
    let mut results: HashMap<String, bool> = HashMap::new();
    let mut active = tokio::task::JoinSet::new();
    while !pending.is_empty() || !active.is_empty() {
        let mut index = 0;
        while index < pending.len() {
            if pending[index]
                .depends_on
                .iter()
                .any(|d| results.get(d) == Some(&false))
            {
                let task = pending.remove(index);
                eprintln!("[skipped] {}: dependency failed", task.name);
                results.insert(task.name, false);
            } else if active.len() < args.parallelism
                && pending[index]
                    .depends_on
                    .iter()
                    .all(|d| results.get(d) == Some(&true))
            {
                let task = pending.remove(index);
                active.spawn(execute(task, args.timeout, root.clone()));
            } else {
                index += 1;
            }
        }
        if let Some(result) = active.join_next().await {
            let (name, ok) = result?;
            results.insert(name, ok);
        }
    }
    let failed = results.values().filter(|ok| !**ok).count();
    eprintln!(
        "{} passed, {} failed or skipped",
        results.len() - failed,
        failed
    );
    if failed > 0 {
        bail!("Mission failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(name: &str, deps: &[&str]) -> TaskSpec {
        TaskSpec {
            name: name.into(),
            agent: "local".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            params: Params {
                command: vec!["true".into()],
                cwd: None,
            },
        }
    }
    #[test]
    fn validates_out_of_order_graph() {
        assert!(validate(&[task("b", &["a"]), task("a", &[])]).is_ok());
    }
    #[test]
    fn rejects_invalid_graphs_before_execution() {
        assert!(validate(&[task("a", &["b"]), task("b", &["a"])]).is_err());
        assert!(validate(&[task("a", &["missing"])]).is_err());
        assert!(validate(&[task("a", &[]), task("a", &[])]).is_err());
    }
    #[test]
    fn rejects_unconnected_agents() {
        let mut t = task("a", &[]);
        t.agent = "remote".into();
        assert!(validate(&[t]).is_err());
    }
}
