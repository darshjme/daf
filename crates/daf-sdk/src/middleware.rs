//! Agent middleware for cross-cutting concerns.
//!
//! Middleware intercepts messages before and after handler processing,
//! providing a clean separation for logging, metrics, retries, timeouts,
//! and authentication. The pattern is identical to HTTP middleware in
//! frameworks like Tower or Axum.
//!
//! # Middleware chain
//!
//! Middleware is composed into a [`MiddlewareStack`] that wraps a terminal
//! handler. Each middleware gets the message, can inspect or modify it,
//! then decides whether to call the next layer or short-circuit.
//!
//! ```text
//! Request ──▶ Logging ──▶ Metrics ──▶ Timeout ──▶ Handler ──▶ Response
//! ```
//!
//! # Examples
//!
//! ```rust,ignore
//! use daf_sdk::middleware::{MiddlewareStack, LoggingMiddleware, TimeoutMiddleware};
//! use std::time::Duration;
//!
//! let stack = MiddlewareStack::new(my_handler)
//!     .with(LoggingMiddleware::new())
//!     .with(TimeoutMiddleware::new(Duration::from_secs(30)));
//! ```

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use daf_core::error::{DafError, DafResult};
use daf_core::message::Message;
use daf_core::AgentContext;
use tracing::{debug, warn};

use crate::handler::MessageHandler;

// ---------------------------------------------------------------------------
// Middleware trait
// ---------------------------------------------------------------------------

/// A single middleware layer in the processing pipeline.
///
/// Middleware receives a message and a reference to the next handler in
/// the chain. It can:
/// - Pass the message through unchanged.
/// - Modify the message before passing it on.
/// - Short-circuit by returning a response directly.
/// - Inspect or modify the response from the next layer.
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    /// Process the message, optionally delegating to the next handler.
    async fn process(
        &self,
        msg: Message,
        ctx: &AgentContext,
        next: &dyn MessageHandler,
    ) -> DafResult<Option<Message>>;

    /// Human-readable name for logging and debugging.
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

// ---------------------------------------------------------------------------
// LoggingMiddleware
// ---------------------------------------------------------------------------

/// Logs every incoming message and outgoing response with tracing spans.
///
/// Captures: message ID, kind, source, target, payload size, and
/// processing duration.
#[derive(Debug, Clone)]
pub struct LoggingMiddleware {
    /// Whether to log message payloads (may contain sensitive data).
    log_payloads: bool,
}

impl LoggingMiddleware {
    /// Create a new logging middleware with payload logging disabled.
    pub fn new() -> Self {
        Self {
            log_payloads: false,
        }
    }

    /// Enable or disable payload logging.
    pub fn with_payload_logging(mut self, enabled: bool) -> Self {
        self.log_payloads = enabled;
        self
    }
}

impl Default for LoggingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn process(
        &self,
        msg: Message,
        ctx: &AgentContext,
        next: &dyn MessageHandler,
    ) -> DafResult<Option<Message>> {
        let start = Instant::now();
        let msg_id = msg.id;
        let msg_kind = msg.kind;
        let payload_size = msg.payload.len();

        debug!(
            msg_id = %msg_id,
            kind = %msg_kind,
            source = %msg.source,
            payload_bytes = payload_size,
            "middleware: incoming message"
        );

        if self.log_payloads {
            if let Ok(text) = msg.payload_str() {
                debug!(msg_id = %msg_id, payload = text, "middleware: message payload");
            }
        }

        let result = next.handle_message(msg, ctx).await;
        let elapsed = start.elapsed();

        match &result {
            Ok(Some(reply)) => {
                debug!(
                    msg_id = %msg_id,
                    reply_id = %reply.id,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "middleware: message handled with reply"
                );
            }
            Ok(None) => {
                debug!(
                    msg_id = %msg_id,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "middleware: message handled (no reply)"
                );
            }
            Err(e) => {
                warn!(
                    msg_id = %msg_id,
                    error = %e,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "middleware: message handling failed"
                );
            }
        }

        result
    }

    fn name(&self) -> &str {
        "LoggingMiddleware"
    }
}

// ---------------------------------------------------------------------------
// MetricsMiddleware
// ---------------------------------------------------------------------------

/// Tracks message counts, success/failure rates, and latency percentiles.
///
/// Metrics are stored in atomic counters and can be read at any time via
/// the [`snapshot`](Self::snapshot) method.
#[derive(Debug)]
pub struct MetricsMiddleware {
    total_messages: AtomicU64,
    successful: AtomicU64,
    failed: AtomicU64,
    total_latency_us: AtomicU64,
}

