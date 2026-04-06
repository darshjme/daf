//! Playbook definitions for declarative agent configuration.
//!
//! A playbook is the top-level unit of execution in DAF-configure, analogous
//! to an Ansible playbook. It contains one or more *plays*, each targeting
//! a set of agents and declaring an ordered list of tasks (and optionally
//! roles) to apply.
//!
//! Playbooks can be authored as Rust structs or parsed from YAML files.
//! The engine evaluates each play sequentially, resolving agent selectors
//! against the inventory, interpolating variables, and running tasks through
//! the module system.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use daf_core::AgentKind;

use crate::condition::Condition;
use crate::handler::Handler;

// ---------------------------------------------------------------------------
// AgentSelector
// ---------------------------------------------------------------------------

/// Selects which agents a play targets.
///
/// Selectors can match by group membership, glob patterns on agent names,
/// or boolean combinations thereof. The inventory evaluates these at
/// runtime to produce the concrete list of target agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSelector {
    /// Target all agents in the inventory.
    All,
    /// Target agents belonging to the named group.
    Group(String),
    /// Target agents whose name matches the glob pattern.
    Pattern(String),
    /// Target a single agent by exact name.
    Agent(String),
    /// Intersection: agents must match all sub-selectors.
    And(Vec<AgentSelector>),
    /// Union: agents matching any sub-selector.
    Or(Vec<AgentSelector>),
    /// Target agents by their [`AgentKind`].
    ByKind(AgentKind),
    /// Exclusion: match the first but not the second.
    Exclude {
        include: Box<AgentSelector>,
        exclude: Box<AgentSelector>,
    },
}

impl AgentSelector {
    /// Check if a given agent name and set of groups matches this selector.
    ///
    /// This is a simplified matcher — the full inventory-based resolution
    /// lives in the inventory module. This method is useful for unit tests
    /// and simple cases.
    pub fn matches(&self, agent_name: &str, groups: &[String]) -> bool {
        match self {
            Self::All => true,
            Self::Group(g) => groups.iter().any(|ag| ag == g),
            Self::Pattern(pat) => glob_match(pat, agent_name),
            Self::Agent(name) => name == agent_name,
            Self::ByKind(_) => {
                // Kind-based matching requires the inventory to resolve agent
                // kinds. This simplified matcher always returns false for ByKind;
                // the full inventory-based resolver handles it properly.
                false
            }
            Self::And(selectors) => selectors.iter().all(|s| s.matches(agent_name, groups)),
            Self::Or(selectors) => selectors.iter().any(|s| s.matches(agent_name, groups)),
            Self::Exclude { include, exclude } => {
                include.matches(agent_name, groups) && !exclude.matches(agent_name, groups)
            }
        }
    }
}

/// Simple glob matching supporting `*` as a wildcard for any sequence of characters.
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        // No wildcards — exact match.
        return pattern == text;
    }

    let mut remaining = text;

    // Check prefix.
    if let Some(&first) = parts.first() {
        if !first.is_empty() {
            if !remaining.starts_with(first) {
                return false;
            }
            remaining = &remaining[first.len()..];
        }
    }

    // Check suffix.
    if let Some(&last) = parts.last() {
        if !last.is_empty() {
            if !remaining.ends_with(last) {
                return false;
            }
            remaining = &remaining[..remaining.len() - last.len()];
        }
    }

    // Check middle parts.
    for &part in &parts[1..parts.len().saturating_sub(1)] {
        if part.is_empty() {
            continue;
        }
        match remaining.find(part) {
            Some(idx) => remaining = &remaining[idx + part.len()..],
            None => return false,
        }
    }

    true
}

// ---------------------------------------------------------------------------
// TaskDef
// ---------------------------------------------------------------------------

