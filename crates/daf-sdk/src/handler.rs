//! Message, task, and event handler abstractions.
//!
//! Handlers are the primary extension point for agent developers. Instead
//! of implementing the full [`Agent`](daf_core::Agent) trait, developers
//! can compose small, focused handlers and wire them together through the
//! [`HandlerRegistry`].
//!
//! # Handler traits
//!
//! - [`MessageHandler`] — processes a single inbound message, optionally
//!   producing a reply.
//! - [`TaskHandler`] — executes a task specification and returns a result.
//! - [`EventHandler`] — reacts to system or domain events with no return value.
//!
//! # Composition
//!
//! Handlers can be composed into chains. The [`HandlerChain`] runs a
//! sequence of message handlers where each can short-circuit or pass
//! through to the next, similar to middleware in web frameworks.
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::handler::{MessageHandler, FnMessageHandler};
//! use daf_core::{Message, AgentContext, DafResult};
//!
//! // Create a handler from a closure.
//! let echo = FnMessageHandler::new(|msg, _ctx| {
//!     Box::pin(async move { Ok(Some(msg)) })
//! });
//! ```

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use daf_core::error::DafResult;
use daf_core::message::Message;
use daf_core::AgentContext;

use crate::task_types::{Event, SdkTaskResult, SdkTaskSpec};

// ---------------------------------------------------------------------------
// MessageHandler
// ---------------------------------------------------------------------------

/// Processes a single inbound message.
///
/// Returning `Ok(Some(reply))` sends a reply back to the sender.
/// Returning `Ok(None)` acknowledges the message without replying.
#[async_trait]
pub trait MessageHandler: Send + Sync + 'static {
    /// Handle an inbound message within the given agent context.
    async fn handle_message(
        &self,
        msg: Message,
        ctx: &AgentContext,
    ) -> DafResult<Option<Message>>;

    /// Human-readable name for this handler (used in logs and metrics).
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

// ---------------------------------------------------------------------------
// TaskHandler
// ---------------------------------------------------------------------------

/// Executes a task specification and produces a result.
#[async_trait]
pub trait TaskHandler: Send + Sync + 'static {
    /// Execute the given task.
    async fn handle_task(
        &self,
        task: SdkTaskSpec,
        ctx: &AgentContext,
    ) -> DafResult<SdkTaskResult>;

    /// Human-readable name for this handler.
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

// ---------------------------------------------------------------------------
// EventHandler
// ---------------------------------------------------------------------------

/// Reacts to system or domain events. Fire-and-forget; no return value
/// beyond success/failure.
#[async_trait]
pub trait EventHandler: Send + Sync + 'static {
    /// Handle the event.
    async fn handle_event(&self, event: Event, ctx: &AgentContext) -> DafResult<()>;

    /// Human-readable name for this handler.
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

// ---------------------------------------------------------------------------
// FnMessageHandler — closure adapter
// ---------------------------------------------------------------------------

/// Wraps an async closure as a [`MessageHandler`].
///
/// This is the most ergonomic way to create simple handlers without defining
/// a separate struct.
pub struct FnMessageHandler<F>
where
    F: Fn(Message, AgentContext) -> Pin<Box<dyn Future<Output = DafResult<Option<Message>>> + Send>>
        + Send
        + Sync
        + 'static,
{
    func: F,
    handler_name: String,
}

impl<F> FnMessageHandler<F>
where
    F: Fn(Message, AgentContext) -> Pin<Box<dyn Future<Output = DafResult<Option<Message>>> + Send>>
        + Send
        + Sync
        + 'static,
{
    /// Create a new closure-based message handler.
    pub fn new(func: F) -> Self {
        Self {
            func,
            handler_name: "FnMessageHandler".into(),
        }
    }

    /// Set a custom name for this handler.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.handler_name = name.into();
        self
    }
}

