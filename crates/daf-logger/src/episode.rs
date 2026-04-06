//! Episode memory events.
//!
//! An [`Episode`] is a bounded chunk of agent activity — a task from start to
//! finish, a debugging session, a handoff sequence. The [`EpisodeRecorder`]
//! attaches to a live conversation and automatically segments it into episodes
//! based on task boundaries, topic shifts, or explicit markers.
//!
//! Episodes are the unit of long-term memory: when an agent asks "have I seen
//! this before?", the answer is an episode, not a raw log entry.

use chrono::{DateTime, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::entry::{ConversationLog, LogEntry};

// ---------------------------------------------------------------------------
// EpisodeEventType
// ---------------------------------------------------------------------------

/// What kind of thing happened in an episode event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeEventType {
    /// Agent performed an action (tool call, write, API call).
    Action,
    /// Agent observed something (read output, received data).
    Observation,
    /// Agent made a decision about what to do next.
    Decision,
    /// Agent communicated with another agent or human.
    Communication,
}

impl std::fmt::Display for EpisodeEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Action => write!(f, "action"),
            Self::Observation => write!(f, "observation"),
            Self::Decision => write!(f, "decision"),
            Self::Communication => write!(f, "communication"),
        }
    }
}

// ---------------------------------------------------------------------------
// EpisodeEvent
// ---------------------------------------------------------------------------

/// A single event within an episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeEvent {
    /// When this event occurred.
    pub timestamp: DateTime<Utc>,
    /// Which agent produced this event.
    pub agent_id: AgentId,
    /// Classification of the event.
    pub event_type: EpisodeEventType,
    /// Human-readable description of what happened.
    pub description: String,
    /// Optional structured data associated with the event.
    pub data: Value,
    /// The source log entry ID, if this event was derived from a log entry.
    pub source_entry_id: Option<Uuid>,
}

impl EpisodeEvent {
    /// Create a new episode event.
    pub fn new(
        agent_id: AgentId,
        event_type: EpisodeEventType,
        description: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            agent_id,
            event_type,
            description: description.into(),
            data: Value::Null,
            source_entry_id: None,
        }
    }

    /// Attach structured data.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }

    /// Link back to the originating log entry.
    pub fn with_source(mut self, entry_id: Uuid) -> Self {
        self.source_entry_id = Some(entry_id);
        self
    }
}

// ---------------------------------------------------------------------------
// Episode
// ---------------------------------------------------------------------------

/// A bounded, self-contained chunk of agent activity.
///
/// Episodes are the atoms of episodic memory. They capture what happened,
/// who was involved, what triggered the activity, and what was learned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    /// Unique identifier for this episode.
    pub id: Uuid,
    /// Short human-readable title summarizing the episode.
    pub title: String,
    /// Agents that participated.
    pub participants: Vec<AgentId>,
    /// When the episode started.
    pub start_time: DateTime<Utc>,
    /// When the episode ended (`None` if still active).
    pub end_time: Option<DateTime<Utc>>,
    /// What triggered this episode (user request, scheduled task, error, etc.).
    pub trigger: String,
    /// Outcome of the episode (success, failure, partial, handed off).
    pub outcome: Option<String>,
    /// Ordered sequence of events within this episode.
    pub key_events: Vec<EpisodeEvent>,
    /// Lessons learned or insights from this episode.
    pub learnings: Vec<String>,
    /// The conversation ID this episode was extracted from.
    pub conversation_id: Uuid,
    /// Arbitrary metadata.
    pub metadata: Value,
}

impl Episode {
    /// Start a new episode.
    pub fn new(
        title: impl Into<String>,
        trigger: impl Into<String>,
        conversation_id: Uuid,
        participants: Vec<AgentId>,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            title: title.into(),
            participants,
            start_time: Utc::now(),
            end_time: None,
            trigger: trigger.into(),
            outcome: None,
            key_events: Vec::new(),
            learnings: Vec::new(),
            conversation_id,
            metadata: Value::Object(serde_json::Map::new()),
        }
    }

    /// Record an event in this episode.
    pub fn record_event(&mut self, event: EpisodeEvent) {
        if !self.participants.contains(&event.agent_id) {
            self.participants.push(event.agent_id);
        }
        self.key_events.push(event);
    }

    /// Close the episode with an outcome.
    pub fn close(&mut self, outcome: impl Into<String>) {
        self.end_time = Some(Utc::now());
        self.outcome = Some(outcome.into());
    }

    /// Add a learning.
    pub fn add_learning(&mut self, learning: impl Into<String>) {
        self.learnings.push(learning.into());
    }

    /// Duration of the episode.
    pub fn duration(&self) -> Option<chrono::Duration> {
        self.end_time.map(|end| end - self.start_time)
    }

    /// Whether the episode is still active.
    pub fn is_active(&self) -> bool {
        self.end_time.is_none()
    }

    /// Number of events recorded.
    pub fn event_count(&self) -> usize {
        self.key_events.len()
    }
}

