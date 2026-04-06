//! Log querying and aggregation.
//!
//! [`LogQuery`] is a builder for filtering log entries by agent, conversation,
//! time range, level, tags, and content. [`QueryResult`] wraps the matches
//! with pagination metadata. Aggregation helpers provide quick statistics.

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entry::{ConversationLog, LogEntry, LogLevel};

// ---------------------------------------------------------------------------
// LogQuery
// ---------------------------------------------------------------------------

/// Builder for filtering log entries.
///
/// All filters are conjunctive (AND). An empty query matches everything.
///
/// ```rust,ignore
/// let query = LogQuery::new()
///     .by_agent(some_agent_id)
///     .by_level(LogLevel::Warn)
///     .by_tag("security")
///     .limit(50);
/// ```
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    agent: Option<AgentId>,
    conversation: Option<Uuid>,
    time_start: Option<DateTime<Utc>>,
    time_end: Option<DateTime<Utc>>,
    min_level: Option<LogLevel>,
    tags: Vec<String>,
    content_search: Option<String>,
    max_results: Option<usize>,
    offset: usize,
}

impl LogQuery {
    /// Create an empty query (matches everything).
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter entries by source agent.
    pub fn by_agent(mut self, agent: AgentId) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Filter entries by conversation ID.
    pub fn by_conversation(mut self, id: Uuid) -> Self {
        self.conversation = Some(id);
        self
    }

    /// Filter entries created at or after `start`.
    pub fn by_time_start(mut self, start: DateTime<Utc>) -> Self {
        self.time_start = Some(start);
        self
    }

    /// Filter entries created at or before `end`.
    pub fn by_time_end(mut self, end: DateTime<Utc>) -> Self {
        self.time_end = Some(end);
        self
    }

    /// Convenience: filter by a time range.
    pub fn by_time_range(self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.by_time_start(start).by_time_end(end)
    }

    /// Filter entries at or above this severity level.
    pub fn by_level(mut self, level: LogLevel) -> Self {
        self.min_level = Some(level);
        self
    }

    /// Filter entries that have this tag.
    pub fn by_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Full-text search over entry content (case-insensitive substring match).
    pub fn by_content_search(mut self, search: impl Into<String>) -> Self {
        self.content_search = Some(search.into());
        self
    }

    /// Maximum number of results to return.
    pub fn limit(mut self, n: usize) -> Self {
        self.max_results = Some(n);
        self
    }

    /// Skip the first `n` matching entries (for pagination).
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = n;
        self
    }

    /// Test whether a single entry matches all filters in this query.
    pub fn matches(&self, entry: &LogEntry) -> bool {
        if let Some(agent) = &self.agent {
            if &entry.source_agent != agent {
                return false;
            }
        }
        if let Some(conv) = &self.conversation {
            if &entry.conversation_id != conv {
                return false;
            }
        }
        if let Some(start) = &self.time_start {
            if entry.timestamp < *start {
                return false;
            }
        }
        if let Some(end) = &self.time_end {
            if entry.timestamp > *end {
                return false;
            }
        }
        if let Some(min) = &self.min_level {
            if entry.level < *min {
                return false;
            }
        }
        for tag in &self.tags {
            if !entry.tags.contains(tag) {
                return false;
            }
        }
        if let Some(search) = &self.content_search {
            let content_str = match &entry.content {
                serde_json::Value::String(s) => s.to_lowercase(),
                other => other.to_string().to_lowercase(),
            };
            if !content_str.contains(&search.to_lowercase()) {
                return false;
            }
        }
        true
    }

    /// Execute the query against a slice of entries.
    pub fn execute<'a>(&self, entries: &'a [LogEntry]) -> QueryResult<&'a LogEntry> {
        let matching: Vec<&'a LogEntry> = entries.iter().filter(|e| self.matches(e)).collect();

        let total = matching.len();
        let page = matching
            .into_iter()
            .skip(self.offset)
            .take(self.max_results.unwrap_or(usize::MAX))
            .collect();

        QueryResult {
            items: page,
            total,
            offset: self.offset,
            limit: self.max_results,
        }
    }

    /// Execute against a [`ConversationLog`].
    pub fn execute_on_conversation<'a>(
        &self,
        log: &'a ConversationLog,
    ) -> QueryResult<&'a LogEntry> {
        self.execute(&log.entries)
    }
}

// ---------------------------------------------------------------------------
// QueryResult
// ---------------------------------------------------------------------------

/// Paginated query result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult<T> {
    /// The matching items for this page.
    pub items: Vec<T>,
    /// Total number of matching items (before pagination).
    pub total: usize,
    /// Offset used for this page.
    pub offset: usize,
    /// Limit used for this page (`None` = unlimited).
    pub limit: Option<usize>,
}

impl<T> QueryResult<T> {
    /// Whether there are more results beyond this page.
    pub fn has_more(&self) -> bool {
        match self.limit {
            Some(limit) => self.offset + limit < self.total,
            None => false,
        }
    }

