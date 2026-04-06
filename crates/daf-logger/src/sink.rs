//! Log sink pipeline.
//!
//! A sink receives log entries and does something with them — filter, transform,
//! fan out to multiple destinations, or buffer for batch writes. Sinks compose
//! into pipelines:
//!
//! ```text
//! source → FilterSink → TransformSink → FanOutSink
//!                                          ├─→ FileWriter
//!                                          ├─→ KBExtractor
//!                                          └─→ EpisodeRecorder
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use tracing::debug;

use crate::entry::{LogEntry, LogLevel};
use crate::error::LoggerError;

// ---------------------------------------------------------------------------
// LogSink trait
// ---------------------------------------------------------------------------

/// Async processor for log entries. Implement this to add custom pipeline
/// stages (metrics, alerting, external forwarding, etc.).
#[async_trait]
pub trait LogSink: Send + Sync + 'static {
    /// Process a single log entry.
    async fn process(&self, entry: &LogEntry) -> Result<(), LoggerError>;

    /// Flush any internal state (called periodically and at shutdown).
    async fn flush(&self) -> Result<(), LoggerError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FilterSink
// ---------------------------------------------------------------------------

/// Predicate-based filter. Only forwards entries that pass all predicates.
pub struct FilterSink {
    /// Minimum log level (entries below this are dropped).
    min_level: Option<LogLevel>,
    /// Only forward entries from these agents (empty = allow all).
    allowed_agents: Vec<daf_core::AgentId>,
    /// Only forward entries with at least one of these tags (empty = allow all).
    required_tags: Vec<String>,
    /// Downstream sink to forward matching entries to.
    downstream: Arc<dyn LogSink>,
}

impl FilterSink {
    /// Create a filter that forwards everything to `downstream`.
    pub fn new(downstream: Arc<dyn LogSink>) -> Self {
        Self {
            min_level: None,
            allowed_agents: Vec::new(),
            required_tags: Vec::new(),
            downstream,
        }
    }

    /// Set a minimum log level.
    pub fn with_min_level(mut self, level: LogLevel) -> Self {
        self.min_level = Some(level);
        self
    }

    /// Only allow entries from specific agents.
    pub fn with_allowed_agents(mut self, agents: Vec<daf_core::AgentId>) -> Self {
        self.allowed_agents = agents;
        self
    }

    /// Only allow entries that have at least one of these tags.
    pub fn with_required_tags(mut self, tags: Vec<String>) -> Self {
        self.required_tags = tags;
        self
    }

    fn passes(&self, entry: &LogEntry) -> bool {
        if let Some(min) = self.min_level {
            if entry.level < min {
                return false;
            }
        }
        if !self.allowed_agents.is_empty()
            && !self.allowed_agents.contains(&entry.source_agent)
        {
            return false;
        }
        if !self.required_tags.is_empty()
            && !entry.tags.iter().any(|t| self.required_tags.contains(t))
        {
            return false;
        }
        true
    }
}

#[async_trait]
impl LogSink for FilterSink {
    async fn process(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        if self.passes(entry) {
            self.downstream.process(entry).await
        } else {
            Ok(())
        }
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        self.downstream.flush().await
    }
}

// ---------------------------------------------------------------------------
// TransformSink
// ---------------------------------------------------------------------------

/// A transform function that can modify entries before forwarding.
pub type TransformFn = Box<dyn Fn(LogEntry) -> LogEntry + Send + Sync>;

/// Applies a transformation to each entry before forwarding downstream.
///
/// Common uses: redact secrets from content, enrich metadata, normalize tags.
pub struct TransformSink {
    transform: TransformFn,
    downstream: Arc<dyn LogSink>,
}

impl TransformSink {
    /// Create a transform sink with the given function and downstream.
    pub fn new(transform: TransformFn, downstream: Arc<dyn LogSink>) -> Self {
        Self {
            transform,
            downstream,
        }
    }

    /// Create a sink that redacts common secret patterns from content.
    pub fn redact_secrets(downstream: Arc<dyn LogSink>) -> Self {
        let patterns = [
            "password",
            "secret",
            "token",
            "api_key",
            "apikey",
            "api-key",
            "authorization",
            "credential",
        ];

        let transform: TransformFn = Box::new(move |mut entry| {
            if let Value::String(ref s) = entry.content {
                let lower = s.to_lowercase();
                for pat in &patterns {
                    if lower.contains(pat) {
                        entry.content =
                            Value::String("[REDACTED: contains sensitive content]".to_string());
                        entry.tags.push("redacted".to_string());
                        break;
                    }
                }
            }
            // Also redact from metadata.
            if let Value::Object(map) = &entry.metadata {
                let mut new_map = map.clone();
                let mut changed = false;
                for key in map.keys() {
                    let lower_key = key.to_lowercase();
                    for pat in &patterns {
                        if lower_key.contains(pat) {
                            new_map.insert(key.clone(), Value::String("[REDACTED]".to_string()));
                            changed = true;
                            break;
                        }
                    }
                }
                if changed {
                    entry.metadata = Value::Object(new_map);
                }
            }
            entry
        });

        Self::new(transform, downstream)
    }