// ---------------------------------------------------------------------------
// EpisodeRecorder
// ---------------------------------------------------------------------------

/// Attaches to a conversation and automatically segments it into episodes.
///
/// The recorder watches incoming log entries and uses heuristics to detect
/// task boundaries:
/// - Explicit boundary markers (tags like `"episode:start"`, `"episode:end"`).
/// - Long gaps between entries (configurable idle threshold).
/// - Topic shifts detected by a change in participating agents.
pub struct EpisodeRecorder {
    /// Active episode being recorded.
    current_episode: Option<Episode>,
    /// Completed episodes.
    completed: Vec<Episode>,
    /// Conversation we are recording.
    conversation_id: Uuid,
    /// How long of a gap (in seconds) triggers a new episode.
    idle_threshold_secs: i64,
    /// The timestamp of the last entry we processed.
    last_entry_time: Option<DateTime<Utc>>,
}

impl EpisodeRecorder {
    /// Create a recorder for a specific conversation.
    pub fn new(conversation_id: Uuid) -> Self {
        Self {
            current_episode: None,
            completed: Vec::new(),
            conversation_id,
            idle_threshold_secs: 300, // 5 minutes
            last_entry_time: None,
        }
    }

    /// Set the idle threshold (in seconds) that triggers a new episode.
    pub fn with_idle_threshold(mut self, seconds: i64) -> Self {
        self.idle_threshold_secs = seconds;
        self
    }

    /// Process a log entry and update episode state.
    pub fn record(&mut self, entry: &LogEntry) {
        // Check for explicit boundary markers.
        if entry.tags.iter().any(|t| t == "episode:end") {
            self.end_current_episode("Explicit episode:end marker");
            self.last_entry_time = Some(entry.timestamp);
            return;
        }

        if entry.tags.iter().any(|t| t == "episode:start") {
            self.end_current_episode("New episode:start marker");
            self.start_episode_from_entry(entry);
            self.last_entry_time = Some(entry.timestamp);
            return;
        }

        // Check for idle gap.
        if let Some(last_time) = self.last_entry_time {
            let gap = (entry.timestamp - last_time).num_seconds();
            if gap >= self.idle_threshold_secs {
                self.end_current_episode(&format!("Idle gap of {gap}s exceeded threshold"));
            }
        }

        // Ensure an episode is active.
        if self.current_episode.is_none() {
            self.start_episode_from_entry(entry);
        }

        // Record the event.
        if let Some(ep) = self.current_episode.as_mut() {
            let event_type = classify_entry(entry);
            let text = match &entry.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            let event = EpisodeEvent::new(entry.source_agent, event_type, text)
                .with_source(entry.id);
            ep.record_event(event);
        }

        self.last_entry_time = Some(entry.timestamp);
    }

    /// Process an entire conversation log and produce episodes.
    pub fn record_conversation(&mut self, log: &ConversationLog) -> &[Episode] {
        for entry in &log.entries {
            self.record(entry);
        }
        // Finalize any open episode.
        self.end_current_episode("Conversation ended");
        &self.completed
    }

    /// Explicitly end the current episode.
    pub fn end_current_episode(&mut self, reason: &str) {
        if let Some(mut ep) = self.current_episode.take() {
            ep.close(reason);
            self.completed.push(ep);
        }
    }

    /// Get completed episodes.
    pub fn episodes(&self) -> &[Episode] {
        &self.completed
    }

    /// Consume the recorder and return all episodes (including the current one).
    pub fn into_episodes(mut self) -> Vec<Episode> {
        self.end_current_episode("Recorder consumed");
        self.completed
    }

    /// The currently active episode, if any.
    pub fn current(&self) -> Option<&Episode> {
        self.current_episode.as_ref()
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn start_episode_from_entry(&mut self, entry: &LogEntry) {
        let text = match &entry.content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };

        let title = if text.len() > 60 {
            format!("{}...", &text[..57])
        } else {
            text.clone()
        };

        let ep = Episode::new(
            title,
            text,
            self.conversation_id,
            vec![entry.source_agent],
        );
        self.current_episode = Some(ep);
    }
}