/// A single task definition within a play.
///
/// Tasks are the atomic unit of work. Each task invokes a module with
/// arguments, optionally gated by a condition and producing registered
/// output that subsequent tasks can reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDef {
    /// Human-readable name for logging and progress reporting.
    pub name: String,

    /// The module to invoke (e.g., `"config"`, `"capability"`, `"command"`).
    pub module: String,

    /// Arguments passed to the module.
    pub args: Value,

    /// Optional condition that must be true for the task to execute.
    /// If `None`, the task always runs.
    pub when: Option<Condition>,

    /// List of handler names to notify when this task reports a change.
    #[serde(default)]
    pub notify: Vec<String>,

    /// If set, the task's output is stored in the variable context under
    /// this name for subsequent tasks and conditions.
    pub register: Option<String>,

    /// Task-level variable overrides.
    #[serde(default)]
    pub vars: HashMap<String, Value>,

    /// Tags for selective execution (`--tags` / `--skip-tags`).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Number of times to retry on failure before giving up.
    #[serde(default)]
    pub retries: u32,

    /// Delay in seconds between retries.
    #[serde(default = "default_retry_delay")]
    pub retry_delay_secs: u64,

    /// If `true`, errors on this task are ignored and execution continues.
    #[serde(default)]
    pub ignore_errors: bool,
}

fn default_retry_delay() -> u64 {
    5
}

impl TaskDef {
    /// Create a minimal task definition.
    pub fn new(
        name: impl Into<String>,
        module: impl Into<String>,
        args: Value,
    ) -> Self {
        Self {
            name: name.into(),
            module: module.into(),
            args,
            when: None,
            notify: Vec::new(),
            register: None,
            vars: HashMap::new(),
            tags: Vec::new(),
            retries: 0,
            retry_delay_secs: 5,
            ignore_errors: false,
        }
    }

    /// Builder: add a condition.
    pub fn with_condition(mut self, condition: Condition) -> Self {
        self.when = Some(condition);
        self
    }

    /// Builder: add a handler notification.
    pub fn with_notify(mut self, handler: impl Into<String>) -> Self {
        self.notify.push(handler.into());
        self
    }

    /// Builder: register output under a variable name.
    pub fn with_register(mut self, name: impl Into<String>) -> Self {
        self.register = Some(name.into());
        self
    }

    /// Builder: add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Builder: set retry count.
    pub fn with_retries(mut self, retries: u32, delay_secs: u64) -> Self {
        self.retries = retries;
        self.retry_delay_secs = delay_secs;
        self
    }

    /// Builder: ignore errors.
    pub fn with_ignore_errors(mut self) -> Self {
        self.ignore_errors = true;
        self
    }
}

// ---------------------------------------------------------------------------
// RoleRef
// ---------------------------------------------------------------------------

/// A reference to a role to include in a play.
///
/// Roles are resolved from the role loader at play execution time.
/// The `vars` field allows overriding role defaults per-inclusion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleRef {
    /// The role name (looked up by the role loader).
    pub role: String,

    /// Variable overrides for this role inclusion.
    #[serde(default)]
    pub vars: HashMap<String, Value>,

    /// Optional condition for role inclusion.
    pub when: Option<Condition>,

    /// Tags applied to all tasks within this role.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl RoleRef {
    /// Create a simple role reference.
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            vars: HashMap::new(),
            when: None,
            tags: Vec::new(),
        }
    }

    /// Builder: add a variable override.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }

    /// Builder: add a condition.
    pub fn with_condition(mut self, condition: Condition) -> Self {
        self.when = Some(condition);
        self
    }
}

/// Type alias for backward compatibility — `PlayRole` is the same as [`RoleRef`].
pub type PlayRole = RoleRef;

// ---------------------------------------------------------------------------
// Play
// ---------------------------------------------------------------------------