    /// Create a sink that enriches metadata with a static key-value pair.
    pub fn enrich(key: String, value: Value, downstream: Arc<dyn LogSink>) -> Self {
        let transform: TransformFn = Box::new(move |mut entry| {
            if let Value::Object(ref mut map) = entry.metadata {
                map.insert(key.clone(), value.clone());
            }
            entry
        });

        Self::new(transform, downstream)
    }
}

#[async_trait]
impl LogSink for TransformSink {
    async fn process(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        let transformed = (self.transform)(entry.clone());
        self.downstream.process(&transformed).await
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        self.downstream.flush().await
    }
}

// ---------------------------------------------------------------------------
// FanOutSink
// ---------------------------------------------------------------------------

/// Sends each entry to multiple downstream sinks in parallel.
pub struct FanOutSink {
    sinks: Vec<Arc<dyn LogSink>>,
}

impl FanOutSink {
    /// Create a fan-out with the given downstream sinks.
    pub fn new(sinks: Vec<Arc<dyn LogSink>>) -> Self {
        Self { sinks }
    }

    /// Add a downstream sink.
    pub fn add_sink(&mut self, sink: Arc<dyn LogSink>) {
        self.sinks.push(sink);
    }
}

#[async_trait]
impl LogSink for FanOutSink {
    async fn process(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        // Process concurrently across all sinks.
        let futs: Vec<_> = self.sinks.iter().map(|s| s.process(entry)).collect();
        let results = futures::future::join_all(futs).await;

        // Collect errors but don't fail the whole pipeline for one bad sink.
        let errors: Vec<_> = results.into_iter().filter_map(|r| r.err()).collect();
        if !errors.is_empty() {
            tracing::warn!(count = errors.len(), "FanOutSink: some downstream sinks failed");
            // Return the first error.
            return Err(errors.into_iter().next().unwrap());
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        for sink in &self.sinks {
            sink.flush().await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// BufferedSink
// ---------------------------------------------------------------------------

/// Accumulates entries in a buffer and flushes them as a batch when the buffer
/// is full or when `flush()` is called explicitly.
pub struct BufferedSink {
    buffer: Arc<Mutex<Vec<LogEntry>>>,
    capacity: usize,
    downstream: Arc<dyn LogSink>,
}

impl BufferedSink {
    /// Create a buffered sink with the given batch size.
    pub fn new(capacity: usize, downstream: Arc<dyn LogSink>) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::with_capacity(capacity))),
            capacity,
            downstream,
        }
    }

