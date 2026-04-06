//! Ansible-style agent configuration with playbooks
//!
//! This example demonstrates DAF's configuration management layer — the
//! same pattern Ansible uses for server configuration, applied to agents.
//! You define playbooks with tasks, roles, handlers, and variables, then
//! run them against an inventory of agents.
//!
//! Key concepts demonstrated:
//! - Playbook definition with ordered tasks
//! - Inventory of target agents with host variables
//! - Roles that group related tasks
//! - Handlers triggered by task notifications
//! - Variable interpolation in task parameters
//! - Idempotent task execution with change detection
//!
//! Run with:
//!   cargo run -p daf-example-configuration

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Inventory — the agents we are configuring
// ---------------------------------------------------------------------------

/// An inventory entry representing a target agent or group of agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InventoryHost {
    /// Agent name or identifier.
    name: String,
    /// Agent kind.
    kind: String,
    /// Host-specific variables (override group/global vars).
    vars: HashMap<String, String>,
    /// Groups this host belongs to.
    groups: Vec<String>,
}

/// The full inventory of agents available for configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Inventory {
    hosts: Vec<InventoryHost>,
    /// Group-level variables.
    group_vars: HashMap<String, HashMap<String, String>>,
    /// Global variables (lowest precedence).
    global_vars: HashMap<String, String>,
}

impl Inventory {
    /// Resolve all variables for a specific host, with proper precedence:
    /// host vars > group vars > global vars.
    fn resolve_vars(&self, host: &InventoryHost) -> HashMap<String, String> {
        let mut vars = self.global_vars.clone();

        // Apply group vars (in order of groups listed)
        for group in &host.groups {
            if let Some(group_vars) = self.group_vars.get(group) {
                vars.extend(group_vars.clone());
            }
        }

        // Apply host vars (highest precedence)
        vars.extend(host.vars.clone());

        vars
    }

    /// Select hosts matching a pattern (simple glob for this example).
    fn select(&self, pattern: &str) -> Vec<&InventoryHost> {
        if pattern == "all" {
            return self.hosts.iter().collect();
        }

        self.hosts
            .iter()
            .filter(|h| {
                h.name == pattern
                    || h.groups.iter().any(|g| g == pattern)
                    || h.kind == pattern
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Task — a single unit of configuration work
// ---------------------------------------------------------------------------

/// The result of executing a task on a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskResult {
    /// Task ran and made changes.
    Changed,
    /// Task ran but no changes were needed (already in desired state).
    Ok,
    /// Task failed.
    Failed,
    /// Task was skipped (condition not met).
    Skipped,
}

impl fmt::Display for TaskResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed => write!(f, "changed"),
            Self::Ok => write!(f, "ok"),
            Self::Failed => write!(f, "FAILED"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

/// A single configuration task within a playbook.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    /// Human-readable task name.
    name: String,
    /// The module to run (e.g., "capability", "resource_limit", "metadata").
    module: String,
    /// Module parameters.
    params: HashMap<String, String>,
    /// Condition for execution (variable name that must be "true").
    when: Option<String>,
    /// Handlers to notify on change.
    notify: Vec<String>,
    /// Whether this task should be run in check mode (dry-run).
    check_mode: bool,
}

/// A handler that runs when notified by a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Handler {
    /// Handler name (must match the string in Task::notify).
    name: String,
    /// Module to run.
    module: String,
    /// Module parameters.
    params: HashMap<String, String>,
}

/// A role groups related tasks and handlers under a reusable name.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Role {
    /// Role name.
    name: String,
    /// Tasks that belong to this role, executed in order.
    tasks: Vec<Task>,
    /// Handlers defined by this role.
    handlers: Vec<Handler>,
    /// Default variables for this role (can be overridden by playbook vars).
    defaults: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Playbook — the top-level configuration document
// ---------------------------------------------------------------------------

/// A complete configuration playbook that runs against an inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Playbook {
    /// Playbook name.
    name: String,
    /// Description of what this playbook does.
    description: String,
    /// Host pattern selecting which inventory hosts to configure.
    hosts: String,
    /// Variables set at the playbook level.
    vars: HashMap<String, String>,
    /// Roles to apply (in order).
    roles: Vec<String>,
    /// Inline tasks (run after roles).
    tasks: Vec<Task>,
    /// Handlers available to all tasks in this playbook.
    handlers: Vec<Handler>,
}

// ---------------------------------------------------------------------------
// Executor — runs playbooks against inventory
// ---------------------------------------------------------------------------

struct PlaybookExecutor {
    roles: HashMap<String, Role>,
}

impl PlaybookExecutor {
    fn new() -> Self {
        Self {
            roles: HashMap::new(),
        }
    }

    /// Register a role so it can be referenced by playbooks.
    fn register_role(&mut self, role: Role) {
        self.roles.insert(role.name.clone(), role);
    }

