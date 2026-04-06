//! Layered variable management with template rendering.
//!
//! Variables in DAF-configure follow a precedence model inspired by Ansible
//! but simplified for the agent domain. Variables at a more specific scope
//! override those at a broader scope:
//!
//! ```text
//!   Task vars  (highest priority)
//!     ↓
//!   Agent vars
//!     ↓
//!   Play vars
//!     ↓
//!   Role vars
//!     ↓
//!   Role defaults
//!     ↓
//!   Global vars  (lowest priority)
//! ```
//!
//! Template rendering substitutes `{{ var_name }}` references in strings,
//! and `{{ vault:secret_name }}` triggers secret resolution through the
//! vault provider callback.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

// ---------------------------------------------------------------------------
// VarScope
// ---------------------------------------------------------------------------

/// Scoping level for variable declarations.
///
/// Variables declared at a narrower scope shadow those at a broader scope.
/// The numeric ordering here defines precedence: higher discriminant wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VarScope {
    /// Framework-wide defaults.
    Global = 0,
    /// Role `defaults/` directory — lowest role-level precedence.
    RoleDefault = 1,
    /// Role `vars/` directory.
    Role = 2,
    /// Play-level `vars` block.
    Play = 3,
    /// Per-agent variables from the inventory.
    Agent = 4,
    /// Task-level `vars` or `register` output.
    Task = 5,
}

// ---------------------------------------------------------------------------
// VarLayer
// ---------------------------------------------------------------------------

/// A single layer in the variable stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VarLayer {
    scope: VarScope,
    label: String,
    values: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// VarManager
// ---------------------------------------------------------------------------

/// Manages layered variable resolution and template rendering.
///
/// Variables are stored in ordered layers. When a key is looked up, the
/// manager walks layers from highest precedence (most recently pushed
/// `Task` layer) to lowest (`Global`), returning the first match.
#[derive(Debug, Clone, Default)]
pub struct VarManager {
    /// Layers ordered from lowest to highest precedence.
    layers: Vec<VarLayer>,
}

impl VarManager {
    /// Create an empty variable manager.
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
        }
    }

    /// Push a new variable layer at the given scope.
    ///
    /// The `label` is a human-readable identifier for debugging (e.g.
    /// `"play:configure-researchers"` or `"role:security-baseline"`).
    pub fn push_layer(
        &mut self,
        scope: VarScope,
        label: impl Into<String>,
        values: HashMap<String, Value>,
    ) {
        self.layers.push(VarLayer {
            scope,
            label: label.into(),
            values,
        });
        // Keep layers sorted by scope precedence so highest-precedence
        // layers come last. Within the same scope, later pushes win
        // because we iterate in reverse.
    }

    /// Remove all layers with the given scope.
    pub fn pop_scope(&mut self, scope: VarScope) {
        self.layers.retain(|l| l.scope != scope);
    }

    /// Set a single variable in the most recently pushed layer of `scope`.
    /// If no layer exists at that scope, one is created.
    pub fn set(&mut self, scope: VarScope, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(layer) = self.layers.iter_mut().rev().find(|l| l.scope == scope) {
            layer.values.insert(key, value);
        } else {
            let mut values = HashMap::new();
            values.insert(key, value);
            self.push_layer(scope, format!("{scope:?}"), values);
        }
    }

    /// Resolve a variable by key, searching from highest to lowest precedence.
    pub fn get(&self, key: &str) -> Option<&Value> {
        // Walk layers in reverse (highest precedence first).
        for layer in self.layers.iter().rev() {
            if let Some(v) = layer.values.get(key) {
                return Some(v);
            }
        }
        None
    }

    /// Flatten all layers into a single `HashMap`, respecting precedence.
    ///
    /// This is the view of variables that tasks and conditions see at
    /// evaluation time.
    pub fn merged(&self) -> HashMap<String, Value> {
        let mut result = HashMap::new();
        // Iterate low-to-high precedence so higher overrides lower.
        for layer in &self.layers {
            for (k, v) in &layer.values {
                result.insert(k.clone(), v.clone());
            }
        }
        result
    }

    /// Render template substitutions in a string.
    ///
    /// Replaces `{{ key }}` with the resolved variable value. Supports
    /// `{{ vault:secret_name }}` syntax, resolved through the optional
    /// `vault_resolver` callback.
    ///
    /// Unresolved references are left as-is and a warning is logged.
    pub fn render_template(
        &self,
        template: &str,
        vault_resolver: Option<&dyn Fn(&str) -> Option<String>>,
    ) -> String {
        render_template_with_vars(template, &self.merged(), vault_resolver)
    }

    /// Render template substitutions in a JSON [`Value`], recursively
    /// walking strings, arrays, and objects.
    pub fn render_value(
        &self,
        value: &Value,
        vault_resolver: Option<&dyn Fn(&str) -> Option<String>>,
    ) -> Value {
        render_value_recursive(value, &self.merged(), vault_resolver)
    }

    /// Inject environment variables into the Global scope.
    ///
    /// Environment variables are prefixed with `env_` to avoid collisions
    /// (e.g. `HOME` becomes `env_HOME`).
    pub fn inject_env(&mut self) {
        let mut env_vars = HashMap::new();
        for (key, value) in std::env::vars() {
            env_vars.insert(format!("env_{key}"), Value::String(value));
        }
        self.push_layer(VarScope::Global, "environment", env_vars);
    }

    /// Register a task result under the given name so subsequent tasks
    /// can reference it via conditions and templates.
    pub fn register_result(&mut self, name: impl Into<String>, output: Value) {
        self.set(VarScope::Task, name, output);
    }

    /// Return all layers for debugging/inspection.
    pub fn layers(&self) -> &[impl std::fmt::Debug] {
        &self.layers
    }
}

