//! # DAF Configure
//!
//! Ansible-inspired configuration management for agents. Instead of
//! configuring servers, you configure agent behaviors, roles, and task
//! playbooks. Idempotent operations, handler chains, and role-based
//! organization.
//!
//! # Core concepts
//!
//! - **Playbooks** — declarative YAML/JSON documents that describe a
//!   sequence of plays, each targeting a set of agents.
//! - **Plays** — a play selects agents and executes tasks against them,
//!   with optional pre/post hooks and handlers.
//! - **Tasks** — atomic configuration steps that invoke a module.
//! - **Modules** — idempotent operations (config, capability, channel,
//!   memory, health, command, template).
//! - **Handlers** — deferred actions triggered by task notifications,
//!   executed once at the end of a play.
//! - **Roles** — reusable bundles of tasks, handlers, and variables.
//! - **Inventory** — catalog of agents available for configuration.
//! - **Variables** — layered resolution with template rendering.
//! - **Conditions** — boolean predicates for conditional execution.
//!
//! # Quick start
//!
//! ```rust
//! use daf_configure::playbook::{Playbook, Play, TaskDef, AgentSelector};
//! use daf_configure::handler::Handler;
//! use serde_json::json;
//!
//! let playbook = Playbook::new("Setup research team")
//!     .with_play(
//!         Play::new("Configure researchers").with_target(AgentSelector::Group("specialists".into()))
//!             .with_task(TaskDef::new(
//!                 "Add analysis capability",
//!                 "capability",
//!                 json!({"add": [{"name": "analysis", "version": "1.0", "description": "Deep analysis"}]}),
//!             ).with_notify("reload"))
//!             .with_handler(Handler::new("reload", "command", json!({"cmd": "reload"}))),
//!     );
//!
//! assert_eq!(playbook.play_count(), 1);
//! assert_eq!(playbook.task_count(), 1);
//! ```

pub mod condition;
pub mod handler;
pub mod inventory;
pub mod module;
pub mod playbook;
pub mod role;
pub mod vars;

// Re-export the most commonly used types at crate root.
pub use condition::Condition;
pub use handler::{Handler, HandlerChain};
pub use inventory::{AgentEntry, Group, Inventory, InventoryLoader};
pub use module::{Module, ModuleContext, ModuleError, ModuleRegistry, ModuleResult};
pub use playbook::{AgentSelector, Play, Playbook, RoleRef, TaskDef};
pub use role::{resolve_dependency_order, Role, RoleDependency, RoleLoader};
pub use vars::{VarManager, VarScope};
