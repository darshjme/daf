//! Conditional execution for playbook tasks.
//!
//! Conditions are evaluated against a variable context (a flat
//! `HashMap<String, Value>`) to decide whether a task or handler should
//! run. The condition tree supports boolean combinators (`And`, `Or`,
//! `Not`) so complex predicates can be composed declaratively in YAML
//! playbooks.
//!
//! # Variable references
//!
//! Condition keys use dot-separated paths that are resolved against the
//! variable context. A key like `"agent.kind"` looks up the nested path
//! in the JSON value stored under `"agent"`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Condition
// ---------------------------------------------------------------------------

/// A boolean predicate evaluated against a variable context.
///
/// Conditions gate task execution in playbooks (`when` field) and guard
/// role dependencies. They form a small expression tree that the engine
/// evaluates at runtime, after variable interpolation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// True when `key` resolves to a value equal to `expected`.
    Equals(String, Value),
    /// True when `key` resolves to a value *not* equal to `expected`.
    NotEquals(String, Value),
    /// True when `key` resolves to a string or array that contains `needle`.
    Contains(String, Value),
    /// True when `key` is present in the variable context (even if null).
    Defined(String),
    /// True when `key` is absent from the variable context.
    Undefined(String),
    /// All sub-conditions must be true.
    And(Vec<Condition>),
    /// At least one sub-condition must be true.
    Or(Vec<Condition>),
    /// Negates the inner condition.
    Not(Box<Condition>),
    /// A free-form expression string evaluated by a simple interpreter.
    /// Supports `==`, `!=`, `contains`, `is defined`, `is undefined`.
    Expression(String),
}

impl Condition {
    /// Evaluate this condition against `vars`.
    ///
    /// Returns `true` when the condition is satisfied. Unknown keys are
    /// treated as absent (not defined), and comparisons against absent
    /// keys return `false` except for [`Condition::Undefined`].
    pub fn evaluate(&self, vars: &HashMap<String, Value>) -> bool {
        match self {
            Self::Equals(key, expected) => resolve(vars, key)
                .map(|v| v == *expected)
                .unwrap_or(false),

            Self::NotEquals(key, expected) => resolve(vars, key)
                .map(|v| v != *expected)
                .unwrap_or(true),

            Self::Contains(key, needle) => resolve(vars, key)
                .map(|v| value_contains(&v, needle))
                .unwrap_or(false),

            Self::Defined(key) => resolve(vars, key).is_some(),

            Self::Undefined(key) => resolve(vars, key).is_none(),

            Self::And(conds) => conds.iter().all(|c| c.evaluate(vars)),

            Self::Or(conds) => conds.iter().any(|c| c.evaluate(vars)),

            Self::Not(inner) => !inner.evaluate(vars),

            Self::Expression(expr) => evaluate_expression(expr, vars),
        }
    }
}

// ---------------------------------------------------------------------------
// Variable resolution
// ---------------------------------------------------------------------------