// ---------------------------------------------------------------------------
// Template rendering internals
// ---------------------------------------------------------------------------

/// Replace `{{ key }}` patterns in `template` with values from `vars`.
fn render_template_with_vars(
    template: &str,
    vars: &HashMap<String, Value>,
    vault_resolver: Option<&dyn Fn(&str) -> Option<String>>,
) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + 2..];

        if let Some(end) = after_open.find("}}") {
            let raw_key = after_open[..end].trim();
            let replacement = resolve_template_key(raw_key, vars, vault_resolver);
            result.push_str(&replacement);
            remaining = &after_open[end + 2..];
        } else {
            // No closing braces — emit literal.
            result.push_str("{{");
            remaining = after_open;
        }
    }

    result.push_str(remaining);
    result
}

/// Resolve a single template key, handling `vault:` prefix.
fn resolve_template_key(
    key: &str,
    vars: &HashMap<String, Value>,
    vault_resolver: Option<&dyn Fn(&str) -> Option<String>>,
) -> String {
    // Vault secret reference: {{ vault:secret_name }}
    if let Some(secret_name) = key.strip_prefix("vault:") {
        let secret_name = secret_name.trim();
        if let Some(resolver) = vault_resolver {
            if let Some(value) = resolver(secret_name) {
                return value;
            }
        }
        warn!(secret = secret_name, "unresolved vault reference");
        return format!("{{{{ vault:{secret_name} }}}}");
    }

    // Regular variable lookup (supports dot paths).
    if let Some(value) = resolve_dot_path(vars, key) {
        return value_to_string(&value);
    }

    warn!(key, "unresolved template variable");
    format!("{{{{ {key} }}}}")
}