impl MetricsMiddleware {
    /// Create a new metrics middleware with zeroed counters.
    pub fn new() -> Self {
        Self {
            total_messages: AtomicU64::new(0),
            successful: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
        }
    }

    /// Snapshot the current metrics.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.total_messages.load(Ordering::Relaxed);
        let success = self.successful.load(Ordering::Relaxed);
        let fail = self.failed.load(Ordering::Relaxed);
        let latency_us = self.total_latency_us.load(Ordering::Relaxed);

        MetricsSnapshot {
            total_messages: total,
            successful: success,
            failed: fail,
            avg_latency: if total > 0 {
                Duration::from_micros(latency_us / total)
            } else {
                Duration::ZERO
            },
        }
    }
}

impl Default for MetricsMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

/// Point-in-time metrics snapshot.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    /// Total messages processed.
    pub total_messages: u64,
    /// Messages that completed successfully.
    pub successful: u64,
    /// Messages that resulted in an error.
    pub failed: u64,
    /// Average processing latency.
    pub avg_latency: Duration,
}

impl fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "messages={} ok={} err={} avg_latency={:?}",
            self.total_messages, self.successful, self.failed, self.avg_latency,
        )
    }
}

#[async_trait]
impl Middleware for MetricsMiddleware {
    async fn process(
        &self,
        msg: Message,
        ctx: &AgentContext,
        next: &dyn MessageHandler,
    ) -> DafResult<Option<Message>> {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();

        let result = next.handle_message(msg, ctx).await;
        let elapsed = start.elapsed();

        self.total_latency_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);

        match &result {
            Ok(_) => {
                self.successful.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
            }
        }

        result
    }

    fn name(&self) -> &str {
        "MetricsMiddleware"
    }
}

// ---------------------------------------------------------------------------
// RetryMiddleware
// ---------------------------------------------------------------------------

/// Retries failed handler invocations with exponential backoff.
///
/// Only retries errors where [`DafError::is_retryable`] returns `true`.
/// Non-retryable errors are propagated immediately.
#[derive(Debug, Clone)]
pub struct RetryMiddleware {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Initial backoff duration (doubles on each retry).
    pub initial_backoff: Duration,
    /// Maximum backoff duration cap.
    pub max_backoff: Duration,
}

impl RetryMiddleware {
    /// Create a retry middleware with the given max retries.
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(30),
        }
    }

    /// Set the initial backoff duration.
    pub fn with_initial_backoff(mut self, backoff: Duration) -> Self {
        self.initial_backoff = backoff;
        self
    }

    /// Set the maximum backoff cap.
    pub fn with_max_backoff(mut self, max: Duration) -> Self {
        self.max_backoff = max;
        self
    }
}

#[async_trait]
impl Middleware for RetryMiddleware {
    async fn process(
        &self,
        msg: Message,
        ctx: &AgentContext,
        next: &dyn MessageHandler,
    ) -> DafResult<Option<Message>> {
        let mut last_error = None;
        let mut backoff = self.initial_backoff;

        for attempt in 0..=self.max_retries {
            match next.handle_message(msg.clone(), ctx).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if !e.is_retryable() || attempt == self.max_retries {
                        return Err(e);
                    }

                    warn!(
                        attempt = attempt + 1,
                        max = self.max_retries,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %e,
                        "middleware: retrying after error"
                    );

                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(self.max_backoff);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            DafError::Internal("retry exhausted with no error".into())
        }))
    }

    fn name(&self) -> &str {
        "RetryMiddleware"
    }
}

// ---------------------------------------------------------------------------
// TimeoutMiddleware
// ---------------------------------------------------------------------------

/// Enforces a per-message processing deadline.
///
/// If the handler does not complete within the configured timeout, a
/// [`DafError::TimeoutError`] is returned.
#[derive(Debug, Clone)]
pub struct TimeoutMiddleware {
    /// Maximum time to wait for handler completion.
    pub timeout: Duration,
}

impl TimeoutMiddleware {
    /// Create a timeout middleware with the given deadline.
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

#[async_trait]
impl Middleware for TimeoutMiddleware {
    async fn process(
        &self,
        msg: Message,
        ctx: &AgentContext,
        next: &dyn MessageHandler,
    ) -> DafResult<Option<Message>> {
        match tokio::time::timeout(self.timeout, next.handle_message(msg, ctx)).await {
            Ok(result) => result,
            Err(_) => Err(DafError::TimeoutError {
                operation: "message handler".into(),
                duration: self.timeout,
            }),
        }
    }

