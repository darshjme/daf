//! Provision output collection and reference resolution.
//!
//! After resources are provisioned, they produce outputs — key-value pairs
//! that describe the created infrastructure (pool IDs, channel endpoints,
//! agent counts). Outputs can be:
//!
//! - Exposed to the operator for inspection
//! - Referenced by other resources via `${resource.name.output_key}` syntax
//! - Marked as sensitive so they are masked in display but available programmatically

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use daf_core::{DafError, DafResult};

use crate::state::ProvisionState;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// A single named output from a provisioned resource or topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    /// Output name.
    pub name: String,

    /// The output value.
    pub value: Value,

    /// Whether this output contains sensitive data (e.g., tokens, passwords).
    /// Sensitive outputs are masked in human-readable display but available
    /// programmatically.
    pub sensitive: bool,

    /// Human-readable description of what this output represents.
    pub description: String,
}

impl Output {
    /// Create a new non-sensitive output.
    pub fn new(name: impl Into<String>, value: Value, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value,
            sensitive: false,
            description: description.into(),
        }
    }

    /// Create a sensitive output (masked in display).
    pub fn sensitive(
        name: impl Into<String>,
        value: Value,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            value,
            sensitive: true,
            description: description.into(),
        }
    }

    /// Return the display representation of the value, masking sensitive outputs.
    pub fn display_value(&self) -> String {
        if self.sensitive {
            "(sensitive)".into()
        } else {
            match &self.value {
                Value::String(s) => format!("\"{s}\""),
                other => other.to_string(),
            }
        }
    }
}

impl fmt::Display for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} = {} # {}",
            self.name,
            self.display_value(),
            self.description,
        )
    }
}

// ---------------------------------------------------------------------------
// OutputCollector
// ---------------------------------------------------------------------------

/// Collects outputs from provisioned resources and resolves cross-resource
/// references.
///
/// After applying a plan, the operator creates an `OutputCollector`, registers
/// the outputs they care about, and then calls [`resolve`] to substitute
/// `${resource.name.key}` references with actual values from the state.
#[derive(Debug, Default)]
pub struct OutputCollector {
    outputs: Vec<Output>,
}

impl OutputCollector {
    /// Create a new empty collector.
    pub fn new() -> Self {
        Self {
            outputs: Vec::new(),
        }
    }

    /// Add an output to the collection.
    pub fn add(&mut self, output: Output) {
        self.outputs.push(output);
    }

    /// Add a non-sensitive output.
    pub fn add_value(
        &mut self,
        name: impl Into<String>,
        value: Value,
        description: impl Into<String>,
    ) {
        self.outputs.push(Output::new(name, value, description));
    }

    /// Add a sensitive output.
    pub fn add_sensitive(
        &mut self,
        name: impl Into<String>,
        value: Value,
        description: impl Into<String>,
    ) {
        self.outputs
            .push(Output::sensitive(name, value, description));
    }

    /// Collect outputs from all resources in the provision state.
    ///
    /// Each resource output is namespaced as `{resource_name}.{output_key}`.
    pub fn collect_from_state(&mut self, state: &ProvisionState) {
        for (resource_name, resource) in &state.resources {
            for (key, value) in &resource.outputs {
                let output_name = format!("{resource_name}.{key}");
                self.outputs.push(Output::new(
                    &output_name,
                    value.clone(),
                    format!("Output '{key}' from resource '{resource_name}'"),
                ));
            }
        }
    }

    /// Resolve `${resource.name.key}` references in output values.
    ///
    /// Scans all output values for strings containing the `${...}` pattern
    /// and replaces them with the actual value from the provision state.
    pub fn resolve(&mut self, state: &ProvisionState) -> DafResult<()> {
        let snapshot: HashMap<String, Value> = self
            .outputs
            .iter()
            .map(|o| (o.name.clone(), o.value.clone()))
            .collect();

        for output in &mut self.outputs {
            output.value = resolve_references(&output.value, state, &snapshot)?;
        }

        Ok(())
    }