    /// Interpolate variables in a string. Variables use {{ var_name }} syntax.
    fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
        let mut result = template.to_string();
        for (key, value) in vars {
            let pattern = format!("{{{{ {key} }}}}");
            result = result.replace(&pattern, value);
        }
        result
    }

    /// Execute a single task against a host, returning the result.
    fn execute_task(
        task: &Task,
        host: &InventoryHost,
        vars: &HashMap<String, String>,
    ) -> TaskResult {
        // Check the when condition
        if let Some(condition) = &task.when {
            let resolved = Self::interpolate(condition, vars);
            if resolved != "true" {
                info!(
                    task = task.name,
                    host = host.name,
                    condition = condition,
                    "Skipping (condition not met)"
                );
                return TaskResult::Skipped;
            }
        }

        // Interpolate all parameters
        let resolved_params: HashMap<String, String> = task
            .params
            .iter()
            .map(|(k, v)| (k.clone(), Self::interpolate(v, vars)))
            .collect();

        info!(
            task = task.name,
            host = host.name,
            module = task.module,
            params = ?resolved_params,
            "Executing task"
        );

        // Simulate module execution. In production, each module would be
        // a real implementation that checks current state and applies changes.
        match task.module.as_str() {
            "capability" => {
                let cap_name = resolved_params.get("name").map(|s| s.as_str()).unwrap_or("unknown");
                let cap_version = resolved_params.get("version").map(|s| s.as_str()).unwrap_or("1.0.0");
                let state = resolved_params.get("state").map(|s| s.as_str()).unwrap_or("present");

                info!(
                    capability = cap_name,
                    version = cap_version,
                    state,
                    "Configuring capability"
                );

                if state == "present" {
                    TaskResult::Changed
                } else {
                    TaskResult::Ok
                }
            }
            "resource_limit" => {
                let resource = resolved_params.get("resource").map(|s| s.as_str()).unwrap_or("memory");
                let limit = resolved_params.get("limit").map(|s| s.as_str()).unwrap_or("256");

                info!(
                    resource,
                    limit,
                    "Setting resource limit"
                );
                TaskResult::Changed
            }
            "metadata" => {
                let key = resolved_params.get("key").map(|s| s.as_str()).unwrap_or("");
                let value = resolved_params.get("value").map(|s| s.as_str()).unwrap_or("");

                info!(key, value, "Setting metadata");
                TaskResult::Changed
            }
            "health_check" => {
                let endpoint = resolved_params.get("endpoint").map(|s| s.as_str()).unwrap_or("/health");
                let interval = resolved_params.get("interval").map(|s| s.as_str()).unwrap_or("30s");

                info!(endpoint, interval, "Configuring health check");
                TaskResult::Changed
            }
            "restart" => {
                info!(host = host.name, "Restarting agent");
                TaskResult::Changed
            }
            "validate" => {
                info!(host = host.name, "Validating agent configuration");
                TaskResult::Ok
            }
            unknown => {
                warn!(module = unknown, "Unknown module");
                TaskResult::Failed
            }
        }
    }

    /// Run a playbook against the inventory.
    fn run(
        &self,
        playbook: &Playbook,
        inventory: &Inventory,
    ) -> PlaybookResult {
        info!(
            playbook = playbook.name,
            hosts = playbook.hosts,
            "Running playbook"
        );

        let target_hosts = inventory.select(&playbook.hosts);
        if target_hosts.is_empty() {
            warn!(pattern = playbook.hosts, "No hosts matched");
            return PlaybookResult::empty();
        }

        info!(count = target_hosts.len(), "Matched hosts");

        let mut result = PlaybookResult::empty();

        for host in &target_hosts {
            info!(host = host.name, "--- Configuring host ---");

            // Resolve variables with full precedence chain
            let mut vars = inventory.resolve_vars(host);
            vars.extend(playbook.vars.clone());

            // Collect tasks from roles first, then inline tasks
            let mut all_tasks: Vec<Task> = Vec::new();
            let mut all_handlers: Vec<Handler> = playbook.handlers.clone();

            for role_name in &playbook.roles {
                if let Some(role) = self.roles.get(role_name) {
                    // Merge role defaults (lowest precedence for role vars)
                    for (k, v) in &role.defaults {
                        vars.entry(k.clone()).or_insert(v.clone());
                    }
                    all_tasks.extend(role.tasks.clone());
                    all_handlers.extend(role.handlers.clone());
                } else {
                    warn!(role = role_name, "Role not found, skipping");
                }
            }

            all_tasks.extend(playbook.tasks.clone());

            // Execute tasks in order
            let mut pending_handlers: Vec<String> = Vec::new();

            for task in &all_tasks {
                let task_result = Self::execute_task(task, host, &vars);

                info!(
                    task = task.name,
                    host = host.name,
                    result = %task_result,
                    "Task result"
                );

                match task_result {
                    TaskResult::Changed => {
                        result.changed += 1;
                        // Collect handler notifications
                        for handler_name in &task.notify {
                            if !pending_handlers.contains(handler_name) {
                                pending_handlers.push(handler_name.clone());
                            }
                        }
                    }
                    TaskResult::Ok => result.ok += 1,
                    TaskResult::Failed => result.failed += 1,
                    TaskResult::Skipped => result.skipped += 1,
                }
            }

            // Run pending handlers (each handler runs at most once, even if
            // notified by multiple tasks — same as Ansible behavior)
            if !pending_handlers.is_empty() {
                info!(
                    handlers = ?pending_handlers,
                    "Running notified handlers"
                );

                for handler_name in &pending_handlers {
                    if let Some(handler) = all_handlers.iter().find(|h| &h.name == handler_name) {
                        let handler_task = Task {
                            name: format!("handler: {}", handler.name),
                            module: handler.module.clone(),
                            params: handler.params.clone(),
                            when: None,
                            notify: Vec::new(),
                            check_mode: false,
                        };

                        let handler_result = Self::execute_task(&handler_task, host, &vars);
                        info!(
                            handler = handler.name,
                            result = %handler_result,
                            "Handler result"
                        );
                    } else {
                        warn!(handler = handler_name, "Handler not found");
                    }
                }
            }
        }

        result
    }
}