#[async_trait]
impl<F> MessageHandler for FnMessageHandler<F>
where
    F: Fn(Message, AgentContext) -> Pin<Box<dyn Future<Output = DafResult<Option<Message>>> + Send>>
        + Send
        + Sync
        + 'static,
{
    async fn handle_message(
        &self,
        msg: Message,
        ctx: &AgentContext,
    ) -> DafResult<Option<Message>> {
        (self.func)(msg, ctx.clone()).await
    }

    fn name(&self) -> &str {
        &self.handler_name
    }
}

// ---------------------------------------------------------------------------
// FnTaskHandler — closure adapter
// ---------------------------------------------------------------------------

/// Wraps an async closure as a [`TaskHandler`].
pub struct FnTaskHandler<F>
where
    F: Fn(SdkTaskSpec, AgentContext) -> Pin<Box<dyn Future<Output = DafResult<SdkTaskResult>> + Send>>
        + Send
        + Sync
        + 'static,
{
    func: F,
    handler_name: String,
}

impl<F> FnTaskHandler<F>
where
    F: Fn(SdkTaskSpec, AgentContext) -> Pin<Box<dyn Future<Output = DafResult<SdkTaskResult>> + Send>>
        + Send
        + Sync
        + 'static,
{
    /// Create a new closure-based task handler.
    pub fn new(func: F) -> Self {
        Self {
            func,
            handler_name: "FnTaskHandler".into(),
        }
    }

    /// Set a custom name for this handler.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.handler_name = name.into();
        self
    }
}

#[async_trait]
impl<F> TaskHandler for FnTaskHandler<F>
where
    F: Fn(SdkTaskSpec, AgentContext) -> Pin<Box<dyn Future<Output = DafResult<SdkTaskResult>> + Send>>
        + Send
        + Sync
        + 'static,
{
    async fn handle_task(
        &self,
        task: SdkTaskSpec,
        ctx: &AgentContext,
    ) -> DafResult<SdkTaskResult> {
        (self.func)(task, ctx.clone()).await
    }

    fn name(&self) -> &str {
        &self.handler_name
    }
}

// ---------------------------------------------------------------------------
// HandlerRegistry
// ---------------------------------------------------------------------------

/// Routes messages to handlers based on message type or pattern.
///
/// The registry maps string patterns (message headers, kinds, or custom
/// discriminators) to handlers. When a message arrives, the registry
/// finds the first matching handler and invokes it.
///
/// # Examples
///
/// ```rust,ignore
/// use daf_sdk::handler::HandlerRegistry;
///
/// let mut registry = HandlerRegistry::new();
/// registry.register_message_handler("command:deploy", deploy_handler);
/// registry.register_message_handler("event:*", catch_all_handler);
/// ```
pub struct HandlerRegistry {
    message_handlers: Vec<(String, Arc<dyn MessageHandler>)>,
    task_handlers: Vec<(String, Arc<dyn TaskHandler>)>,
    event_handlers: Vec<(String, Arc<dyn EventHandler>)>,
    default_message_handler: Option<Arc<dyn MessageHandler>>,
    default_task_handler: Option<Arc<dyn TaskHandler>>,
}

