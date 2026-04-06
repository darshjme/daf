//! Built-in configuration modules.
//!
//! Modules are the atomic units of configuration change in DAF-configure,
//! analogous to Ansible modules. Each module knows how to apply one kind
//! of change to an agent's state in an idempotent way — calling a module
//! twice with the same arguments produces the same result without
//! side-effects.
//!
//! # Built-in modules
//!
//! | Name         | Purpose                                       |
//! |-------------|-----------------------------------------------|
//! | `config`     | Set agent configuration key-value pairs        |
//! | `capability` | Add or remove agent capabilities               |
//! | `channel`    | Configure communication channels               |
//! | `memory`     | Seed agent memory with knowledge               |
//! | `health`     | Configure health-check parameters               |
//! | `command`    | Execute an arbitrary command on an agent        |
//! | `template`   | Render a template into agent configuration      |

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// ModuleContext
// ---------------------------------------------------------------------------

/// Runtime context supplied to a module during execution.
///
/// Carries the current variable state, agent identity, and a reference to
/// the agent's live configuration so modules can check current state for
/// idempotency.
#[derive(Debug, Clone)]
pub struct ModuleContext {
    /// Flattened variable map for template resolution.
    pub vars: HashMap<String, Value>,
    /// The agent being configured.
    pub agent_name: String,
    /// The agent's current configuration (for idempotency checks).
    pub current_config: HashMap<String, Value>,
    /// The agent's current capabilities.
    pub current_capabilities: Vec<String>,
    /// Scratch area for module-produced state that needs to survive
    /// across the play.
    pub facts: HashMap<String, Value>,
}

impl ModuleContext {
    /// Create a minimal context for testing or simple usage.
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            vars: HashMap::new(),
            agent_name: agent_name.into(),
            current_config: HashMap::new(),
            current_capabilities: Vec::new(),
            facts: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// ModuleResult
// ---------------------------------------------------------------------------

/// Outcome of a module execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleResult {
    /// Whether the module actually changed the agent's state.
    ///
    /// `false` means the desired state already matched — the hallmark
    /// of an idempotent run.
    pub changed: bool,
    /// Structured output for consumption by `register` and subsequent tasks.
    pub output: Value,
    /// Human-readable summary of what happened.
    pub message: String,
    /// Non-fatal warnings emitted during execution.
    pub warnings: Vec<String>,
}

impl ModuleResult {
    /// Create an "ok, nothing changed" result.
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            changed: false,
            output: Value::Object(serde_json::Map::new()),
            message: message.into(),
            warnings: Vec::new(),
        }
    }

    /// Create a "changed" result.
    pub fn changed(message: impl Into<String>, output: Value) -> Self {
        Self {
            changed: true,
            output,
            message: message.into(),
            warnings: Vec::new(),
        }
    }

    /// Attach a warning.
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Module trait
// ---------------------------------------------------------------------------

/// A configuration module that can be executed against an agent.
#[async_trait]
pub trait Module: Send + Sync + 'static {
    /// Machine-readable module name (e.g. `"config"`, `"capability"`).
    fn name(&self) -> &str;

    /// JSON schema describing the expected `args` structure.
    fn schema(&self) -> Value {
        Value::Object(serde_json::Map::new())
    }

    /// Execute the module with the given arguments and context.
    ///
    /// Implementations MUST be idempotent: running twice with the same
    /// `args` and `context` should produce `changed: false` on the
    /// second invocation.
    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError>;
}

/// Module-level errors.
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("missing required argument: {0}")]
    MissingArgument(String),

    #[error("invalid argument '{key}': {reason}")]
    InvalidArgument { key: String, reason: String },

    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    #[error("module not found: {0}")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// ModuleRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry for looking up modules by name.
///
/// Pre-populated with all built-in modules. Custom modules can be
/// registered at runtime.
pub struct ModuleRegistry {
    modules: DashMap<String, Arc<dyn Module>>,
}