/// Summary of a playbook run.
#[derive(Debug)]
struct PlaybookResult {
    ok: usize,
    changed: usize,
    failed: usize,
    skipped: usize,
}

impl PlaybookResult {
    fn empty() -> Self {
        Self {
            ok: 0,
            changed: 0,
            failed: 0,
            skipped: 0,
        }
    }
}

impl fmt::Display for PlaybookResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ok={ok}    changed={changed}    failed={failed}    skipped={skipped}",
            ok = self.ok,
            changed = self.changed,
            failed = self.failed,
            skipped = self.skipped,
        )
    }
}

// ---------------------------------------------------------------------------
// main — define inventory, playbook, and run
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("=== DAF Configuration Management Example ===");

    // -----------------------------------------------------------------------
    // 1. Define the inventory of agents to configure.
    // -----------------------------------------------------------------------

    let inventory = Inventory {
        hosts: vec![
            InventoryHost {
                name: "researcher-01".into(),
                kind: "specialist".into(),
                vars: HashMap::from([("max_memory_mb".into(), "512".into())]),
                groups: vec!["researchers".into(), "production".into()],
            },
            InventoryHost {
                name: "researcher-02".into(),
                kind: "specialist".into(),
                vars: HashMap::new(),
                groups: vec!["researchers".into(), "production".into()],
            },
            InventoryHost {
                name: "analyzer-01".into(),
                kind: "specialist".into(),
                vars: HashMap::from([("enable_gpu".into(), "true".into())]),
                groups: vec!["analyzers".into(), "production".into()],
            },
            InventoryHost {
                name: "monitor-01".into(),
                kind: "monitor".into(),
                vars: HashMap::new(),
                groups: vec!["monitors".into(), "production".into()],
            },
        ],
        group_vars: HashMap::from([
            (
                "researchers".into(),
                HashMap::from([
                    ("max_memory_mb".into(), "256".into()),
                    ("health_interval".into(), "30s".into()),
                ]),
            ),
            (
                "analyzers".into(),
                HashMap::from([
                    ("max_memory_mb".into(), "1024".into()),
                    ("health_interval".into(), "15s".into()),
                ]),
            ),
            (
                "production".into(),
                HashMap::from([
                    ("log_level".into(), "info".into()),
                    ("metrics_enabled".into(), "true".into()),
                ]),
            ),
        ]),
        global_vars: HashMap::from([
            ("daf_version".into(), "0.1.0".into()),
            ("cluster_name".into(), "research-cluster".into()),
            ("max_memory_mb".into(), "128".into()),
            ("health_interval".into(), "60s".into()),
            ("enable_gpu".into(), "false".into()),
        ]),
    };

    info!(hosts = inventory.hosts.len(), "Inventory loaded");

    // -----------------------------------------------------------------------
    // 2. Define roles — reusable configuration bundles.
    // -----------------------------------------------------------------------

    let base_role = Role {
        name: "base".into(),
        tasks: vec![
            Task {
                name: "Set cluster metadata".into(),
                module: "metadata".into(),
                params: HashMap::from([
                    ("key".into(), "cluster".into()),
                    ("value".into(), "{{ cluster_name }}".into()),
                ]),
                when: None,
                notify: Vec::new(),
                check_mode: false,
            },
            Task {
                name: "Set DAF version metadata".into(),
                module: "metadata".into(),
                params: HashMap::from([
                    ("key".into(), "daf_version".into()),
                    ("value".into(), "{{ daf_version }}".into()),
                ]),
                when: None,
                notify: Vec::new(),
                check_mode: false,
            },
            Task {
                name: "Configure memory limit".into(),
                module: "resource_limit".into(),
                params: HashMap::from([
                    ("resource".into(), "memory".into()),
                    ("limit".into(), "{{ max_memory_mb }}MB".into()),
                ]),
                when: None,
                notify: vec!["restart agent".into()],
                check_mode: false,
            },
            Task {
                name: "Configure health check".into(),
                module: "health_check".into(),
                params: HashMap::from([
                    ("endpoint".into(), "/health".into()),
                    ("interval".into(), "{{ health_interval }}".into()),
                ]),
                when: None,
                notify: Vec::new(),
                check_mode: false,
            },
        ],
        handlers: vec![Handler {
            name: "restart agent".into(),
            module: "restart".into(),
            params: HashMap::new(),
        }],
        defaults: HashMap::from([
            ("max_memory_mb".into(), "128".into()),
            ("health_interval".into(), "60s".into()),
        ]),
    };

    let gpu_role = Role {
        name: "gpu-accelerated".into(),
        tasks: vec![
            Task {
                name: "Enable GPU acceleration".into(),
                module: "capability".into(),
                params: HashMap::from([
                    ("name".into(), "gpu_inference".into()),
                    ("version".into(), "1.0.0".into()),
                    ("state".into(), "present".into()),
                ]),
                when: Some("{{ enable_gpu }}".into()),
                notify: vec!["restart agent".into()],
                check_mode: false,
            },
            Task {
                name: "Set GPU memory limit".into(),
                module: "resource_limit".into(),
                params: HashMap::from([
                    ("resource".into(), "gpu_memory".into()),
                    ("limit".into(), "4096MB".into()),
                ]),
                when: Some("{{ enable_gpu }}".into()),
                notify: Vec::new(),
                check_mode: false,
            },
        ],
        handlers: vec![Handler {
            name: "restart agent".into(),
            module: "restart".into(),
            params: HashMap::new(),
        }],
        defaults: HashMap::from([("enable_gpu".into(), "false".into())]),
    };

    // -----------------------------------------------------------------------
    // 3. Define the playbook.
    // -----------------------------------------------------------------------

    let playbook = Playbook {
        name: "Configure research pipeline agents".into(),
        description: "Sets up base configuration, resource limits, health checks, \
                       and optional GPU acceleration for all production agents."
            .into(),
        hosts: "production".into(),
        vars: HashMap::from([("deployment_id".into(), "deploy-2026-04-07".into())]),
        roles: vec!["base".into(), "gpu-accelerated".into()],
        tasks: vec![
            Task {
                name: "Tag with deployment ID".into(),
                module: "metadata".into(),
                params: HashMap::from([
                    ("key".into(), "deployment_id".into()),
                    ("value".into(), "{{ deployment_id }}".into()),
                ]),
                when: None,
                notify: Vec::new(),
                check_mode: false,
            },
            Task {
                name: "Validate final configuration".into(),
                module: "validate".into(),
                params: HashMap::new(),
                when: None,
                notify: Vec::new(),
                check_mode: false,
            },
        ],
        handlers: vec![Handler {
            name: "restart agent".into(),
            module: "restart".into(),
            params: HashMap::new(),
        }],
    };

    info!(
        playbook = playbook.name,
        hosts = playbook.hosts,
        roles = ?playbook.roles,
        tasks = playbook.tasks.len(),
        "Playbook loaded"
    );

    // -----------------------------------------------------------------------
    // 4. Register roles and run the playbook.
    // -----------------------------------------------------------------------

    let mut executor = PlaybookExecutor::new();
    executor.register_role(base_role);
    executor.register_role(gpu_role);

    info!("\n--- Playbook Execution ---\n");

    let result = executor.run(&playbook, &inventory);

    println!("\n============================");
    println!("PLAY RECAP");
    println!("============================");
    println!("{result}");
    println!("============================\n");

    // -----------------------------------------------------------------------
    // 5. Show variable resolution for a specific host (debugging aid).
    // -----------------------------------------------------------------------

    info!("--- Variable Resolution Demo ---");

    for host in &inventory.hosts {
        let vars = inventory.resolve_vars(host);
        info!(
            host = host.name,
            max_memory_mb = vars.get("max_memory_mb").map(|s| s.as_str()).unwrap_or("?"),
            health_interval = vars.get("health_interval").map(|s| s.as_str()).unwrap_or("?"),
            enable_gpu = vars.get("enable_gpu").map(|s| s.as_str()).unwrap_or("?"),
            "Resolved variables"
        );
    }

    info!("=== Configuration management example complete ===");

    Ok(())
}