    /// Drain the buffer and process all entries downstream.
    async fn drain(&self) -> Result<(), LoggerError> {
        let entries: Vec<LogEntry> = {
            let mut buf = self.buffer.lock();
            std::mem::take(&mut *buf)
        };

        if entries.is_empty() {
            return Ok(());
        }

        debug!(count = entries.len(), "BufferedSink draining batch");

        for entry in &entries {
            self.downstream.process(entry).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl LogSink for BufferedSink {
    async fn process(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        let should_flush = {
            let mut buf = self.buffer.lock();
            buf.push(entry.clone());
            buf.len() >= self.capacity
        };

        if should_flush {
            self.drain().await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), LoggerError> {
        self.drain().await?;
        self.downstream.flush().await
    }
}

// ---------------------------------------------------------------------------
// CollectorSink (for testing)
// ---------------------------------------------------------------------------

/// Simple sink that collects all entries. Useful for testing pipelines.
pub struct CollectorSink {
    entries: Arc<Mutex<Vec<LogEntry>>>,
}

impl CollectorSink {
    /// Create a new collector.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get all collected entries.
    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.lock().clone()
    }

    /// Number of collected entries.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether the collector is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl Default for CollectorSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LogSink for CollectorSink {
    async fn process(&self, entry: &LogEntry) -> Result<(), LoggerError> {
        self.entries.lock().push(entry.clone());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{ContentType, LogEntry, LogLevel};
    use daf_core::AgentId;
    use uuid::Uuid;

    fn make_entry(text: &str, level: LogLevel) -> LogEntry {
        LogEntry::new(
            AgentId::new(),
            Some(AgentId::new()),
            Uuid::now_v7(),
            0,
            ContentType::Text,
            serde_json::Value::String(text.to_owned()),
            level,
        )
    }

    #[tokio::test]
    async fn filter_sink_by_level() {
        let collector = Arc::new(CollectorSink::new());
        let filter = FilterSink::new(collector.clone() as Arc<dyn LogSink>)
            .with_min_level(LogLevel::Warn);

        filter.process(&make_entry("debug msg", LogLevel::Debug)).await.unwrap();
        filter.process(&make_entry("warn msg", LogLevel::Warn)).await.unwrap();
        filter.process(&make_entry("error msg", LogLevel::Error)).await.unwrap();

        assert_eq!(collector.len(), 2);
    }

    #[tokio::test]
    async fn transform_sink_redacts_secrets() {
        let collector = Arc::new(CollectorSink::new());
        let redactor = TransformSink::redact_secrets(collector.clone() as Arc<dyn LogSink>);

        let clean = make_entry("hello world", LogLevel::Info);
        let sensitive = make_entry("my password is hunter2", LogLevel::Info);

        redactor.process(&clean).await.unwrap();
        redactor.process(&sensitive).await.unwrap();

        let entries = collector.entries();
        assert_eq!(entries.len(), 2);

        // Clean entry should pass through unchanged.
        assert_eq!(entries[0].content, serde_json::Value::String("hello world".into()));

        // Sensitive entry should be redacted.
        let redacted = entries[1].content.as_str().unwrap();
        assert!(redacted.contains("REDACTED"));
        assert!(entries[1].tags.contains(&"redacted".to_string()));
    }

    #[tokio::test]
    async fn fan_out_sink_sends_to_all() {
        let c1 = Arc::new(CollectorSink::new());
        let c2 = Arc::new(CollectorSink::new());
        let c3 = Arc::new(CollectorSink::new());

        let fan = FanOutSink::new(vec![
            c1.clone() as Arc<dyn LogSink>,
            c2.clone() as Arc<dyn LogSink>,
            c3.clone() as Arc<dyn LogSink>,
        ]);

        let entry = make_entry("broadcast", LogLevel::Info);
        fan.process(&entry).await.unwrap();

        assert_eq!(c1.len(), 1);
        assert_eq!(c2.len(), 1);
        assert_eq!(c3.len(), 1);
    }

    #[tokio::test]
    async fn buffered_sink_flushes_at_capacity() {
        let collector = Arc::new(CollectorSink::new());
        let buffered = BufferedSink::new(3, collector.clone() as Arc<dyn LogSink>);

        // Two entries — should still be buffered.
        buffered.process(&make_entry("one", LogLevel::Info)).await.unwrap();
        buffered.process(&make_entry("two", LogLevel::Info)).await.unwrap();
        assert_eq!(collector.len(), 0);

        // Third entry triggers flush.
        buffered.process(&make_entry("three", LogLevel::Info)).await.unwrap();
        assert_eq!(collector.len(), 3);
    }

    #[tokio::test]
    async fn buffered_sink_explicit_flush() {
        let collector = Arc::new(CollectorSink::new());
        let buffered = BufferedSink::new(100, collector.clone() as Arc<dyn LogSink>);

        buffered.process(&make_entry("one", LogLevel::Info)).await.unwrap();
        assert_eq!(collector.len(), 0);

        buffered.flush().await.unwrap();
        assert_eq!(collector.len(), 1);
    }

    #[tokio::test]
    async fn pipeline_composition() {
        // Build: filter(warn+) → redact → collect
        let collector = Arc::new(CollectorSink::new());
        let redactor = Arc::new(TransformSink::redact_secrets(
            collector.clone() as Arc<dyn LogSink>,
        ));
        let filter = FilterSink::new(redactor as Arc<dyn LogSink>)
            .with_min_level(LogLevel::Warn);

        // Debug-level entry with secret — should be filtered out.
        filter
            .process(&make_entry("password=abc", LogLevel::Debug))
            .await
            .unwrap();
        assert_eq!(collector.len(), 0);

        // Warn-level with secret — should be forwarded + redacted.
        filter
            .process(&make_entry("password=abc", LogLevel::Warn))
            .await
            .unwrap();
        assert_eq!(collector.len(), 1);

        let entries = collector.entries();
        assert!(entries[0].content.as_str().unwrap().contains("REDACTED"));
    }

    #[tokio::test]
    async fn filter_by_tags() {
        let collector = Arc::new(CollectorSink::new());
        let filter = FilterSink::new(collector.clone() as Arc<dyn LogSink>)
            .with_required_tags(vec!["important".into()]);

        let untagged = make_entry("no tags", LogLevel::Info);
        let tagged = make_entry("has tags", LogLevel::Info).with_tag("important");

        filter.process(&untagged).await.unwrap();
        filter.process(&tagged).await.unwrap();

        assert_eq!(collector.len(), 1);
    }

    #[tokio::test]
    async fn enrich_transform() {
        let collector = Arc::new(CollectorSink::new());
        let enricher = TransformSink::enrich(
            "env".into(),
            serde_json::Value::String("prod".into()),
            collector.clone() as Arc<dyn LogSink>,
        );

        enricher.process(&make_entry("test", LogLevel::Info)).await.unwrap();

        let entries = collector.entries();
        assert_eq!(entries[0].metadata["env"], "prod");
    }
}