/// Walk a dot-separated path into nested JSON values.
fn resolve_dot_path(vars: &HashMap<String, Value>, path: &str) -> Option<Value> {
    let mut parts = path.split('.');
    let root = parts.next()?;
    let mut current = vars.get(root)?.clone();

    for part in parts {
        match current {
            Value::Object(map) => {
                current = map.get(part)?.clone();
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Convert a JSON value to a string suitable for template insertion.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Recursively render template references inside a JSON value tree.
fn render_value_recursive(
    value: &Value,
    vars: &HashMap<String, Value>,
    vault_resolver: Option<&dyn Fn(&str) -> Option<String>>,
) -> Value {
    match value {
        Value::String(s) => {
            let rendered = render_template_with_vars(s, vars, vault_resolver);
            // If the whole string was a single template ref that resolved to
            // a non-string JSON value, try to parse it back.
            if s.starts_with("{{") && s.ends_with("}}") && s.matches("{{").count() == 1 {
                if let Ok(parsed) = serde_json::from_str::<Value>(&rendered) {
                    return parsed;
                }
            }
            Value::String(rendered)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| render_value_recursive(v, vars, vault_resolver))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let rendered_key = render_template_with_vars(k, vars, vault_resolver);
                    (rendered_key, render_value_recursive(v, vars, vault_resolver))
                })
                .collect(),
        ),
        other => other.clone(),
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
    fn basic_resolution() {
        let mut mgr = VarManager::new();
        let mut globals = HashMap::new();
        globals.insert("region".into(), json!("us-east"));
        mgr.push_layer(VarScope::Global, "globals", globals);

        assert_eq!(mgr.get("region"), Some(&json!("us-east")));
        assert_eq!(mgr.get("missing"), None);
    }

    #[test]
    fn precedence_higher_scope_wins() {
        let mut mgr = VarManager::new();

        let mut globals = HashMap::new();
        globals.insert("timeout".into(), json!(30));
        mgr.push_layer(VarScope::Global, "globals", globals);

        let mut play_vars = HashMap::new();
        play_vars.insert("timeout".into(), json!(60));
        mgr.push_layer(VarScope::Play, "play", play_vars);

        assert_eq!(mgr.get("timeout"), Some(&json!(60)));
    }

    #[test]
    fn task_scope_overrides_all() {
        let mut mgr = VarManager::new();

        let mut globals = HashMap::new();
        globals.insert("x".into(), json!(1));
        mgr.push_layer(VarScope::Global, "globals", globals);

        mgr.set(VarScope::Task, "x", json!(99));
        assert_eq!(mgr.get("x"), Some(&json!(99)));
    }

    #[test]
    fn pop_scope() {
        let mut mgr = VarManager::new();
        mgr.set(VarScope::Task, "a", json!(1));
        mgr.set(VarScope::Play, "b", json!(2));

        mgr.pop_scope(VarScope::Task);
        assert_eq!(mgr.get("a"), None);
        assert_eq!(mgr.get("b"), Some(&json!(2)));
    }

    #[test]
    fn merged_view() {
        let mut mgr = VarManager::new();
        mgr.set(VarScope::Global, "a", json!(1));
        mgr.set(VarScope::Global, "b", json!(2));
        mgr.set(VarScope::Task, "b", json!(20));

        let merged = mgr.merged();
        assert_eq!(merged.get("a"), Some(&json!(1)));
        assert_eq!(merged.get("b"), Some(&json!(20)));
    }

    #[test]
    fn template_rendering() {
        let mut mgr = VarManager::new();
        mgr.set(VarScope::Global, "name", json!("Alice"));
        mgr.set(VarScope::Global, "role", json!("researcher"));

        let result = mgr.render_template("Hello {{ name }}, you are a {{ role }}.", None);
        assert_eq!(result, "Hello Alice, you are a researcher.");
    }

    #[test]
    fn template_unresolved_preserved() {
        let mgr = VarManager::new();
        let result = mgr.render_template("Value is {{ missing }}.", None);
        assert_eq!(result, "Value is {{ missing }}.");
    }

    #[test]
    fn template_vault_resolution() {
        let mgr = VarManager::new();
        let resolver = |name: &str| -> Option<String> {
            if name == "api_key" {
                Some("sk-secret-123".into())
            } else {
                None
            }
        };
        let result = mgr.render_template("Key: {{ vault:api_key }}", Some(&resolver));
        assert_eq!(result, "Key: sk-secret-123");
    }

    #[test]
    fn template_vault_unresolved() {
        let mgr = VarManager::new();
        let resolver = |_: &str| -> Option<String> { None };
        let result = mgr.render_template("Key: {{ vault:missing }}", Some(&resolver));
        assert_eq!(result, "Key: {{ vault:missing }}");
    }

    #[test]
    fn render_value_recursive_test() {
        let mut mgr = VarManager::new();
        mgr.set(VarScope::Global, "host", json!("localhost"));
        mgr.set(VarScope::Global, "port", json!("8080"));

        let template = json!({
            "url": "http://{{ host }}:{{ port }}",
            "tags": ["{{ host }}", "production"],
            "nested": {
                "value": "{{ host }}"
            }
        });

        let rendered = mgr.render_value(&template, None);
        assert_eq!(rendered["url"], "http://localhost:8080");
        assert_eq!(rendered["tags"][0], "localhost");
        assert_eq!(rendered["nested"]["value"], "localhost");
    }

    #[test]
    fn register_result() {
        let mut mgr = VarManager::new();
        mgr.register_result("deploy_output", json!({"status": "ok", "version": 3}));

        assert_eq!(
            mgr.get("deploy_output"),
            Some(&json!({"status": "ok", "version": 3}))
        );
    }

    #[test]
    fn inject_env() {
        let mut mgr = VarManager::new();
        // SAFETY: single-threaded test; no other thread reads this var.
        unsafe { std::env::set_var("DAF_TEST_VAR_12345", "hello") };
        mgr.inject_env();

        assert_eq!(
            mgr.get("env_DAF_TEST_VAR_12345"),
            Some(&json!("hello"))
        );
        unsafe { std::env::remove_var("DAF_TEST_VAR_12345") };
    }

    #[test]
    fn dot_path_in_template() {
        let mut mgr = VarManager::new();
        mgr.set(
            VarScope::Global,
            "result",
            json!({"status": "ok", "data": {"count": 42}}),
        );

        let rendered = mgr.render_template("Status: {{ result.status }}", None);
        assert_eq!(rendered, "Status: ok");
    }
}