    fn name(&self) -> &str {
        "TimeoutMiddleware"
    }
}

// ---------------------------------------------------------------------------
// AuthMiddleware
// ---------------------------------------------------------------------------

/// Verifies that messages carry a valid authorization header.
///
/// This is a simple header-based check. For production, integrate with
/// the vault crate for cryptographic signature verification.
#[derive(Debug, Clone)]
pub struct AuthMiddleware {
    /// Header key to check for auth tokens.
    header_key: String,
    /// Set of valid tokens. In production, replace with a token verifier.
    valid_tokens: Vec<String>,
}

impl AuthMiddleware {
    /// Create an auth middleware checking the given header for valid tokens.
    pub fn new(
        header_key: impl Into<String>,
        valid_tokens: Vec<String>,
    ) -> Self {
        Self {
            header_key: header_key.into(),
            valid_tokens,
        }
    }
}

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn process(
        &self,
        msg: Message,
        ctx: &AgentContext,
        next: &dyn MessageHandler,
    ) -> DafResult<Option<Message>> {
        match msg.headers.get(&self.header_key) {
            Some(token) if self.valid_tokens.contains(token) => {
                next.handle_message(msg, ctx).await
            }
            Some(_) => Err(DafError::Unauthorized {
                message: format!(
                    "invalid auth token in header '{}'",
                    self.header_key
                ),
            }),
            None => Err(DafError::Unauthorized {
                message: format!(
                    "missing auth header '{}'",
                    self.header_key
                ),
            }),
        }
    }

    fn name(&self) -> &str {
        "AuthMiddleware"
    }
}

// ---------------------------------------------------------------------------
// MiddlewareStack
// ---------------------------------------------------------------------------