impl HandlerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            message_handlers: Vec::new(),
            task_handlers: Vec::new(),
            event_handlers: Vec::new(),
            default_message_handler: None,
            default_task_handler: None,
        }
    }

    /// Register a message handler for the given pattern.
    ///
    /// Patterns are matched in registration order. The first match wins.
    /// Use `"*"` for a catch-all pattern.
    pub fn register_message_handler(
        &mut self,
        pattern: impl Into<String>,
        handler: impl MessageHandler,
    ) {
        self.message_handlers
            .push((pattern.into(), Arc::new(handler)));
    }

    /// Register a task handler for the given capability name.
    pub fn register_task_handler(
        &mut self,
        capability: impl Into<String>,
        handler: impl TaskHandler,
    ) {
        self.task_handlers
            .push((capability.into(), Arc::new(handler)));
    }

    /// Register an event handler for the given event type pattern.
    pub fn register_event_handler(
        &mut self,
        pattern: impl Into<String>,
        handler: impl EventHandler,
    ) {
        self.event_handlers
            .push((pattern.into(), Arc::new(handler)));
    }

    /// Set a fallback handler for messages that don't match any pattern.
    pub fn set_default_message_handler(&mut self, handler: impl MessageHandler) {
        self.default_message_handler = Some(Arc::new(handler));
    }

    /// Set a fallback handler for tasks that don't match any capability.
    pub fn set_default_task_handler(&mut self, handler: impl TaskHandler) {
        self.default_task_handler = Some(Arc::new(handler));
    }

    /// Find the first message handler matching the given key.
    pub fn find_message_handler(&self, key: &str) -> Option<Arc<dyn MessageHandler>> {
        for (pattern, handler) in &self.message_handlers {
            if pattern_matches(pattern, key) {
                return Some(Arc::clone(handler));
            }
        }
        self.default_message_handler.clone()
    }

    /// Find the first task handler matching the given capability.
    pub fn find_task_handler(&self, capability: &str) -> Option<Arc<dyn TaskHandler>> {
        for (pattern, handler) in &self.task_handlers {
            if pattern_matches(pattern, capability) {
                return Some(Arc::clone(handler));
            }
        }
        self.default_task_handler.clone()
    }

    /// Find all event handlers matching the given event type.
    pub fn find_event_handlers(&self, event_type: &str) -> Vec<Arc<dyn EventHandler>> {
        self.event_handlers
            .iter()
            .filter(|(pattern, _)| pattern_matches(pattern, event_type))
            .map(|(_, handler)| Arc::clone(handler))
            .collect()
    }

    /// Number of registered handlers across all categories.
    pub fn handler_count(&self) -> usize {
        self.message_handlers.len()
            + self.task_handlers.len()
            + self.event_handlers.len()
    }
}

impl Default for HandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for HandlerRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandlerRegistry")
            .field("message_patterns", &self.message_handlers.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>())
            .field("task_patterns", &self.task_handlers.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>())
            .field("event_patterns", &self.event_handlers.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// HandlerChain
// ---------------------------------------------------------------------------

/// Composes multiple message handlers into a pipeline.
///
/// Handlers are invoked in order. If a handler returns `Some(reply)`, the
/// chain short-circuits and returns that reply. If a handler returns `None`,
/// execution continues to the next handler.
pub struct HandlerChain {
    handlers: Vec<Arc<dyn MessageHandler>>,
    chain_name: String,
}

impl HandlerChain {
    /// Create a new empty chain.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            handlers: Vec::new(),
            chain_name: name.into(),
        }
    }

    /// Append a handler to the chain.
    pub fn then(mut self, handler: impl MessageHandler) -> Self {
        self.handlers.push(Arc::new(handler));
        self
    }

    /// Number of handlers in the chain.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Whether the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

#[async_trait]
impl MessageHandler for HandlerChain {
    async fn handle_message(
        &self,
        msg: Message,
        ctx: &AgentContext,
    ) -> DafResult<Option<Message>> {
        let current_msg = msg;

        for handler in &self.handlers {
            match handler.handle_message(current_msg.clone(), ctx).await? {
                Some(reply) => return Ok(Some(reply)),
                None => {
                    // Continue with the same message to the next handler.
                }
            }
        }

        Ok(None)
    }

    fn name(&self) -> &str {
        &self.chain_name
    }
}

// ---------------------------------------------------------------------------
// Pattern matching helper
// ---------------------------------------------------------------------------