/// A single play within a playbook.
///
/// A play targets a set of agents (via the selector) and applies an ordered
/// sequence of tasks and/or roles. Handlers are collected from both the
/// play definition and included roles, then flushed at the end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Play {
    /// Human-readable name for the play.
    pub name: String,

    /// Which agents this play targets.
    pub agents: AgentSelector,

    /// Play-level variables.
    #[serde(default)]
    pub vars: HashMap<String, Value>,

    /// Roles to apply before the task list.
    #[serde(default)]
    pub roles: Vec<RoleRef>,

    /// Ordered list of tasks to execute.
    #[serde(default)]
    pub tasks: Vec<TaskDef>,

    /// Handlers that tasks can notify.
    #[serde(default)]
    pub handlers: Vec<Handler>,

    /// If `true`, stop the entire playbook on the first task failure
    /// within this play (default: `true`).
    #[serde(default = "default_true")]
    pub any_errors_fatal: bool,

    /// Maximum number of agents to process in parallel (0 = unlimited).
    #[serde(default)]
    pub serial: u32,

    /// Tags for selective play execution.
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Play {
    /// Create a new play with the given name and default `All` selector.
    ///
    /// Use [`with_target`](Self::with_target) to narrow the agent selection.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            agents: AgentSelector::All,
            vars: HashMap::new(),
            roles: Vec::new(),
            tasks: Vec::new(),
            handlers: Vec::new(),
            any_errors_fatal: true,
            serial: 0,
            tags: Vec::new(),
        }
    }

    /// Create a new play with both a name and an explicit agent selector.
    pub fn targeted(name: impl Into<String>, agents: AgentSelector) -> Self {
        Self {
            name: name.into(),
            agents,
            vars: HashMap::new(),
            roles: Vec::new(),
            tasks: Vec::new(),
            handlers: Vec::new(),
            any_errors_fatal: true,
            serial: 0,
            tags: Vec::new(),
        }
    }

    /// Builder: set the agent selector for this play.
    pub fn with_target(mut self, selector: AgentSelector) -> Self {
        self.agents = selector;
        self
    }

    /// Add a task to the play.
    pub fn with_task(mut self, task: TaskDef) -> Self {
        self.tasks.push(task);
        self
    }

    /// Add a role to the play.
    pub fn with_role(mut self, role: RoleRef) -> Self {
        self.roles.push(role);
        self
    }

    /// Add a handler to the play.
    pub fn with_handler(mut self, handler: Handler) -> Self {
        self.handlers.push(handler);
        self
    }

    /// Add a play-level variable.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }
}

// ---------------------------------------------------------------------------
// Playbook
// ---------------------------------------------------------------------------

/// A complete playbook: one or more plays to execute in order.
///
/// The playbook is the top-level configuration artifact. The engine reads
/// a playbook, iterates over its plays, resolves agents for each play,
/// and executes tasks/roles against those agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playbook {
    /// Human-readable name for the playbook.
    pub name: String,

    /// Optional description.
    #[serde(default)]
    pub description: String,

    /// Playbook-level variables (lowest precedence after global).
    #[serde(default)]
    pub vars: HashMap<String, Value>,

    /// The ordered list of plays.
    pub plays: Vec<Play>,
}

