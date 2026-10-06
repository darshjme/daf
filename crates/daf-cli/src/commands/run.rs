//! Execute a validated dependency graph of local commands.
use crate::{Cli, OutputFormat};
use anyhow::{Context, Result, bail};
use dialoguer::Confirm;
use serde::{Deserialize, Serialize};
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
#[serde(deny_unknown_fields)]
struct MissionFile {
    mission: MissionSpec,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionSpec {
    name: String,
    #[serde(default)]
    tasks: Vec<TaskSpec>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskSpec {
    name: String,
    agent: String,
    #[serde(default)]
    depends_on: Vec<String>,
    params: Params,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
        if task.name.trim().is_empty() || !names.insert(task.name.clone()) {
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
    for task in tasks {
        let mut deps = HashSet::new();
        for dep in &task.depends_on {
            if !names.contains(dep) {
                bail!(
                    "Task '{}' references unknown dependency '{}'",
                    task.name,
                    dep
                );
            }
            if !deps.insert(dep) {
                bail!("Task '{}' repeats dependency '{}'", task.name, dep);
            }
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

#[derive(Debug, Serialize)]
struct TaskResult {
    name: String,
    status: &'static str,
    exit_code: Option<i32>,
    error: Option<String>,
}

impl TaskResult {
    fn passed(&self) -> bool {
        self.status == "passed"
    }
}

async fn execute(task: TaskSpec, timeout: u64, root: PathBuf, json: bool) -> TaskResult {
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
    if json {
        // Keep stdout parseable while retaining command output for humans.
        command.stdout(std::process::Stdio::from(std::io::stderr()));
    }
    eprintln!("[running] {}", task.name);
    let mut result = TaskResult {
        name: task.name,
        status: "failed",
        exit_code: None,
        error: None,
    };
    match command.spawn() {
        Ok(mut child) => {
            match tokio::time::timeout(Duration::from_secs(timeout), child.wait()).await {
                Ok(Ok(status)) => {
                    result.exit_code = status.code();
                    if status.success() {
                        result.status = "passed";
                    } else {
                        result.error = Some(format!("command exited with {status}"));
                    }
                }
                Ok(Err(error)) => {
                    result.error = Some(error.to_string());
                }
                Err(_) => {
                    result.status = "timed_out";
                    result.error = Some(format!("timed out after {timeout}s"));
                    // Reap the command before releasing its concurrency slot.
                    if let Err(error) = child.kill().await {
                        result.error = Some(format!(
                            "timed out after {timeout}s; termination failed: {error}"
                        ));
                    }
                }
            }
        }
        Err(error) => {
            result.error = Some(error.to_string());
        }
    }
    if let Some(error) = &result.error {
        eprintln!("{}: {error}", result.name);
    }
    eprintln!("[{}] {}", result.status, result.name);
    result
}

pub async fn exec(args: &RunArgs, cli: &Cli) -> Result<()> {
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
    if mission.name.trim().is_empty() {
        bail!("Mission name must be nonempty");
    }
    validate(&mission.tasks)?;
    let root = path
        .parent()
        .context("Mission has no directory")?
        .to_path_buf();
    for task in &mission.tasks {
        if let Some(cwd) = &task.params.cwd {
            if !root.join(cwd).is_dir() {
                bail!(
                    "Task '{}' working directory does not exist: {}",
                    task.name,
                    root.join(cwd).display()
                );
            }
        }
    }
    let order: Vec<_> = mission.tasks.iter().map(|task| task.name.clone()).collect();
    let json = matches!(cli.format, OutputFormat::Json);
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
    let mut pending = mission.tasks;
    let mut results: HashMap<String, TaskResult> = HashMap::new();
    let mut active = tokio::task::JoinSet::new();
    while !pending.is_empty() || !active.is_empty() {
        let mut index = 0;
        while index < pending.len() {
            if pending[index]
                .depends_on
                .iter()
                .any(|d| results.get(d).is_some_and(|result| !result.passed()))
            {
                let task = pending.remove(index);
                eprintln!("[skipped] {}: dependency failed", task.name);
                results.insert(
                    task.name.clone(),
                    TaskResult {
                        name: task.name,
                        status: "skipped",
                        exit_code: None,
                        error: Some("dependency failed".into()),
                    },
                );
            } else if active.len() < args.parallelism
                && pending[index]
                    .depends_on
                    .iter()
                    .all(|d| results.get(d).is_some_and(TaskResult::passed))
            {
                let task = pending.remove(index);
                active.spawn(execute(task, args.timeout, root.clone(), json));
            } else {
                index += 1;
            }
        }
        if let Some(result) = active.join_next().await {
            let result = result?;
            results.insert(result.name.clone(), result);
        }
    }
    let failed = results.values().filter(|result| !result.passed()).count();
    eprintln!(
        "{} passed, {} failed or skipped",
        results.len() - failed,
        failed
    );
    if json {
        let tasks: Vec<_> = order.iter().filter_map(|name| results.get(name)).collect();
        crate::display::format_json(
            &serde_json::json!({"mission": mission.name, "status": if failed == 0 { "passed" } else { "failed" }, "tasks": tasks}),
        )?;
    }
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
