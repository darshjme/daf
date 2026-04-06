//! KB extraction engine.
//!
//! Scans completed [`ConversationLog`]s and pulls out structured knowledge —
//! decisions made, errors encountered, patterns observed — that can be persisted
//! to the knowledge base for future agent reference.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::entry::{ConversationLog, LogEntry, LogLevel};

// ---------------------------------------------------------------------------
// KnowledgeCategory
// ---------------------------------------------------------------------------

/// What kind of knowledge was extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeCategory {
    /// An explicit decision point (e.g. "we chose X over Y because Z").
    Decision,
    /// A factual statement or data point discovered during conversation.
    Fact,
    /// A higher-level insight or conclusion drawn from multiple facts.
    Insight,
    /// An error, failure, or exception and how it was resolved.
    Error,
    /// A recurring interaction pattern between agents.
    Pattern,
}

impl std::fmt::Display for KnowledgeCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decision => write!(f, "decision"),
            Self::Fact => write!(f, "fact"),
            Self::Insight => write!(f, "insight"),
            Self::Error => write!(f, "error"),
            Self::Pattern => write!(f, "pattern"),
        }
    }
}

// ---------------------------------------------------------------------------
// KnowledgeEntry
// ---------------------------------------------------------------------------

/// A single piece of extracted knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    /// Unique identifier.
    pub id: Uuid,
    /// Conversation this knowledge was extracted from.
    pub source_conversation: Uuid,
    /// When extraction happened.
    pub extracted_at: DateTime<Utc>,
    /// Category of knowledge.
    pub category: KnowledgeCategory,
    /// The knowledge content (human-readable).
    pub content: String,
    /// Confidence score in `[0.0, 1.0]`. Higher = more certain.
    pub confidence: f64,
    /// IDs of related knowledge entries (for cross-referencing).
    pub related_entries: Vec<Uuid>,
    /// The source log entry IDs that contributed to this knowledge.
    pub source_entry_ids: Vec<Uuid>,
    /// Arbitrary metadata.
    pub metadata: Value,
}

