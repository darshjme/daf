//! Handler system for deferred, deduplicated actions.
//!
//! Handlers are the DAF equivalent of Ansible handlers: named actions that
//! are *notified* by tasks but only execute once at the end of a play (or
//! when explicitly flushed). Multiple notifications of the same handler
//! collapse into a single execution.
//!
//! This is the right pattern for "restart the agent after all config
//! changes are applied" or "flush the memory cache once, not after every
//! write."

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// A named, deferred action triggered by task notifications.
///
/// Handlers are structurally identical to tasks — they specify a module
/// and arguments — but they execute only when notified, and at most once
/// per play run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handler {
    /// Unique name within the play or role. Tasks reference this in their
    /// `notify` list.
    pub name: String,
    /// The configuration module to invoke (e.g. `"config"`, `"command"`).
    pub module: String,
    /// Arguments passed to the module.
    pub args: Value,
    /// Additional event names this handler listens to.
    ///
    /// A handler can respond to its own `name` *and* any names listed in
    /// `listen`. This allows grouping: multiple tasks can notify a
    /// logical event like `"config_changed"` and all handlers listening
    /// for that event will fire.
    pub listen: Vec<String>,
}

impl Handler {
    /// Create a new handler with no extra listen targets.
    pub fn new(name: impl Into<String>, module: impl Into<String>, args: Value) -> Self {
        Self {
            name: name.into(),
            module: module.into(),
            args,
            listen: Vec::new(),
        }
    }

    /// Add an additional event name this handler responds to.
    pub fn with_listen(mut self, event: impl Into<String>) -> Self {
        self.listen.push(event.into());
        self
    }

    /// Returns `true` if this handler should fire for the given
    /// notification name.
    pub fn matches(&self, notification: &str) -> bool {
        self.name == notification || self.listen.iter().any(|l| l == notification)
    }
}

// ---------------------------------------------------------------------------
// HandlerChain
// ---------------------------------------------------------------------------

/// An ordered collection of handlers with notification tracking.
///
/// The chain records which handlers have been notified (pending) and
/// ensures each fires at most once per flush cycle. After flushing,
/// the pending set is cleared so handlers can be re-notified in the
/// next cycle.
#[derive(Debug, Clone, Default)]
pub struct HandlerChain {
    /// Handlers in execution order.
    handlers: Vec<Handler>,
    /// Set of handler names that have been notified and are pending execution.
    pending: HashSet<String>,
    /// Name-to-index lookup for fast matching.
    name_index: HashMap<String, Vec<usize>>,
}

impl HandlerChain {
    /// Create a new, empty handler chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a handler chain from a list of handlers.
    pub fn from_handlers(handlers: Vec<Handler>) -> Self {
        let mut chain = Self::new();
        for h in handlers {
            chain.add(h);
        }
        chain
    }

    /// Add a handler to the chain.
    pub fn add(&mut self, handler: Handler) {
        let idx = self.handlers.len();

        // Index by name.
        self.name_index
            .entry(handler.name.clone())
            .or_default()
            .push(idx);

        // Index by listen targets.
        for listen in &handler.listen {
            self.name_index
                .entry(listen.clone())
                .or_default()
                .push(idx);
        }

        self.handlers.push(handler);
    }

    /// Notify one or more handlers by name.
    ///
    /// Marks matching handlers as pending. Calling this multiple times
    /// with the same name is idempotent — the handler still runs only
    /// once on flush.
    pub fn notify(&mut self, name: &str) {
        if let Some(indices) = self.name_index.get(name) {
            for &idx in indices {
                self.pending.insert(self.handlers[idx].name.clone());
            }
        }
    }

    /// Returns the list of handlers that are pending execution, in
    /// definition order.
    pub fn pending_handlers(&self) -> Vec<&Handler> {
        self.handlers
            .iter()
            .filter(|h| self.pending.contains(&h.name))
            .collect()
    }