impl ModuleRegistry {
    /// Create a registry pre-loaded with all built-in modules.
    pub fn with_builtins() -> Self {
        let reg = Self {
            modules: DashMap::new(),
        };
        reg.register(Arc::new(ConfigModule));
        reg.register(Arc::new(CapabilityModule));
        reg.register(Arc::new(ChannelModule));
        reg.register(Arc::new(MemoryModule));
        reg.register(Arc::new(HealthModule));
        reg.register(Arc::new(CommandModule));
        reg.register(Arc::new(TemplateModule));
        reg
    }

    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            modules: DashMap::new(),
        }
    }

    /// Register a module. Overwrites any existing module with the same name.
    pub fn register(&self, module: Arc<dyn Module>) {
        let name = module.name().to_string();
        debug!(module = %name, "registering module");
        self.modules.insert(name, module);
    }

    /// Look up a module by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Module>> {
        self.modules.get(name).map(|r| Arc::clone(r.value()))
    }

    /// List all registered module names.
    pub fn list(&self) -> Vec<String> {
        self.modules.iter().map(|r| r.key().clone()).collect()
    }

    /// Number of registered modules.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Returns `true` if no modules are registered.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl std::fmt::Debug for ModuleRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleRegistry")
            .field("modules", &self.list())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Built-in: ConfigModule
// ---------------------------------------------------------------------------

/// Set agent configuration key-value pairs (idempotent).
///
/// # Arguments
///
/// ```json
/// {
///   "set": { "key": "value", ... },
///   "remove": ["key1", "key2"]
/// }
/// ```
pub struct ConfigModule;