/// Simple pattern matching: supports exact match and `*` wildcard suffix.
///
/// - `"*"` matches everything.
/// - `"command:*"` matches any string starting with `"command:"`.
/// - `"event:deploy"` matches only `"event:deploy"` exactly.
fn pattern_matches(pattern: &str, input: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return input.starts_with(prefix);
    }
    pattern == input
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use daf_core::AgentId;
    use daf_core::message::MessageKind;
    use uuid::Uuid;

    fn test_ctx() -> AgentContext {
        AgentContext::new(AgentId::new(), Uuid::now_v7())
    }

    fn test_msg() -> Message {
        Message::builder(MessageKind::Request, AgentId::new())
            .payload(Bytes::from_static(b"test"))
            .build()
    }

    /// A simple echo handler for testing.
    struct EchoHandler;

    #[async_trait]
    impl MessageHandler for EchoHandler {
        async fn handle_message(
            &self,
            msg: Message,
            _ctx: &AgentContext,
        ) -> DafResult<Option<Message>> {
            Ok(Some(msg))
        }

        fn name(&self) -> &str {
            "echo"
        }
    }

    /// A handler that always returns None (pass-through).
    struct PassHandler;

    #[async_trait]
    impl MessageHandler for PassHandler {
        async fn handle_message(
            &self,
            _msg: Message,
            _ctx: &AgentContext,
        ) -> DafResult<Option<Message>> {
            Ok(None)
        }

        fn name(&self) -> &str {
            "pass"
        }
    }

    #[test]
    fn pattern_matching_exact() {
        assert!(pattern_matches("event:deploy", "event:deploy"));
        assert!(!pattern_matches("event:deploy", "event:rollback"));
    }

    #[test]
    fn pattern_matching_wildcard() {
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("command:*", "command:deploy"));
        assert!(pattern_matches("command:*", "command:rollback"));
        assert!(!pattern_matches("command:*", "event:deploy"));
    }

    #[test]
    fn registry_find_message_handler() {
        let mut reg = HandlerRegistry::new();
        reg.register_message_handler("command:deploy", EchoHandler);
        reg.register_message_handler("event:*", PassHandler);

        assert!(reg.find_message_handler("command:deploy").is_some());
        assert!(reg.find_message_handler("event:anything").is_some());
        assert!(reg.find_message_handler("unknown").is_none());
    }

    #[test]
    fn registry_default_handler() {
        let mut reg = HandlerRegistry::new();
        reg.set_default_message_handler(EchoHandler);

        // No specific pattern matches, but the default should.
        assert!(reg.find_message_handler("anything").is_some());
    }

    #[test]
    fn registry_handler_count() {
        let mut reg = HandlerRegistry::new();
        assert_eq!(reg.handler_count(), 0);

        reg.register_message_handler("a", EchoHandler);
        reg.register_message_handler("b", PassHandler);
        assert_eq!(reg.handler_count(), 2);
    }

    #[tokio::test]
    async fn handler_chain_short_circuits() {
        let chain = HandlerChain::new("test-chain")
            .then(PassHandler)
            .then(EchoHandler)
            .then(PassHandler); // should never reach this

        let ctx = test_ctx();
        let msg = test_msg();

        let result = chain.handle_message(msg.clone(), &ctx).await.unwrap();
        assert!(result.is_some(), "chain should short-circuit at EchoHandler");
    }

    #[tokio::test]
    async fn handler_chain_all_pass() {
        let chain = HandlerChain::new("all-pass")
            .then(PassHandler)
            .then(PassHandler);

        let ctx = test_ctx();
        let msg = test_msg();

        let result = chain.handle_message(msg, &ctx).await.unwrap();
        assert!(result.is_none(), "all handlers passed, chain returns None");
    }

    #[test]
    fn handler_chain_length() {
        let chain = HandlerChain::new("test")
            .then(EchoHandler)
            .then(PassHandler);
        assert_eq!(chain.len(), 2);
        assert!(!chain.is_empty());
    }

    #[test]
    fn empty_chain() {
        let chain = HandlerChain::new("empty");
        assert_eq!(chain.len(), 0);
        assert!(chain.is_empty());
    }

    #[tokio::test]
    async fn echo_handler_returns_same_message() {
        let handler = EchoHandler;
        let ctx = test_ctx();
        let msg = test_msg();
        let msg_id = msg.id;

        let result = handler.handle_message(msg, &ctx).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, msg_id);
    }
}