/// Resolve a dot-separated key path against the variable map.
///
/// Given `vars = {"agent": {"kind": "worker"}}` and `key = "agent.kind"`,
/// returns `Some(Value::String("worker"))`.
fn resolve(vars: &HashMap<String, Value>, key: &str) -> Option<Value> {
    let mut parts = key.split('.');
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

/// Check whether `haystack` contains `needle`.
///
/// - If `haystack` is a string and `needle` is a string, performs substring match.
/// - If `haystack` is an array, checks for element equality.
/// - Otherwise returns false.
fn value_contains(haystack: &Value, needle: &Value) -> bool {
    match (haystack, needle) {
        (Value::String(h), Value::String(n)) => h.contains(n.as_str()),
        (Value::Array(arr), _) => arr.contains(needle),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Simple expression evaluator
// ---------------------------------------------------------------------------

/// Evaluate a simple expression string.
///
/// Supported forms:
/// - `"key == value"`
/// - `"key != value"`
/// - `"key contains value"`
/// - `"key is defined"`
/// - `"key is undefined"`
///
/// Values are parsed as JSON literals when possible, falling back to plain
/// strings.
fn evaluate_expression(expr: &str, vars: &HashMap<String, Value>) -> bool {
    let expr = expr.trim();

    // "key is defined" / "key is undefined"
    if let Some(key) = expr.strip_suffix(" is defined") {
        return resolve(vars, key.trim()).is_some();
    }
    if let Some(key) = expr.strip_suffix(" is undefined") {
        return resolve(vars, key.trim()).is_none();
    }

    // Binary operators
    let ops: &[(&str, fn(&Value, &Value) -> bool)] = &[
        (" == ", bin_eq),
        (" != ", bin_ne),
        (" contains ", bin_contains),
    ];
    for &(op, handler) in ops {
        if let Some(idx) = expr.find(op) {
            let key = expr[..idx].trim();
            let raw_val = expr[idx + op.len()..].trim();
            let expected = parse_value(raw_val);
            return resolve(vars, key)
                .map(|v| handler(&v, &expected))
                .unwrap_or(false);
        }
    }

    // Fallback: treat the whole expression as a variable name; truthy check.
    resolve(vars, expr)
        .map(|v| is_truthy(&v))
        .unwrap_or(false)
}

fn bin_eq(a: &Value, b: &Value) -> bool {
    a == b
}

fn bin_ne(a: &Value, b: &Value) -> bool {
    a != b
}

fn bin_contains(a: &Value, b: &Value) -> bool {
    value_contains(a, b)
}

/// Parse a raw string as a JSON value, falling back to a plain string.
fn parse_value(raw: &str) -> Value {
    // Strip surrounding quotes if present.
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return v;
    }
    Value::String(trimmed.to_string())
}

/// JSON-style truthiness: null, false, 0, empty string, empty array/object
/// are falsy. Everything else is truthy.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_vars() -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("agent_kind".into(), json!("worker"));
        m.insert("enabled".into(), json!(true));
        m.insert("count".into(), json!(42));
        m.insert("tags".into(), json!(["fast", "gpu"]));
        m.insert(
            "agent".into(),
            json!({"kind": "specialist", "name": "coder"}),
        );
        m.insert("empty_str".into(), json!(""));
        m
    }

    #[test]
    fn equals_string() {
        let vars = test_vars();
        let c = Condition::Equals("agent_kind".into(), json!("worker"));
        assert!(c.evaluate(&vars));

        let c2 = Condition::Equals("agent_kind".into(), json!("monitor"));
        assert!(!c2.evaluate(&vars));
    }

    #[test]
    fn not_equals() {
        let vars = test_vars();
        let c = Condition::NotEquals("agent_kind".into(), json!("monitor"));
        assert!(c.evaluate(&vars));

        // Missing key: not-equals against absent is true.
        let c2 = Condition::NotEquals("missing".into(), json!("x"));
        assert!(c2.evaluate(&vars));
    }

    #[test]
    fn contains_array() {
        let vars = test_vars();
        let c = Condition::Contains("tags".into(), json!("gpu"));
        assert!(c.evaluate(&vars));

        let c2 = Condition::Contains("tags".into(), json!("cpu"));
        assert!(!c2.evaluate(&vars));
    }

    #[test]
    fn contains_string() {
        let vars = test_vars();
        let c = Condition::Contains("agent_kind".into(), json!("work"));
        assert!(c.evaluate(&vars));
    }

    #[test]
    fn defined_undefined() {
        let vars = test_vars();
        assert!(Condition::Defined("count".into()).evaluate(&vars));
        assert!(!Condition::Defined("missing".into()).evaluate(&vars));
        assert!(Condition::Undefined("missing".into()).evaluate(&vars));
        assert!(!Condition::Undefined("count".into()).evaluate(&vars));
    }

    #[test]
    fn boolean_combinators() {
        let vars = test_vars();
        let c = Condition::And(vec![
            Condition::Equals("agent_kind".into(), json!("worker")),
            Condition::Equals("enabled".into(), json!(true)),
        ]);
        assert!(c.evaluate(&vars));

        let c2 = Condition::Or(vec![
            Condition::Equals("agent_kind".into(), json!("monitor")),
            Condition::Equals("enabled".into(), json!(true)),
        ]);
        assert!(c2.evaluate(&vars));

        let c3 = Condition::Not(Box::new(Condition::Equals(
            "agent_kind".into(),
            json!("monitor"),
        )));
        assert!(c3.evaluate(&vars));
    }

    #[test]
    fn nested_key_resolution() {
        let vars = test_vars();
        let c = Condition::Equals("agent.kind".into(), json!("specialist"));
        assert!(c.evaluate(&vars));

        let c2 = Condition::Equals("agent.name".into(), json!("coder"));
        assert!(c2.evaluate(&vars));
    }

    #[test]
    fn expression_equals() {
        let vars = test_vars();
        let c = Condition::Expression("agent_kind == \"worker\"".into());
        assert!(c.evaluate(&vars));
    }

    #[test]
    fn expression_not_equals() {
        let vars = test_vars();
        let c = Condition::Expression("agent_kind != \"monitor\"".into());
        assert!(c.evaluate(&vars));
    }

    #[test]
    fn expression_defined() {
        let vars = test_vars();
        let c = Condition::Expression("count is defined".into());
        assert!(c.evaluate(&vars));

        let c2 = Condition::Expression("missing is undefined".into());
        assert!(c2.evaluate(&vars));
    }

    #[test]
    fn expression_truthy_fallback() {
        let vars = test_vars();
        let c = Condition::Expression("enabled".into());
        assert!(c.evaluate(&vars));

        let c2 = Condition::Expression("empty_str".into());
        assert!(!c2.evaluate(&vars));
    }

    #[test]
    fn condition_serde_roundtrip() {
        let c = Condition::And(vec![
            Condition::Equals("x".into(), json!(1)),
            Condition::Not(Box::new(Condition::Undefined("y".into()))),
        ]);
        let json = serde_json::to_string(&c).unwrap();
        let back: Condition = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