impl KnowledgeEntry {
    /// Create a new knowledge entry.
    pub fn new(
        source_conversation: Uuid,
        category: KnowledgeCategory,
        content: impl Into<String>,
        confidence: f64,
        source_entry_ids: Vec<Uuid>,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            source_conversation,
            extracted_at: Utc::now(),
            category,
            content: content.into(),
            confidence: confidence.clamp(0.0, 1.0),
            related_entries: Vec::new(),
            source_entry_ids,
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// ExtractionRule
// ---------------------------------------------------------------------------

/// Defines how to detect and categorize a piece of knowledge from log text.
#[derive(Debug, Clone)]
pub struct ExtractionRule {
    /// Human-readable name for this rule.
    pub name: String,
    /// Keywords or patterns to match against entry content (case-insensitive substring).
    pub keywords: Vec<String>,
    /// Category to assign when the rule matches.
    pub category: KnowledgeCategory,
    /// Confidence assigned by this rule (before any modifiers).
    pub base_confidence: f64,
}

impl ExtractionRule {
    /// Create a new rule.
    pub fn new(
        name: impl Into<String>,
        keywords: Vec<String>,
        category: KnowledgeCategory,
        base_confidence: f64,
    ) -> Self {
        Self {
            name: name.into(),
            keywords,
            category,
            base_confidence: base_confidence.clamp(0.0, 1.0),
        }
    }

    /// Check whether an entry's content matches this rule.
    pub fn matches(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.keywords
            .iter()
            .any(|kw| lower.contains(&kw.to_lowercase()))
    }
}

// ---------------------------------------------------------------------------
// KBExtractor
// ---------------------------------------------------------------------------

/// The extraction engine. Holds built-in + custom rules and runs them over
/// conversation logs to produce [`KnowledgeEntry`] records.
pub struct KBExtractor {
    rules: Vec<ExtractionRule>,
}

impl KBExtractor {
    /// Create an extractor with the default built-in rules.
    pub fn new() -> Self {
        Self {
            rules: Self::builtin_rules(),
        }
    }

    /// Create an extractor with no rules (useful for adding only custom rules).
    pub fn empty() -> Self {
        Self { rules: Vec::new() }
    }

    /// Register a custom extraction rule.
    pub fn add_rule(&mut self, rule: ExtractionRule) {
        self.rules.push(rule);
    }

    /// Run all rules over a conversation and return extracted knowledge.
    pub fn extract_from_conversation(&self, log: &ConversationLog) -> Vec<KnowledgeEntry> {
        let mut results = Vec::new();

        // Rule-based extraction over every entry.
        for entry in &log.entries {
            let text = entry_text(entry);
            for rule in &self.rules {
                if rule.matches(&text) {
                    results.push(KnowledgeEntry::new(
                        log.conversation_id,
                        rule.category,
                        format!("[{}] {}", rule.name, text),
                        rule.base_confidence,
                        vec![entry.id],
                    ));
                }
            }
        }

        // Structural extraction (not rule-based).
        results.extend(self.extract_decisions(log));
        results.extend(self.extract_errors(log));
        results.extend(self.extract_patterns(log));

        results
    }

    /// Find decision points — entries that reference choosing between alternatives.
    pub fn extract_decisions(&self, log: &ConversationLog) -> Vec<KnowledgeEntry> {
        let decision_signals = [
            "decided to",
            "choosing",
            "chose",
            "decision:",
            "we'll go with",
            "going with",
            "selected",
            "picking",
            "opted for",
            "conclusion:",
            "resolution:",
            "agreed on",
        ];

        let mut out = Vec::new();
        for entry in &log.entries {
            let text = entry_text(entry);
            let lower = text.to_lowercase();
            if decision_signals.iter().any(|s| lower.contains(s)) {
                out.push(KnowledgeEntry::new(
                    log.conversation_id,
                    KnowledgeCategory::Decision,
                    text,
                    0.75,
                    vec![entry.id],
                ));
            }
        }
        out
    }

    /// Capture error entries and any immediately following resolution.
    pub fn extract_errors(&self, log: &ConversationLog) -> Vec<KnowledgeEntry> {
        let mut out = Vec::new();
        let entries = &log.entries;

        for (i, entry) in entries.iter().enumerate() {
            if entry.level >= LogLevel::Error {
                let error_text = entry_text(entry);

                // Look ahead for a resolution (next entry from a different agent).
                let resolution = entries.get(i + 1).map(|next| {
                    if next.source_agent != entry.source_agent {
                        format!(" | Resolution: {}", entry_text(next))
                    } else {
                        String::new()
                    }
                });

                let content =
                    format!("Error: {}{}", error_text, resolution.unwrap_or_default());

                out.push(KnowledgeEntry::new(
                    log.conversation_id,
                    KnowledgeCategory::Error,
                    content,
                    0.9,
                    vec![entry.id],
                ));
            }
        }
        out
    }

    /// Identify recurring interaction patterns (e.g. request-response pairs,
    /// repeated agent hand-offs).
    pub fn extract_patterns(&self, log: &ConversationLog) -> Vec<KnowledgeEntry> {
        let mut out = Vec::new();

        if log.entries.len() < 4 {
            return out;
        }

        // Detect ping-pong: same two agents alternating for 4+ turns.
        let mut streak_start = 0;
        let mut streak_len = 0u64;

        for i in 2..log.entries.len() {
            let same_pair = log.entries[i].source_agent == log.entries[i - 2].source_agent
                && log.entries[i - 1].source_agent != log.entries[i].source_agent;
            if same_pair {
                if streak_len == 0 {
                    streak_start = i - 2;
                    streak_len = 3;
                } else {
                    streak_len += 1;
                }
            } else {
                if streak_len >= 4 {
                    let a = log.entries[streak_start].source_agent;
                    let b = log.entries[streak_start + 1].source_agent;
                    out.push(KnowledgeEntry::new(
                        log.conversation_id,
                        KnowledgeCategory::Pattern,
                        format!(
                            "Ping-pong pattern between {a} and {b} for {streak_len} turns (starting turn {})",
                            log.entries[streak_start].turn_number
                        ),
                        0.6,
                        vec![
                            log.entries[streak_start].id,
                            log.entries[streak_start + 1].id,
                        ],
                    ));
                }
                streak_len = 0;
            }
        }

        // Flush remaining streak.
        if streak_len >= 4 {
            let a = log.entries[streak_start].source_agent;
            let b = log.entries[streak_start + 1].source_agent;
            out.push(KnowledgeEntry::new(
                log.conversation_id,
                KnowledgeCategory::Pattern,
                format!(
                    "Ping-pong pattern between {a} and {b} for {streak_len} turns (starting turn {})",
                    log.entries[streak_start].turn_number
                ),
                0.6,
                vec![
                    log.entries[streak_start].id,
                    log.entries[streak_start + 1].id,
                ],
            ));
        }

        out
    }

    // -----------------------------------------------------------------------
    // Built-in rules
    // -----------------------------------------------------------------------

    fn builtin_rules() -> Vec<ExtractionRule> {
        vec![
            ExtractionRule::new(
                "todo_detected",
                vec!["TODO".into(), "FIXME".into(), "HACK".into(), "XXX".into()],
                KnowledgeCategory::Insight,
                0.7,
            ),
            ExtractionRule::new(
                "performance_concern",
                vec![
                    "slow".into(),
                    "latency".into(),
                    "timeout".into(),
                    "bottleneck".into(),
                    "performance".into(),
                ],
                KnowledgeCategory::Insight,
                0.6,
            ),
            ExtractionRule::new(
                "security_flag",
                vec![
                    "secret".into(),
                    "credential".into(),
                    "vulnerability".into(),
                    "CVE-".into(),
                    "injection".into(),
                    "XSS".into(),
                ],
                KnowledgeCategory::Fact,
                0.85,
            ),
            ExtractionRule::new(
                "dependency_note",
                vec![
                    "depends on".into(),
                    "requires".into(),
                    "prerequisite".into(),
                    "blocked by".into(),
                ],
                KnowledgeCategory::Fact,
                0.65,
            ),
            ExtractionRule::new(
                "lesson_learned",
                vec![
                    "learned that".into(),
                    "turns out".into(),
                    "in hindsight".into(),
                    "next time".into(),
                    "lesson:".into(),
                ],
                KnowledgeCategory::Insight,
                0.8,
            ),
        ]
    }
}

impl Default for KBExtractor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a plain-text representation from a log entry's content field.
fn entry_text(entry: &LogEntry) -> String {
    match &entry.content {
        Value::String(s) => s.clone(),
        other => other.to_string(),
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

    fn conv_with_messages(messages: &[(&str, LogLevel)]) -> ConversationLog {
        let conv_id = Uuid::now_v7();
        let a = AgentId::new();
        let b = AgentId::new();
        let mut log = ConversationLog::new(conv_id, vec![a, b]);

        for (i, (text, level)) in messages.iter().enumerate() {
            let src = if i % 2 == 0 { a } else { b };
            let tgt = if i % 2 == 0 { b } else { a };
            let mut entry = LogEntry::text(src, tgt, conv_id, i as u64, text);
            entry.level = *level;
            log.push(entry);
        }
        log
    }

    #[test]
    fn extracts_decisions() {
        let log = conv_with_messages(&[
            ("Let's discuss the approach", LogLevel::Info),
            ("I think we should use RocksDB", LogLevel::Info),
            ("Agreed — decided to go with RocksDB for persistence", LogLevel::Info),
        ]);

        let ext = KBExtractor::new();
        let decisions = ext.extract_decisions(&log);
        assert!(!decisions.is_empty());
        assert!(decisions
            .iter()
            .any(|d| d.content.contains("decided to")));
    }

    #[test]
    fn extracts_errors_with_resolution() {
        let log = conv_with_messages(&[
            ("Starting the task", LogLevel::Info),
            ("Connection refused on port 5432", LogLevel::Error),
            ("Restarted postgres — connection restored", LogLevel::Info),
        ]);

        let ext = KBExtractor::new();
        let errors = ext.extract_errors(&log);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].content.contains("Resolution:"));
    }

    #[test]
    fn builtin_rules_detect_security() {
        let log = conv_with_messages(&[
            ("Found a potential SQL injection vulnerability in the auth module", LogLevel::Warn),
        ]);

        let ext = KBExtractor::new();
        let all = ext.extract_from_conversation(&log);
        let security: Vec<_> = all
            .iter()
            .filter(|k| k.content.contains("security_flag"))
            .collect();
        assert!(!security.is_empty());
    }

    #[test]
    fn custom_rule_registration() {
        let mut ext = KBExtractor::empty();
        ext.add_rule(ExtractionRule::new(
            "custom_test",
            vec!["banana".into()],
            KnowledgeCategory::Fact,
            0.99,
        ));

        let log = conv_with_messages(&[("I like banana smoothies", LogLevel::Info)]);
        let results = ext.extract_from_conversation(&log);
        assert!(results.iter().any(|k| k.content.contains("banana")));
    }

    #[test]
    fn extraction_rule_case_insensitive() {
        let rule = ExtractionRule::new(
            "test",
            vec!["ERROR".into()],
            KnowledgeCategory::Error,
            0.5,
        );
        assert!(rule.matches("there was an error here"));
        assert!(rule.matches("ERROR: something broke"));
        assert!(!rule.matches("everything is fine"));
    }

    #[test]
    fn knowledge_entry_confidence_clamped() {
        let ke = KnowledgeEntry::new(
            Uuid::now_v7(),
            KnowledgeCategory::Fact,
            "over-confident",
            1.5,
            vec![],
        );
        assert!((ke.confidence - 1.0).abs() < f64::EPSILON);

        let ke2 = KnowledgeEntry::new(
            Uuid::now_v7(),
            KnowledgeCategory::Fact,
            "negative",
            -0.5,
            vec![],
        );
        assert!(ke2.confidence.abs() < f64::EPSILON);
    }
}