/// Heuristic: classify a log entry into an episode event type based on tags
/// and content signals.
fn classify_entry(entry: &LogEntry) -> EpisodeEventType {
    let tags_lower: Vec<String> = entry.tags.iter().map(|t| t.to_lowercase()).collect();

    if tags_lower.iter().any(|t| t.contains("decision") || t.contains("chose")) {
        return EpisodeEventType::Decision;
    }
    if tags_lower.iter().any(|t| t.contains("action") || t.contains("tool") || t.contains("execute")) {
        return EpisodeEventType::Action;
    }
    if tags_lower.iter().any(|t| t.contains("observe") || t.contains("read") || t.contains("result")) {
        return EpisodeEventType::Observation;
    }

    // Fall back to content heuristics.
    if let serde_json::Value::String(s) = &entry.content {
        let lower = s.to_lowercase();
        if lower.contains("decided") || lower.contains("choosing") || lower.contains("decision") {
            return EpisodeEventType::Decision;
        }
        if lower.contains("executed") || lower.contains("running") || lower.contains("calling") {
            return EpisodeEventType::Action;
        }
        if lower.contains("observed") || lower.contains("result:") || lower.contains("output:") {
            return EpisodeEventType::Observation;
        }
    }

    // Default: communication between agents.
    EpisodeEventType::Communication
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::LogEntry;
    use daf_core::AgentId;

    fn make_conv(n: usize) -> (Uuid, AgentId, AgentId, Vec<LogEntry>) {
        let conv_id = Uuid::now_v7();
        let a = AgentId::new();
        let b = AgentId::new();
        let mut entries = Vec::new();

        for i in 0..n {
            let src = if i % 2 == 0 { a } else { b };
            let tgt = if i % 2 == 0 { b } else { a };
            entries.push(LogEntry::text(src, tgt, conv_id, i as u64, &format!("msg {i}")));
        }

        (conv_id, a, b, entries)
    }

    #[test]
    fn recorder_creates_episode_from_entries() {
        let (conv_id, _, _, entries) = make_conv(5);
        let mut recorder = EpisodeRecorder::new(conv_id);

        for e in &entries {
            recorder.record(e);
        }

        // Should have one active episode.
        assert!(recorder.current().is_some());
        assert_eq!(recorder.current().unwrap().event_count(), 5);
    }

    #[test]
    fn recorder_segments_on_explicit_markers() {
        let conv_id = Uuid::now_v7();
        let a = AgentId::new();
        let b = AgentId::new();

        let mut recorder = EpisodeRecorder::new(conv_id);

        let e1 = LogEntry::text(a, b, conv_id, 0, "start task 1")
            .with_tag("episode:start");
        let e2 = LogEntry::text(b, a, conv_id, 1, "working on task 1");
        let e3 = LogEntry::text(a, b, conv_id, 2, "done with task 1")
            .with_tag("episode:end");
        let e4 = LogEntry::text(b, a, conv_id, 3, "start task 2")
            .with_tag("episode:start");
        let e5 = LogEntry::text(a, b, conv_id, 4, "working on task 2");

        for e in [&e1, &e2, &e3, &e4, &e5] {
            recorder.record(e);
        }

        // One completed episode (task 1) + one active (task 2).
        assert_eq!(recorder.episodes().len(), 1);
        assert!(recorder.current().is_some());
        assert_eq!(recorder.current().unwrap().event_count(), 2);
    }

    #[test]
    fn into_episodes_finalizes_active() {
        let (conv_id, _, _, entries) = make_conv(3);
        let mut recorder = EpisodeRecorder::new(conv_id);
        for e in &entries {
            recorder.record(e);
        }

        let episodes = recorder.into_episodes();
        assert_eq!(episodes.len(), 1);
        assert!(episodes[0].end_time.is_some());
    }

    #[test]
    fn episode_serialization_roundtrip() {
        let ep = Episode::new(
            "Test Episode",
            "unit test",
            Uuid::now_v7(),
            vec![AgentId::new()],
        );
        let json = serde_json::to_string(&ep).unwrap();
        let back: Episode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, ep.id);
        assert_eq!(back.title, "Test Episode");
    }

    #[test]
    fn classify_entry_heuristics() {
        let a = AgentId::new();
        let b = AgentId::new();
        let conv = Uuid::now_v7();

        let decision = LogEntry::text(a, b, conv, 0, "We decided to use Postgres");
        assert_eq!(classify_entry(&decision), EpisodeEventType::Decision);

        let action = LogEntry::text(a, b, conv, 1, "Running the migration script now");
        assert_eq!(classify_entry(&action), EpisodeEventType::Action);

        let comm = LogEntry::text(a, b, conv, 2, "Hello, how are you?");
        assert_eq!(classify_entry(&comm), EpisodeEventType::Communication);
    }

    #[test]
    fn episode_duration() {
        let mut ep = Episode::new("test", "test", Uuid::now_v7(), vec![]);
        assert!(ep.duration().is_none());
        assert!(ep.is_active());

        ep.close("done");
        assert!(!ep.is_active());
        assert!(ep.duration().is_some());
    }
}