impl Playbook {
    /// Create a new empty playbook.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            vars: HashMap::new(),
            plays: Vec::new(),
        }
    }

    /// Add a play to the playbook.
    pub fn with_play(mut self, play: Play) -> Self {
        self.plays.push(play);
        self
    }

    /// Add a playbook-level variable.
    pub fn with_var(mut self, key: impl Into<String>, value: Value) -> Self {
        self.vars.insert(key.into(), value);
        self
    }

    /// Count the number of plays in this playbook.
    pub fn play_count(&self) -> usize {
        self.plays.len()
    }

    /// Count total tasks across all plays (excluding role tasks).
    pub fn task_count(&self) -> usize {
        self.plays.iter().map(|p| p.tasks.len()).sum()
    }

    /// Count total roles across all plays.
    pub fn role_count(&self) -> usize {
        self.plays.iter().map(|p| p.roles.len()).sum()
    }

    /// Validate the playbook structure.
    ///
    /// Checks for empty plays, tasks without modules, and other structural
    /// issues. Returns a list of warnings/errors (empty = valid).
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.plays.is_empty() {
            issues.push("playbook has no plays".into());
        }

        for (i, play) in self.plays.iter().enumerate() {
            if play.tasks.is_empty() && play.roles.is_empty() {
                issues.push(format!(
                    "play[{i}] '{}' has no tasks and no roles",
                    play.name
                ));
            }

            for (j, task) in play.tasks.iter().enumerate() {
                if task.module.is_empty() {
                    issues.push(format!(
                        "play[{i}] '{}' task[{j}] '{}' has no module",
                        play.name, task.name
                    ));
                }
            }

            // Check that notified handlers exist.
            let handler_names: std::collections::HashSet<&str> = play
                .handlers
                .iter()
                .flat_map(|h| {
                    std::iter::once(h.name.as_str())
                        .chain(h.listen.iter().map(|l| l.as_str()))
                })
                .collect();

            for task in &play.tasks {
                for notify in &task.notify {
                    if !handler_names.contains(notify.as_str()) {
                        issues.push(format!(
                            "play[{i}] '{}' task '{}' notifies unknown handler '{notify}'",
                            play.name, task.name
                        ));
                    }
                }
            }
        }

        issues
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_selector_all() {
        let sel = AgentSelector::All;
        assert!(sel.matches("anything", &[]));
    }

    #[test]
    fn agent_selector_group() {
        let sel = AgentSelector::Group("workers".into());
        assert!(sel.matches("agent-1", &["workers".into(), "gpu".into()]));
        assert!(!sel.matches("agent-2", &["monitors".into()]));
    }

    #[test]
    fn agent_selector_pattern() {
        let sel = AgentSelector::Pattern("research-*".into());
        assert!(sel.matches("research-alpha", &[]));
        assert!(!sel.matches("build-beta", &[]));
    }

    #[test]
    fn agent_selector_exact() {
        let sel = AgentSelector::Agent("leader".into());
        assert!(sel.matches("leader", &[]));
        assert!(!sel.matches("follower", &[]));
    }

    #[test]
    fn agent_selector_and() {
        let sel = AgentSelector::And(vec![
            AgentSelector::Group("gpu".into()),
            AgentSelector::Pattern("research-*".into()),
        ]);
        assert!(sel.matches("research-1", &["gpu".into()]));
        assert!(!sel.matches("research-1", &["cpu".into()]));
        assert!(!sel.matches("build-1", &["gpu".into()]));
    }

    #[test]
    fn agent_selector_or() {
        let sel = AgentSelector::Or(vec![
            AgentSelector::Agent("leader".into()),
            AgentSelector::Group("monitors".into()),
        ]);
        assert!(sel.matches("leader", &[]));
        assert!(sel.matches("mon-1", &["monitors".into()]));
        assert!(!sel.matches("worker-1", &["workers".into()]));
    }

    #[test]
    fn agent_selector_exclude() {
        let sel = AgentSelector::Exclude {
            include: Box::new(AgentSelector::Group("all".into())),
            exclude: Box::new(AgentSelector::Agent("broken".into())),
        };
        assert!(sel.matches("healthy", &["all".into()]));
        assert!(!sel.matches("broken", &["all".into()]));
    }

    #[test]
    fn task_def_builder() {
        let task = TaskDef::new("install deps", "command", json!({"cmd": "apt install"}))
            .with_condition(Condition::Equals("os".into(), json!("linux")))
            .with_notify("restart_agent")
            .with_register("install_result")
            .with_tag("setup")
            .with_retries(3, 10);

        assert_eq!(task.name, "install deps");
        assert_eq!(task.module, "command");
        assert!(task.when.is_some());
        assert_eq!(task.notify, vec!["restart_agent"]);
        assert_eq!(task.register.as_deref(), Some("install_result"));
        assert_eq!(task.tags, vec!["setup"]);
        assert_eq!(task.retries, 3);
    }

    #[test]
    fn play_builder() {
        let play = Play::new("configure workers").with_target(AgentSelector::Group("workers".into()))
            .with_task(TaskDef::new("set config", "config", json!({"key": "val"})))
            .with_handler(Handler::new("restart", "command", json!({"cmd": "restart"})))
            .with_var("env", json!("production"));

        assert_eq!(play.name, "configure workers");
        assert_eq!(play.tasks.len(), 1);
        assert_eq!(play.handlers.len(), 1);
        assert_eq!(play.vars["env"], "production");
    }

    #[test]
    fn playbook_builder() {
        let playbook = Playbook::new("deploy-v2")
            .with_play(
                Play::new("setup")
                    .with_task(TaskDef::new("init", "config", json!({}))),
            )
            .with_play(
                Play::new("configure").with_target(AgentSelector::Group("workers".into()))
                    .with_role(RoleRef::new("security-baseline")),
            )
            .with_var("version", json!("2.0"));

        assert_eq!(playbook.plays.len(), 2);
        assert_eq!(playbook.task_count(), 1);
        assert_eq!(playbook.role_count(), 1);
    }

    #[test]
    fn playbook_validate_valid() {
        let playbook = Playbook::new("valid")
            .with_play(
                Play::new("p1")
                    .with_task(TaskDef::new("t1", "config", json!({}))
                        .with_notify("h1"))
                    .with_handler(Handler::new("h1", "command", json!({}))),
            );

        let issues = playbook.validate();
        assert!(issues.is_empty(), "expected no issues, got: {issues:?}");
    }

    #[test]
    fn playbook_validate_empty_plays() {
        let playbook = Playbook::new("empty");
        let issues = playbook.validate();
        assert!(issues.iter().any(|i| i.contains("no plays")));
    }

    #[test]
    fn playbook_validate_empty_task_list() {
        let playbook = Playbook::new("no-tasks")
            .with_play(Play::new("empty-play"));

        let issues = playbook.validate();
        assert!(issues.iter().any(|i| i.contains("no tasks and no roles")));
    }

    #[test]
    fn playbook_validate_missing_handler() {
        let playbook = Playbook::new("bad-notify")
            .with_play(
                Play::new("p1")
                    .with_task(TaskDef::new("t1", "config", json!({}))
                        .with_notify("nonexistent")),
            );

        let issues = playbook.validate();
        assert!(issues.iter().any(|i| i.contains("unknown handler")));
    }

    #[test]
    fn role_ref_builder() {
        let role = RoleRef::new("security-baseline")
            .with_var("level", json!("strict"))
            .with_condition(Condition::Equals("env".into(), json!("production")));

        assert_eq!(role.role, "security-baseline");
        assert_eq!(role.vars["level"], "strict");
        assert!(role.when.is_some());
    }

    #[test]
    fn playbook_serde_roundtrip() {
        let playbook = Playbook::new("serde-test")
            .with_play(
                Play::new("p1")
                    .with_task(TaskDef::new("t1", "config", json!({"k": "v"}))),
            );

        let json = serde_json::to_string(&playbook).unwrap();
        let back: Playbook = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "serde-test");
        assert_eq!(back.plays.len(), 1);
        assert_eq!(back.plays[0].tasks.len(), 1);
    }

    #[test]
    fn agent_selector_serde_roundtrip() {
        let sel = AgentSelector::Exclude {
            include: Box::new(AgentSelector::Group("all".into())),
            exclude: Box::new(AgentSelector::Agent("broken".into())),
        };
        let json = serde_json::to_string(&sel).unwrap();
        let back: AgentSelector = serde_json::from_str(&json).unwrap();
        assert_eq!(sel, back);
    }

    #[test]
    fn glob_match_star() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("research-*", "research-alpha"));
        assert!(glob_match("*-pool", "agent-pool"));
        assert!(glob_match("a*b*c", "axbxc"));
        assert!(!glob_match("research-*", "build-alpha"));
    }

    #[test]
    fn glob_match_no_wildcard() {
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "other"));
    }

    #[test]
    fn task_ignore_errors() {
        let task = TaskDef::new("risky", "command", json!({}))
            .with_ignore_errors();
        assert!(task.ignore_errors);
    }
}