/// Composes middleware layers around a terminal handler.
///
/// Middleware is applied in LIFO order: the last added middleware is the
/// outermost layer (first to see the message).
pub struct MiddlewareStack {
    handler: Arc<dyn MessageHandler>,
    layers: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareStack {
    /// Create a stack wrapping the given terminal handler.
    pub fn new(handler: impl MessageHandler) -> Self {
        Self {
            handler: Arc::new(handler),
            layers: Vec::new(),
        }
    }

    /// Add a middleware layer. Later layers wrap earlier ones.
    pub fn with(mut self, middleware: impl Middleware) -> Self {
        self.layers.push(Arc::new(middleware));
        self
    }

    /// Number of middleware layers.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

/// Internal adapter that makes a middleware + next handler look like a
/// single `MessageHandler`.
struct MiddlewareAdapter {
    middleware: Arc<dyn Middleware>,
    next: Arc<dyn MessageHandler>,
}

#[async_trait]
impl MessageHandler for MiddlewareAdapter {
    async fn handle_message(
        &self,
        msg: Message,
        ctx: &AgentContext,
    ) -> DafResult<Option<Message>> {
        self.middleware.process(msg, ctx, self.next.as_ref()).await
    }

    fn name(&self) -> &str {
        "MiddlewareAdapter"
    }
}

#[async_trait]
impl MessageHandler for MiddlewareStack {
    async fn handle_message(
        &self,
        msg: Message,
        ctx: &AgentContext,
    ) -> DafResult<Option<Message>> {
        // Build the chain from inside out: handler ← layer[0] ← layer[1] ← ...
        let mut current: Arc<dyn MessageHandler> = Arc::clone(&self.handler);

        for layer in &self.layers {
            current = Arc::new(MiddlewareAdapter {
                middleware: Arc::clone(layer),
                next: current,
            });
        }

        current.handle_message(msg, ctx).await
    }

    fn name(&self) -> &str {
        "MiddlewareStack"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use daf_core::message::MessageKind;
    use daf_core::AgentId;
    use uuid::Uuid;

    fn test_ctx() -> AgentContext {
        AgentContext::new(AgentId::new(), Uuid::now_v7())
    }

    fn test_msg() -> Message {
        Message::builder(MessageKind::Request, AgentId::new())
            .payload(Bytes::from_static(b"hello"))
            .build()
    }

    fn authed_msg(token: &str) -> Message {
        Message::builder(MessageKind::Request, AgentId::new())
            .header("authorization", token)
            .payload(Bytes::from_static(b"hello"))
            .build()
    }

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

    struct FailHandler;

    #[async_trait]
    impl MessageHandler for FailHandler {
        async fn handle_message(
            &self,
            _msg: Message,
            _ctx: &AgentContext,
        ) -> DafResult<Option<Message>> {
            Err(DafError::transport("connection reset", true))
        }

        fn name(&self) -> &str {
            "fail"
        }
    }

    #[tokio::test]
    async fn logging_middleware_passes_through() {
        let middleware = LoggingMiddleware::new();
        let handler = EchoHandler;
        let ctx = test_ctx();
        let msg = test_msg();

        let result = middleware.process(msg, &ctx, &handler).await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn metrics_middleware_tracks_counts() {
        let metrics = MetricsMiddleware::new();
        let handler = EchoHandler;
        let ctx = test_ctx();

        for _ in 0..5 {
            metrics.process(test_msg(), &ctx, &handler).await.unwrap();
        }

        let snap = metrics.snapshot();
        assert_eq!(snap.total_messages, 5);
        assert_eq!(snap.successful, 5);
        assert_eq!(snap.failed, 0);
        assert!(snap.avg_latency < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn metrics_middleware_tracks_failures() {
        let metrics = MetricsMiddleware::new();
        let handler = FailHandler;
        let ctx = test_ctx();

        let _ = metrics.process(test_msg(), &ctx, &handler).await;
        let _ = metrics.process(test_msg(), &ctx, &handler).await;

        let snap = metrics.snapshot();
        assert_eq!(snap.total_messages, 2);
        assert_eq!(snap.failed, 2);
        assert_eq!(snap.successful, 0);
    }

    #[tokio::test]
    async fn timeout_middleware_passes_fast_handler() {
        let middleware = TimeoutMiddleware::new(Duration::from_secs(5));
        let handler = EchoHandler;
        let ctx = test_ctx();

        let result = middleware.process(test_msg(), &ctx, &handler).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn auth_middleware_accepts_valid_token() {
        let middleware = AuthMiddleware::new(
            "authorization",
            vec!["secret-token".into()],
        );
        let handler = EchoHandler;
        let ctx = test_ctx();
        let msg = authed_msg("secret-token");

        let result = middleware.process(msg, &ctx, &handler).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn auth_middleware_rejects_invalid_token() {
        let middleware = AuthMiddleware::new(
            "authorization",
            vec!["secret-token".into()],
        );
        let handler = EchoHandler;
        let ctx = test_ctx();
        let msg = authed_msg("wrong-token");

        let result = middleware.process(msg, &ctx, &handler).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid"));
    }

    #[tokio::test]
    async fn auth_middleware_rejects_missing_header() {
        let middleware = AuthMiddleware::new(
            "authorization",
            vec!["secret-token".into()],
        );
        let handler = EchoHandler;
        let ctx = test_ctx();
        let msg = test_msg(); // no auth header

        let result = middleware.process(msg, &ctx, &handler).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[tokio::test]
    async fn middleware_stack_composes_layers() {
        let stack = MiddlewareStack::new(EchoHandler)
            .with(LoggingMiddleware::new())
            .with(TimeoutMiddleware::new(Duration::from_secs(5)));

        let ctx = test_ctx();
        let msg = test_msg();

        let result = stack.handle_message(msg, &ctx).await;
        assert!(result.is_ok());
        assert_eq!(stack.layer_count(), 2);
    }

    #[tokio::test]
    async fn retry_middleware_retries_on_retryable_error() {
        // Use a short backoff for testing.
        let middleware = RetryMiddleware::new(2)
            .with_initial_backoff(Duration::from_millis(1))
            .with_max_backoff(Duration::from_millis(10));
        let handler = FailHandler; // always fails with retryable error
        let ctx = test_ctx();

        let start = Instant::now();
        let result = middleware.process(test_msg(), &ctx, &handler).await;
        let elapsed = start.elapsed();

        // Should fail after exhausting retries.
        assert!(result.is_err());
        // Should have taken at least some backoff time.
        assert!(elapsed >= Duration::from_millis(2));
    }

    #[test]
    fn metrics_snapshot_display() {
        let snap = MetricsSnapshot {
            total_messages: 100,
            successful: 95,
            failed: 5,
            avg_latency: Duration::from_micros(500),
        };
        let display = snap.to_string();
        assert!(display.contains("100"));
        assert!(display.contains("95"));
    }
}