    /// Return all collected outputs.
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// Look up an output by name.
    pub fn get(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name == name)
    }

    /// Return only non-sensitive outputs.
    pub fn public_outputs(&self) -> Vec<&Output> {
        self.outputs.iter().filter(|o| !o.sensitive).collect()
    }

    /// Return the number of collected outputs.
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    /// Returns `true` if no outputs have been collected.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    /// Format all outputs as a human-readable string.
    pub fn display_all(&self) -> String {
        if self.outputs.is_empty() {
            return "No outputs.\n".into();
        }

        let mut out = String::from("Outputs:\n\n");
        for output in &self.outputs {
            out.push_str(&format!("  {output}\n"));
        }
        out
    }

    /// Convert all outputs to a HashMap suitable for JSON serialization.
    pub fn to_map(&self) -> HashMap<String, Value> {
        self.outputs
            .iter()
            .map(|o| (o.name.clone(), o.value.clone()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Reference resolution
// ---------------------------------------------------------------------------

/// Pattern for output references: `${resource_name.output_key}`
const REF_PREFIX: &str = "${";
const REF_SUFFIX: &str = "}";

/// Resolve `${resource.name.key}` references in a JSON value.
fn resolve_references(
    value: &Value,
    state: &ProvisionState,
    snapshot: &HashMap<String, Value>,
) -> DafResult<Value> {
    match value {
        Value::String(s) => resolve_string_references(s, state, snapshot),
        Value::Array(arr) => {
            let resolved: Result<Vec<Value>, _> = arr
                .iter()
                .map(|v| resolve_references(v, state, snapshot))
                .collect();
            Ok(Value::Array(resolved?))
        }
        Value::Object(obj) => {
            let mut resolved = serde_json::Map::new();
            for (k, v) in obj {
                resolved.insert(k.clone(), resolve_references(v, state, snapshot)?);
            }
            Ok(Value::Object(resolved))
        }
        // Non-string primitives pass through unchanged.
        other => Ok(other.clone()),
    }
}

/// Resolve references in a string value.
///
/// If the entire string is a single reference (`${resource.key}`), the
/// resolved value replaces it directly (preserving type). If the string
/// contains embedded references mixed with text, each reference is
/// substituted as a string.
fn resolve_string_references(
    s: &str,
    state: &ProvisionState,
    snapshot: &HashMap<String, Value>,
) -> DafResult<Value> {
    // Check if the entire string is a single reference.
    if s.starts_with(REF_PREFIX) && s.ends_with(REF_SUFFIX) && s.matches(REF_PREFIX).count() == 1 {
        let ref_path = &s[REF_PREFIX.len()..s.len() - REF_SUFFIX.len()];
        return lookup_reference(ref_path, state, snapshot);
    }

    // Handle embedded references.
    let mut result = s.to_owned();
    while let Some(start) = result.find(REF_PREFIX) {
        let rest = &result[start + REF_PREFIX.len()..];
        if let Some(end) = rest.find(REF_SUFFIX) {
            let ref_path = &rest[..end];
            let resolved = lookup_reference(ref_path, state, snapshot)?;
            let replacement = match &resolved {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = format!(
                "{}{}{}",
                &result[..start],
                replacement,
                &rest[end + REF_SUFFIX.len()..],
            );
        } else {
            break; // Unclosed reference, leave as-is.
        }
    }

    Ok(Value::String(result))
}

/// Look up a `resource_name.output_key` reference.
fn lookup_reference(
    ref_path: &str,
    state: &ProvisionState,
    snapshot: &HashMap<String, Value>,
) -> DafResult<Value> {
    // First check the snapshot (other outputs in this collector).
    if let Some(value) = snapshot.get(ref_path) {
        debug!(reference = %ref_path, "resolved from snapshot");
        return Ok(value.clone());
    }

    // Then check the state's resource outputs.
    let parts: Vec<&str> = ref_path.splitn(2, '.').collect();
    if parts.len() == 2 {
        let resource_name = parts[0];
        let output_key = parts[1];

        if let Some(resource) = state.get_resource(resource_name) {
            if let Some(value) = resource.get_output(output_key) {
                debug!(
                    resource = %resource_name,
                    key = %output_key,
                    "resolved output reference"
                );
                return Ok(value.clone());
            }
        }
    }

    Err(DafError::NotFound {
        entity: "output reference".into(),
        id: ref_path.into(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{Resource, ResourceSpec, ResourceState, ResourceType};

    fn state_with_outputs() -> ProvisionState {
        let mut state = ProvisionState::new();

        let spec = ResourceSpec::new(ResourceType::AgentPool, "workers", "agent_pool")
            .with_config(serde_json::json!({"count": 5}));
        let mut resource = Resource::from_spec(spec);
        resource.transition(ResourceState::Created);
        resource.set_physical_id("pool-abc");
        resource.set_output("pool_id", Value::String("pool-abc".into()));
        resource.set_output("count", serde_json::json!(5));
        resource.set_output("token", Value::String("secret-token-123".into()));

        state.upsert_resource(resource);
        state
    }

    #[test]
    fn output_display_normal() {
        let output = Output::new(
            "url",
            Value::String("https://example.com".into()),
            "The URL",
        );
        let display = output.to_string();
        assert!(display.contains("url"));
        assert!(display.contains("https://example.com"));
        assert!(display.contains("The URL"));
    }

    #[test]
    fn output_display_sensitive() {
        let output = Output::sensitive("token", Value::String("secret".into()), "API token");
        let display = output.to_string();
        assert!(display.contains("(sensitive)"));
        assert!(!display.contains("secret"));
    }

    #[test]
    fn output_display_value_types() {
        let str_out = Output::new("s", Value::String("hello".into()), "");
        assert_eq!(str_out.display_value(), "\"hello\"");

        let num_out = Output::new("n", serde_json::json!(42), "");
        assert_eq!(num_out.display_value(), "42");

        let sensitive = Output::sensitive("x", Value::String("hidden".into()), "");
        assert_eq!(sensitive.display_value(), "(sensitive)");
    }

    #[test]
    fn collector_add_and_get() {
        let mut collector = OutputCollector::new();
        collector.add_value("url", Value::String("https://x.com".into()), "The URL");
        collector.add_sensitive("token", Value::String("secret".into()), "Token");

        assert_eq!(collector.len(), 2);
        assert!(!collector.is_empty());

        let url = collector.get("url").unwrap();
        assert!(!url.sensitive);

        let token = collector.get("token").unwrap();
        assert!(token.sensitive);

        assert!(collector.get("nonexistent").is_none());
    }

    #[test]
    fn collector_public_outputs() {
        let mut collector = OutputCollector::new();
        collector.add_value("public", serde_json::json!("yes"), "");
        collector.add_sensitive("private", serde_json::json!("no"), "");

        let public = collector.public_outputs();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].name, "public");
    }

    #[test]
    fn collector_collect_from_state() {
        let state = state_with_outputs();
        let mut collector = OutputCollector::new();
        collector.collect_from_state(&state);

        assert_eq!(collector.len(), 3);
        assert!(collector.get("workers.pool_id").is_some());
        assert!(collector.get("workers.count").is_some());
    }

    #[test]
    fn collector_to_map() {
        let mut collector = OutputCollector::new();
        collector.add_value("a", serde_json::json!(1), "");
        collector.add_value("b", serde_json::json!("two"), "");

        let map = collector.to_map();
        assert_eq!(map["a"], 1);
        assert_eq!(map["b"], "two");
    }

    #[test]
    fn resolve_simple_reference() {
        let state = state_with_outputs();
        let mut collector = OutputCollector::new();
        collector.add_value(
            "resolved",
            Value::String("${workers.pool_id}".into()),
            "Pool ID",
        );

        collector.resolve(&state).unwrap();

        let output = collector.get("resolved").unwrap();
        assert_eq!(output.value, Value::String("pool-abc".into()));
    }

    #[test]
    fn resolve_numeric_reference() {
        let state = state_with_outputs();
        let mut collector = OutputCollector::new();
        collector.add_value("count", Value::String("${workers.count}".into()), "Count");

        collector.resolve(&state).unwrap();

        let output = collector.get("count").unwrap();
        assert_eq!(output.value, serde_json::json!(5));
    }

    #[test]
    fn resolve_embedded_reference() {
        let state = state_with_outputs();
        let mut collector = OutputCollector::new();
        collector.add_value(
            "message",
            Value::String("Pool is ${workers.pool_id} with count ${workers.count}".into()),
            "Info",
        );

        collector.resolve(&state).unwrap();

        let output = collector.get("message").unwrap();
        assert_eq!(
            output.value,
            Value::String("Pool is pool-abc with count 5".into())
        );
    }

    #[test]
    fn resolve_missing_reference_fails() {
        let state = ProvisionState::new();
        let mut collector = OutputCollector::new();
        collector.add_value("bad", Value::String("${nonexistent.key}".into()), "Bad ref");

        let result = collector.resolve(&state);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_non_string_passthrough() {
        let state = state_with_outputs();
        let mut collector = OutputCollector::new();
        collector.add_value("number", serde_json::json!(42), "A number");

        collector.resolve(&state).unwrap();

        let output = collector.get("number").unwrap();
        assert_eq!(output.value, serde_json::json!(42));
    }

    #[test]
    fn resolve_array_with_references() {
        let state = state_with_outputs();
        let mut collector = OutputCollector::new();
        collector.add_value(
            "list",
            serde_json::json!(["${workers.pool_id}", "static"]),
            "Mixed array",
        );

        collector.resolve(&state).unwrap();

        let output = collector.get("list").unwrap();
        let arr = output.value.as_array().unwrap();
        assert_eq!(arr[0], "pool-abc");
        assert_eq!(arr[1], "static");
    }

    #[test]
    fn display_all_outputs() {
        let mut collector = OutputCollector::new();
        collector.add_value("a", serde_json::json!("hello"), "greeting");
        collector.add_sensitive("b", serde_json::json!("secret"), "token");

        let display = collector.display_all();
        assert!(display.contains("Outputs:"));
        assert!(display.contains("hello"));
        assert!(display.contains("(sensitive)"));
    }

    #[test]
    fn display_empty_outputs() {
        let collector = OutputCollector::new();
        let display = collector.display_all();
        assert!(display.contains("No outputs"));
    }
}