    /// Drain all pending handlers, returning them in definition order.
    ///
    /// After this call, the pending set is empty. This is the "flush"
    /// operation that runs handlers at the end of a play.
    pub fn flush(&mut self) -> Vec<&Handler> {
        let result: Vec<&Handler> = self
            .handlers
            .iter()
            .filter(|h| self.pending.contains(&h.name))
            .collect();
        self.pending.clear();
        result
    }

    /// Force-flush: return pending handlers and clear immediately.
    ///
    /// Unlike [`flush`](Self::flush), this returns owned clones so the
    /// caller can execute them without borrowing the chain.
    pub fn force_flush(&mut self) -> Vec<Handler> {
        let result: Vec<Handler> = self
            .handlers
            .iter()
            .filter(|h| self.pending.contains(&h.name))
            .cloned()
            .collect();
        self.pending.clear();
        result
    }

    /// Returns `true` if any handler is pending execution.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Number of pending handlers.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Total number of handlers in the chain.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Returns `true` if the chain contains no handlers.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Return all handlers.
    pub fn handlers(&self) -> &[Handler] {
        &self.handlers
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_chain() -> HandlerChain {
        HandlerChain::from_handlers(vec![
            Handler::new("restart_agent", "command", json!({"cmd": "restart"})),
            Handler::new("flush_cache", "command", json!({"cmd": "flush"}))
                .with_listen("config_changed"),
            Handler::new("notify_monitor", "command", json!({"cmd": "ping"})),
        ])
    }

    #[test]
    fn notify_by_name() {
        let mut chain = test_chain();
        chain.notify("restart_agent");

        assert!(chain.has_pending());
        assert_eq!(chain.pending_count(), 1);

        let pending = chain.pending_handlers();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "restart_agent");
    }

    #[test]
    fn notify_by_listen_target() {
        let mut chain = test_chain();
        chain.notify("config_changed");

        let pending = chain.pending_handlers();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "flush_cache");
    }

    #[test]
    fn notify_is_idempotent() {
        let mut chain = test_chain();
        chain.notify("restart_agent");
        chain.notify("restart_agent");
        chain.notify("restart_agent");

        assert_eq!(chain.pending_count(), 1);
        let flushed = chain.flush();
        assert_eq!(flushed.len(), 1);
    }

    #[test]
    fn flush_clears_pending() {
        let mut chain = test_chain();
        chain.notify("restart_agent");
        chain.notify("flush_cache");

        let flushed = chain.flush();
        assert_eq!(flushed.len(), 2);
        assert!(!chain.has_pending());
        assert_eq!(chain.pending_count(), 0);
    }

    #[test]
    fn flush_preserves_definition_order() {
        let mut chain = test_chain();
        // Notify in reverse order.
        chain.notify("notify_monitor");
        chain.notify("restart_agent");

        let flushed = chain.flush();
        assert_eq!(flushed[0].name, "restart_agent");
        assert_eq!(flushed[1].name, "notify_monitor");
    }

    #[test]
    fn force_flush_returns_owned() {
        let mut chain = test_chain();
        chain.notify("restart_agent");

        let flushed = chain.force_flush();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].name, "restart_agent");
        assert!(!chain.has_pending());
    }

    #[test]
    fn no_pending_when_nothing_notified() {
        let chain = test_chain();
        assert!(!chain.has_pending());
        assert_eq!(chain.pending_count(), 0);
    }

    #[test]
    fn handler_matches() {
        let h = Handler::new("restart", "command", json!({}))
            .with_listen("config_changed")
            .with_listen("deploy_done");

        assert!(h.matches("restart"));
        assert!(h.matches("config_changed"));
        assert!(h.matches("deploy_done"));
        assert!(!h.matches("unknown"));
    }

    #[test]
    fn handler_serde_roundtrip() {
        let h = Handler::new("test", "command", json!({"x": 1})).with_listen("event_a");
        let json = serde_json::to_string(&h).unwrap();
        let back: Handler = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "test");
        assert_eq!(back.listen, vec!["event_a"]);
    }

    #[test]
    fn empty_chain() {
        let chain = HandlerChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }
}