    /// Number of items in this page.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether this page is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Aggregations
// ---------------------------------------------------------------------------

/// Count entries grouped by source agent.
pub fn count_by_agent(entries: &[LogEntry]) -> Vec<(AgentId, usize)> {
    let mut map = std::collections::HashMap::<AgentId, usize>::new();
    for entry in entries {
        *map.entry(entry.source_agent).or_default() += 1;
    }
    let mut result: Vec<_> = map.into_iter().collect();
    result.sort_by(|a, b| b.1.cmp(&a.1)); // descending by count
    result
}

/// Count entries grouped by log level.
pub fn count_by_level(entries: &[LogEntry]) -> Vec<(LogLevel, usize)> {
    let mut map = std::collections::HashMap::<LogLevel, usize>::new();
    for entry in entries {
        *map.entry(entry.level).or_default() += 1;
    }
    let mut result: Vec<_> = map.into_iter().collect();
    result.sort_by(|a, b| a.0.cmp(&b.0)); // ascending by severity
    result
}

/// Statistics about conversation durations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurationStats {
    /// Number of conversations analyzed.
    pub count: usize,
    /// Total duration across all conversations (milliseconds).
    pub total_ms: i64,
    /// Mean duration (milliseconds).
    pub mean_ms: f64,
    /// Minimum duration (milliseconds).
    pub min_ms: i64,
    /// Maximum duration (milliseconds).
    pub max_ms: i64,
}

/// Compute duration statistics across multiple conversation logs.
pub fn conversation_duration_stats(logs: &[ConversationLog]) -> DurationStats {
    let durations: Vec<i64> = logs
        .iter()
        .filter_map(|log| log.duration())
        .map(|d| d.num_milliseconds())
        .collect();

    if durations.is_empty() {
        return DurationStats {
            count: 0,
            total_ms: 0,
            mean_ms: 0.0,
            min_ms: 0,
            max_ms: 0,
        };
    }

    let total: i64 = durations.iter().sum();
    let count = durations.len();

    DurationStats {
        count,
        total_ms: total,
        mean_ms: total as f64 / count as f64,
        min_ms: *durations.iter().min().unwrap(),
        max_ms: *durations.iter().max().unwrap(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::LogEntry;
    use daf_core::AgentId;

    fn sample_entries() -> (AgentId, AgentId, Uuid, Vec<LogEntry>) {
        let a = AgentId::new();
        let b = AgentId::new();
        let conv = Uuid::now_v7();

        let entries = vec![
            LogEntry::text(a, b, conv, 0, "hello").with_tag("greet"),
            LogEntry::text(b, a, conv, 1, "hi back"),
            LogEntry::text(a, b, conv, 2, "let's debug the error"),
            {
                let mut e = LogEntry::text(b, a, conv, 3, "found the bug");
                e.level = LogLevel::Warn;
                e
            },
            {
                let mut e = LogEntry::text(a, b, conv, 4, "critical failure");
                e.level = LogLevel::Error;
                e
            },
        ];

        (a, b, conv, entries)
    }

    #[test]
    fn query_by_agent() {
        let (a, _, _, entries) = sample_entries();
        let result = LogQuery::new().by_agent(a).execute(&entries);
        assert_eq!(result.total, 3); // entries 0, 2, 4 are from agent a
    }

    #[test]
    fn query_by_level() {
        let (_, _, _, entries) = sample_entries();
        let result = LogQuery::new().by_level(LogLevel::Warn).execute(&entries);
        assert_eq!(result.total, 2); // Warn + Error
    }

    #[test]
    fn query_by_tag() {
        let (_, _, _, entries) = sample_entries();
        let result = LogQuery::new().by_tag("greet").execute(&entries);
        assert_eq!(result.total, 1);
    }

    #[test]
    fn query_content_search() {
        let (_, _, _, entries) = sample_entries();
        let result = LogQuery::new()
            .by_content_search("error")
            .execute(&entries);
        assert_eq!(result.total, 1);
        assert_eq!(result.items[0].turn_number, 2);
    }

    #[test]
    fn query_pagination() {
        let (_, _, _, entries) = sample_entries();
        let page1 = LogQuery::new().limit(2).offset(0).execute(&entries);
        assert_eq!(page1.len(), 2);
        assert_eq!(page1.total, 5);
        assert!(page1.has_more());

        let page2 = LogQuery::new().limit(2).offset(2).execute(&entries);
        assert_eq!(page2.len(), 2);
        assert!(page2.has_more());

        let page3 = LogQuery::new().limit(2).offset(4).execute(&entries);
        assert_eq!(page3.len(), 1);
        assert!(!page3.has_more());
    }

    #[test]
    fn combined_filters() {
        let (a, _, _, entries) = sample_entries();
        let result = LogQuery::new()
            .by_agent(a)
            .by_level(LogLevel::Error)
            .execute(&entries);
        assert_eq!(result.total, 1);
        assert_eq!(result.items[0].turn_number, 4);
    }

    #[test]
    fn count_by_agent_aggregation() {
        let (a, b, _, entries) = sample_entries();
        let counts = count_by_agent(&entries);
        let a_count = counts.iter().find(|(id, _)| *id == a).unwrap().1;
        let b_count = counts.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(a_count, 3);
        assert_eq!(b_count, 2);
    }

    #[test]
    fn count_by_level_aggregation() {
        let (_, _, _, entries) = sample_entries();
        let counts = count_by_level(&entries);
        let info = counts.iter().find(|(l, _)| *l == LogLevel::Info).unwrap().1;
        let warn = counts.iter().find(|(l, _)| *l == LogLevel::Warn).unwrap().1;
        let error = counts.iter().find(|(l, _)| *l == LogLevel::Error).unwrap().1;
        assert_eq!(info, 3);
        assert_eq!(warn, 1);
        assert_eq!(error, 1);
    }

    #[test]
    fn empty_duration_stats() {
        let stats = conversation_duration_stats(&[]);
        assert_eq!(stats.count, 0);
    }
}