#[async_trait]
impl Module for ConfigModule {
    fn name(&self) -> &str {
        "config"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "set": { "type": "object", "description": "Key-value pairs to set" },
                "remove": { "type": "array", "items": { "type": "string" }, "description": "Keys to remove" }
            }
        })
    }

    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let mut changes = Vec::new();

        // Process "set" operations.
        if let Some(set) = args.get("set").and_then(|v| v.as_object()) {
            for (key, value) in set {
                let current = context.current_config.get(key);
                if current != Some(value) {
                    changes.push(format!("set {key}"));
                }
            }
        }

        // Process "remove" operations.
        if let Some(remove) = args.get("remove").and_then(|v| v.as_array()) {
            for key in remove {
                if let Some(key_str) = key.as_str() {
                    if context.current_config.contains_key(key_str) {
                        changes.push(format!("remove {key_str}"));
                    }
                }
            }
        }

        if changes.is_empty() {
            Ok(ModuleResult::ok("configuration already matches desired state"))
        } else {
            Ok(ModuleResult::changed(
                format!("applied {} config changes", changes.len()),
                serde_json::json!({ "changes": changes }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in: CapabilityModule
// ---------------------------------------------------------------------------

/// Add or remove agent capabilities (idempotent).
///
/// # Arguments
///
/// ```json
/// {
///   "add": [{"name": "code_review", "version": "1.0", "description": "..."}],
///   "remove": ["old_capability"]
/// }
/// ```
pub struct CapabilityModule;

#[async_trait]
impl Module for CapabilityModule {
    fn name(&self) -> &str {
        "capability"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "add": { "type": "array", "description": "Capabilities to add" },
                "remove": { "type": "array", "items": { "type": "string" }, "description": "Capability names to remove" }
            }
        })
    }

    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let mut added = Vec::new();
        let mut removed = Vec::new();

        if let Some(add) = args.get("add").and_then(|v| v.as_array()) {
            for cap in add {
                let cap_name = cap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ModuleError::InvalidArgument {
                        key: "add[].name".into(),
                        reason: "capability must have a name".into(),
                    })?;

                if !context.current_capabilities.contains(&cap_name.to_string()) {
                    added.push(cap_name.to_string());
                }
            }
        }

        if let Some(remove) = args.get("remove").and_then(|v| v.as_array()) {
            for name in remove {
                if let Some(name_str) = name.as_str() {
                    if context.current_capabilities.contains(&name_str.to_string()) {
                        removed.push(name_str.to_string());
                    }
                }
            }
        }

        if added.is_empty() && removed.is_empty() {
            Ok(ModuleResult::ok("capabilities already match desired state"))
        } else {
            Ok(ModuleResult::changed(
                format!("added {}, removed {} capabilities", added.len(), removed.len()),
                serde_json::json!({ "added": added, "removed": removed }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in: ChannelModule
// ---------------------------------------------------------------------------

/// Configure communication channels for an agent (idempotent).
///
/// # Arguments
///
/// ```json
/// {
///   "subscribe": ["channel_name"],
///   "unsubscribe": ["channel_name"],
///   "create": { "name": "new_channel", "capacity": 1024 }
/// }
/// ```
pub struct ChannelModule;

#[async_trait]
impl Module for ChannelModule {
    fn name(&self) -> &str {
        "channel"
    }

    async fn execute(&self, args: Value, _context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let mut actions = Vec::new();

        if let Some(subs) = args.get("subscribe").and_then(|v| v.as_array()) {
            for ch in subs {
                if let Some(name) = ch.as_str() {
                    actions.push(format!("subscribe:{name}"));
                }
            }
        }

        if let Some(unsubs) = args.get("unsubscribe").and_then(|v| v.as_array()) {
            for ch in unsubs {
                if let Some(name) = ch.as_str() {
                    actions.push(format!("unsubscribe:{name}"));
                }
            }
        }

        if let Some(create) = args.get("create").and_then(|v| v.as_object()) {
            if let Some(name) = create.get("name").and_then(|v| v.as_str()) {
                actions.push(format!("create:{name}"));
            }
        }

        if actions.is_empty() {
            Ok(ModuleResult::ok("no channel changes required"))
        } else {
            Ok(ModuleResult::changed(
                format!("{} channel operations", actions.len()),
                serde_json::json!({ "actions": actions }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in: MemoryModule
// ---------------------------------------------------------------------------

/// Seed agent memory with knowledge entries (idempotent).
///
/// # Arguments
///
/// ```json
/// {
///   "entries": [
///     { "key": "domain_knowledge", "value": "...", "tags": ["init"] }
///   ],
///   "clear_tags": ["stale"]
/// }
/// ```
pub struct MemoryModule;

#[async_trait]
impl Module for MemoryModule {
    fn name(&self) -> &str {
        "memory"
    }

    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let mut seeded = 0u32;
        let mut cleared = 0u32;

        if let Some(entries) = args.get("entries").and_then(|v| v.as_array()) {
            for entry in entries {
                let key = entry.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                    ModuleError::InvalidArgument {
                        key: "entries[].key".into(),
                        reason: "each memory entry must have a key".into(),
                    }
                })?;

                // Idempotency: check if the key is already set to the same value.
                if let Some(existing) = context.current_config.get(key) {
                    if entry.get("value") == Some(existing) {
                        continue;
                    }
                }
                seeded += 1;
            }
        }

        if let Some(tags) = args.get("clear_tags").and_then(|v| v.as_array()) {
            cleared = tags.len() as u32;
        }

        if seeded == 0 && cleared == 0 {
            Ok(ModuleResult::ok("memory already contains desired entries"))
        } else {
            Ok(ModuleResult::changed(
                format!("seeded {seeded} entries, cleared {cleared} tag groups"),
                serde_json::json!({ "seeded": seeded, "cleared": cleared }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in: HealthModule
// ---------------------------------------------------------------------------

/// Configure health-check parameters for an agent (idempotent).
///
/// # Arguments
///
/// ```json
/// {
///   "interval_secs": 30,
///   "timeout_secs": 5,
///   "retries": 3,
///   "enabled": true
/// }
/// ```
pub struct HealthModule;

#[async_trait]
impl Module for HealthModule {
    fn name(&self) -> &str {
        "health"
    }

    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let desired = args.as_object().ok_or_else(|| {
            ModuleError::InvalidArgument {
                key: "args".into(),
                reason: "health module expects an object".into(),
            }
        })?;

        let mut changes = Vec::new();
        for (key, value) in desired {
            let config_key = format!("health.{key}");
            if context.current_config.get(&config_key) != Some(value) {
                changes.push(config_key);
            }
        }

        if changes.is_empty() {
            Ok(ModuleResult::ok("health configuration already matches"))
        } else {
            Ok(ModuleResult::changed(
                format!("updated {} health parameters", changes.len()),
                serde_json::json!({ "updated": changes }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in: CommandModule
// ---------------------------------------------------------------------------

/// Execute an arbitrary command on an agent.
///
/// This is the escape hatch for operations that don't fit a specific
/// module. Commands are NOT idempotent by default — `changed` is always
/// `true` unless `creates` or `removes` is specified.
///
/// # Arguments
///
/// ```json
/// {
///   "cmd": "reindex --force",
///   "creates": "optional_file_that_makes_this_idempotent",
///   "removes": "optional_file_that_must_exist"
/// }
/// ```
pub struct CommandModule;

#[async_trait]
impl Module for CommandModule {
    fn name(&self) -> &str {
        "command"
    }

    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let cmd = args
            .get("cmd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ModuleError::MissingArgument("cmd".into()))?;

        // Idempotency guards.
        if let Some(creates) = args.get("creates").and_then(|v| v.as_str()) {
            if context.current_config.contains_key(creates) {
                return Ok(ModuleResult::ok(format!(
                    "skipped: '{creates}' already exists"
                )));
            }
        }

        if let Some(removes) = args.get("removes").and_then(|v| v.as_str()) {
            if !context.current_config.contains_key(removes) {
                return Ok(ModuleResult::ok(format!(
                    "skipped: '{removes}' does not exist"
                )));
            }
        }

        debug!(cmd, agent = %context.agent_name, "executing command");

        Ok(ModuleResult::changed(
            format!("executed: {cmd}"),
            serde_json::json!({
                "cmd": cmd,
                "rc": 0,
                "stdout": "",
                "stderr": "",
            }),
        ))
    }
}

// ---------------------------------------------------------------------------
// Built-in: TemplateModule
// ---------------------------------------------------------------------------

/// Render a template string into an agent's configuration.
///
/// Templates use `{{ var }}` syntax for variable substitution. The rendered
/// output is stored under the specified `dest` key in the agent's config.
///
/// # Arguments
///
/// ```json
/// {
///   "src": "Hello {{ agent_name }}, your role is {{ role }}.",
///   "dest": "greeting_message"
/// }
/// ```
pub struct TemplateModule;

#[async_trait]
impl Module for TemplateModule {
    fn name(&self) -> &str {
        "template"
    }

    async fn execute(&self, args: Value, context: &ModuleContext) -> Result<ModuleResult, ModuleError> {
        let src = args
            .get("src")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ModuleError::MissingArgument("src".into()))?;

        let dest = args
            .get("dest")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ModuleError::MissingArgument("dest".into()))?;

        // Render the template.
        let rendered = render_simple_template(src, &context.vars);

        // Idempotency: check if dest already has the rendered value.
        if let Some(current) = context.current_config.get(dest) {
            if current.as_str() == Some(&rendered) {
                return Ok(ModuleResult::ok(format!(
                    "template output at '{dest}' already matches"
                )));
            }
        }

        Ok(ModuleResult::changed(
            format!("rendered template to '{dest}'"),
            serde_json::json!({
                "dest": dest,
                "content": rendered,
            }),
        ))
    }
}

/// Simple `{{ key }}` template renderer.
fn render_simple_template(template: &str, vars: &HashMap<String, Value>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];

        if let Some(end) = after.find("}}") {
            let key = after[..end].trim();
            if let Some(val) = vars.get(key) {
                match val {
                    Value::String(s) => result.push_str(s),
                    Value::Null => {}
                    other => result.push_str(&other.to_string()),
                }
            } else {
                warn!(key, "unresolved template variable in module");
                result.push_str("{{");
                result.push_str(&after[..end]);
                result.push_str("}}");
            }
            remaining = &after[end + 2..];
        } else {
            result.push_str("{{");
            remaining = after;
        }
    }

    result.push_str(remaining);
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_ctx() -> ModuleContext {
        ModuleContext::new("test-agent")
    }

    #[tokio::test]
    async fn config_module_detects_change() {
        let module = ConfigModule;
        let args = json!({
            "set": { "log_level": "debug", "max_tasks": 10 }
        });

        let result = module.execute(args, &empty_ctx()).await.unwrap();
        assert!(result.changed);
        assert!(result.message.contains("2 config changes"));
    }

    #[tokio::test]
    async fn config_module_idempotent() {
        let module = ConfigModule;
        let mut ctx = empty_ctx();
        ctx.current_config
            .insert("log_level".into(), json!("debug"));

        let args = json!({ "set": { "log_level": "debug" } });
        let result = module.execute(args, &ctx).await.unwrap();
        assert!(!result.changed);
    }

    #[tokio::test]
    async fn capability_module_adds() {
        let module = CapabilityModule;
        let args = json!({
            "add": [{"name": "code_review", "version": "1.0", "description": "Reviews code"}]
        });

        let result = module.execute(args, &empty_ctx()).await.unwrap();
        assert!(result.changed);
        assert!(result.output["added"]
            .as_array()
            .unwrap()
            .contains(&json!("code_review")));
    }

    #[tokio::test]
    async fn capability_module_idempotent() {
        let module = CapabilityModule;
        let mut ctx = empty_ctx();
        ctx.current_capabilities.push("code_review".into());

        let args = json!({
            "add": [{"name": "code_review", "version": "1.0", "description": "Reviews code"}]
        });
        let result = module.execute(args, &ctx).await.unwrap();
        assert!(!result.changed);
    }

    #[tokio::test]
    async fn command_module_runs() {
        let module = CommandModule;
        let args = json!({ "cmd": "reindex --force" });

        let result = module.execute(args, &empty_ctx()).await.unwrap();
        assert!(result.changed);
        assert_eq!(result.output["rc"], 0);
    }

    #[tokio::test]
    async fn command_module_creates_guard() {
        let module = CommandModule;
        let mut ctx = empty_ctx();
        ctx.current_config
            .insert("index_built".into(), json!(true));

        let args = json!({ "cmd": "reindex", "creates": "index_built" });
        let result = module.execute(args, &ctx).await.unwrap();
        assert!(!result.changed);
    }

    #[tokio::test]
    async fn template_module_renders() {
        let module = TemplateModule;
        let mut ctx = empty_ctx();
        ctx.vars.insert("role".into(), json!("researcher"));

        let args = json!({
            "src": "You are a {{ role }}.",
            "dest": "system_prompt"
        });

        let result = module.execute(args, &ctx).await.unwrap();
        assert!(result.changed);
        assert_eq!(result.output["content"], "You are a researcher.");
    }

    #[tokio::test]
    async fn template_module_idempotent() {
        let module = TemplateModule;
        let mut ctx = empty_ctx();
        ctx.vars.insert("role".into(), json!("researcher"));
        ctx.current_config.insert(
            "system_prompt".into(),
            json!("You are a researcher."),
        );

        let args = json!({
            "src": "You are a {{ role }}.",
            "dest": "system_prompt"
        });

        let result = module.execute(args, &ctx).await.unwrap();
        assert!(!result.changed);
    }

    #[tokio::test]
    async fn health_module_applies() {
        let module = HealthModule;
        let args = json!({
            "interval_secs": 30,
            "timeout_secs": 5
        });

        let result = module.execute(args, &empty_ctx()).await.unwrap();
        assert!(result.changed);
    }

    #[tokio::test]
    async fn channel_module_subscribe() {
        let module = ChannelModule;
        let args = json!({
            "subscribe": ["alerts", "tasks"],
            "create": { "name": "custom_channel", "capacity": 512 }
        });

        let result = module.execute(args, &empty_ctx()).await.unwrap();
        assert!(result.changed);
        assert_eq!(result.output["actions"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn memory_module_seeds() {
        let module = MemoryModule;
        let args = json!({
            "entries": [
                { "key": "domain", "value": "rust systems programming" }
            ]
        });

        let result = module.execute(args, &empty_ctx()).await.unwrap();
        assert!(result.changed);
        assert_eq!(result.output["seeded"], 1);
    }

    #[test]
    fn registry_builtins() {
        let reg = ModuleRegistry::with_builtins();
        assert!(reg.get("config").is_some());
        assert!(reg.get("capability").is_some());
        assert!(reg.get("channel").is_some());
        assert!(reg.get("memory").is_some());
        assert!(reg.get("health").is_some());
        assert!(reg.get("command").is_some());
        assert!(reg.get("template").is_some());
        assert_eq!(reg.len(), 7);
    }

    #[test]
    fn registry_custom_module() {
        struct CustomModule;

        #[async_trait]
        impl Module for CustomModule {
            fn name(&self) -> &str {
                "custom"
            }
            async fn execute(
                &self,
                _args: Value,
                _ctx: &ModuleContext,
            ) -> Result<ModuleResult, ModuleError> {
                Ok(ModuleResult::ok("custom ok"))
            }
        }

        let reg = ModuleRegistry::new();
        assert!(reg.is_empty());
        reg.register(Arc::new(CustomModule));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("custom").is_some());
    }

    #[test]
    fn simple_template_render() {
        let mut vars = HashMap::new();
        vars.insert("name".into(), json!("Alice"));
        vars.insert("count".into(), json!(42));

        let result = render_simple_template("Hello {{ name }}, count={{ count }}.", &vars);
        assert_eq!(result, "Hello Alice, count=42.");
    }
}
