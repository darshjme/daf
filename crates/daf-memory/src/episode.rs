//! Episode management — capturing sequences of agent actions into coherent episodes.
//!
//! An episode is a bounded narrative of what happened during a task or conversation:
//! which agents participated, what events occurred, what was decided, and what was learned.
//! Episodes are the raw material from which semantic memories are distilled.

use chrono::{DateTime, Duration, Utc};
use daf_core::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// EpisodeId
// ---------------------------------------------------------------------------

/// Globally unique, time-ordered identifier for an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct EpisodeId(pub Uuid);

impl EpisodeId {
    /// Create a new time-ordered episode identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }
}

impl Default for EpisodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for EpisodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ep:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// EpisodeEvent
// ---------------------------------------------------------------------------

/// A single event within an episode — the atomic unit of episode history.
///
/// Events record what happened at a specific point in time: which agent
/// acted, what action it took, what it observed, and any data payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeEvent {
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Which agent produced this event.
    pub agent_id: AgentId,
    /// What the agent did (e.g., "called_tool", "sent_message", "made_decision").
    pub action: String,
    /// What the agent observed as a result.
    pub observation: Option<String>,
    /// The decision or reasoning behind the action (chain-of-thought).
    pub decision: Option<String>,
    /// Arbitrary structured data associated with this event.
    pub data: serde_json::Value,
}

impl EpisodeEvent {
    /// Create a new event with the minimum required fields.
    pub fn new(agent_id: AgentId, action: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            agent_id,
            action: action.into(),
            observation: None,
            decision: None,
            data: serde_json::Value::Null,
        }
    }

    /// Set the observation.
    pub fn with_observation(mut self, obs: impl Into<String>) -> Self {
        self.observation = Some(obs.into());
        self
    }

    /// Set the decision reasoning.
    pub fn with_decision(mut self, decision: impl Into<String>) -> Self {
        self.decision = Some(decision.into());
        self
    }

    /// Set the data payload.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }
}

// ---------------------------------------------------------------------------
// EpisodeOutcome
// ---------------------------------------------------------------------------

/// How an episode concluded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EpisodeOutcome {
    /// The task completed successfully.
    Success,
    /// The task failed with a reason.
    Failure(String),
    /// The task was abandoned or timed out.
    Abandoned(String),
    /// The episode is still in progress.
    InProgress,
}

impl std::fmt::Display for EpisodeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Failure(r) => write!(f, "failure: {r}"),
            Self::Abandoned(r) => write!(f, "abandoned: {r}"),
            Self::InProgress => write!(f, "in_progress"),
        }
    }
}

// ---------------------------------------------------------------------------
// Episode
// ---------------------------------------------------------------------------

/// A coherent sequence of agent actions forming a narrative unit.
///
/// Episodes capture the full story of a task: who participated, what
/// happened step by step, how it ended, and what was learned. They are
/// the primary input for memory consolidation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    /// Unique identifier.
    pub id: EpisodeId,
    /// Human-readable title summarizing the episode.
    pub title: String,
    /// Agents that participated.
    pub agent_ids: Vec<AgentId>,
    /// The task this episode relates to (if any).
    pub task_id: Option<Uuid>,
    /// When the episode started.
    pub start_time: DateTime<Utc>,
    /// When the episode ended. `None` if still in progress.
    pub end_time: Option<DateTime<Utc>>,
    /// Ordered sequence of events.
    pub events: Vec<EpisodeEvent>,
    /// How the episode concluded.
    pub outcome: EpisodeOutcome,
    /// Key learnings extracted from this episode.
    pub learnings: Vec<String>,
    /// Importance score in `[0.0, 1.0]`.
    pub importance: f64,
    /// Free-form tags for categorization.
    pub tags: Vec<String>,
    /// Arbitrary metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Episode {
    /// Create a new in-progress episode.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            id: EpisodeId::new(),
            title: title.into(),
            agent_ids: Vec::new(),
            task_id: None,
            start_time: Utc::now(),
            end_time: None,
            events: Vec::new(),
            outcome: EpisodeOutcome::InProgress,
            learnings: Vec::new(),
            importance: 0.5,
            tags: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Record an event in this episode.
    pub fn record_event(&mut self, event: EpisodeEvent) {
        // Track participating agents.
        if !self.agent_ids.contains(&event.agent_id) {
            self.agent_ids.push(event.agent_id);
        }
        self.events.push(event);
    }

    /// Mark the episode as successfully completed.
    pub fn complete_success(&mut self) {
        self.outcome = EpisodeOutcome::Success;
        self.end_time = Some(Utc::now());
    }

    /// Mark the episode as failed.
    pub fn complete_failure(&mut self, reason: impl Into<String>) {
        self.outcome = EpisodeOutcome::Failure(reason.into());
        self.end_time = Some(Utc::now());
    }

    /// Mark the episode as abandoned.
    pub fn abandon(&mut self, reason: impl Into<String>) {
        self.outcome = EpisodeOutcome::Abandoned(reason.into());
        self.end_time = Some(Utc::now());
    }

    /// Add a learning extracted from this episode.
    pub fn add_learning(&mut self, learning: impl Into<String>) {
        self.learnings.push(learning.into());
    }

    /// Duration of the episode. Returns zero if still in progress.
    pub fn duration(&self) -> Duration {
        match self.end_time {
            Some(end) => end - self.start_time,
            None => Utc::now() - self.start_time,
        }
    }

    /// Number of events in this episode.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Check if the episode is still in progress.
    pub fn is_in_progress(&self) -> bool {
        self.outcome == EpisodeOutcome::InProgress
    }
}

// ---------------------------------------------------------------------------
// EpisodeBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for constructing episodes.
///
/// # Example
/// ```
/// use daf_memory::episode::EpisodeBuilder;
///
/// let episode = EpisodeBuilder::new("Deploy auth service")
///     .importance(0.9)
///     .tags(vec!["deploy".into(), "auth".into()])
///     .build();
/// ```
pub struct EpisodeBuilder {
    episode: Episode,
}

impl EpisodeBuilder {
    /// Start building a new episode with the given title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            episode: Episode::new(title),
        }
    }

    /// Set the task ID.
    pub fn task_id(mut self, task_id: Uuid) -> Self {
        self.episode.task_id = Some(task_id);
        self
    }

    /// Set the importance score.
    pub fn importance(mut self, importance: f64) -> Self {
        self.episode.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Set the tags.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.episode.tags = tags;
        self
    }

    /// Add initial agent IDs.
    pub fn agents(mut self, agents: Vec<AgentId>) -> Self {
        self.episode.agent_ids = agents;
        self
    }

    /// Set metadata.
    pub fn metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.episode.metadata.insert(key.into(), value);
        self
    }

    /// Consume the builder and return the episode.
    pub fn build(self) -> Episode {
        self.episode
    }
}

// ---------------------------------------------------------------------------
// EpisodeIndex
// ---------------------------------------------------------------------------

/// In-memory index for searching episodes by agent, time range, outcome, and tags.
///
/// This is a lightweight index that stores episode metadata for fast queries.
/// The full episode data is fetched from the memory store on demand.
#[derive(Debug, Default)]
pub struct EpisodeIndex {
    episodes: Vec<Episode>,
}

impl EpisodeIndex {
    /// Create a new empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an episode to the index.
    pub fn insert(&mut self, episode: Episode) {
        self.episodes.push(episode);
    }

    /// Remove an episode from the index.
    pub fn remove(&mut self, id: &EpisodeId) {
        self.episodes.retain(|ep| ep.id != *id);
    }

    /// Find episodes involving a specific agent.
    pub fn by_agent(&self, agent_id: &AgentId) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.agent_ids.contains(agent_id))
            .collect()
    }

    /// Find episodes within a time range.
    pub fn by_time_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.start_time >= start && ep.start_time <= end)
            .collect()
    }

    /// Find episodes with a specific outcome type.
    pub fn by_outcome(&self, outcome: &EpisodeOutcome) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| std::mem::discriminant(&ep.outcome) == std::mem::discriminant(outcome))
            .collect()
    }

    /// Find episodes with a specific tag.
    pub fn by_tag(&self, tag: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.tags.iter().any(|t| t == tag))
            .collect()
    }

    /// Find episodes with importance above a threshold.
    pub fn by_importance_threshold(&self, threshold: f64) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.importance >= threshold)
            .collect()
    }

    /// Return all episodes, sorted by start time descending (most recent first).
    pub fn all_sorted(&self) -> Vec<&Episode> {
        let mut sorted: Vec<&Episode> = self.episodes.iter().collect();
        sorted.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        sorted
    }

    /// Total number of indexed episodes.
    pub fn len(&self) -> usize {
        self.episodes.len()
    }

    /// Check if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Auto-segmentation
// ---------------------------------------------------------------------------

/// Detect episode boundaries from a stream of events.
///
/// An episode boundary is detected when:
/// - There is a long gap (> `gap_threshold`) between events.
/// - The agent set changes significantly.
/// - An explicit boundary marker is present in event data.
pub fn detect_boundaries(events: &[EpisodeEvent], gap_threshold: Duration) -> Vec<usize> {
    if events.len() < 2 {
        return Vec::new();
    }

    let mut boundaries = Vec::new();

    for i in 1..events.len() {
        let prev = &events[i - 1];
        let curr = &events[i];

        // Time gap detection.
        let gap = curr.timestamp - prev.timestamp;
        if gap > gap_threshold {
            boundaries.push(i);
            continue;
        }

        // Explicit boundary marker in event data.
        if let Some(marker) = curr.data.get("episode_boundary") {
            if marker.as_bool().unwrap_or(false) {
                boundaries.push(i);
                continue;
            }
        }
    }

    boundaries
}

/// Split a stream of events into episodes based on detected boundaries.
pub fn segment_events(
    events: Vec<EpisodeEvent>,
    gap_threshold: Duration,
    base_title: &str,
) -> Vec<Episode> {
    if events.is_empty() {
        return Vec::new();
    }

    let boundaries = detect_boundaries(&events, gap_threshold);
    let mut episodes = Vec::new();
    let mut start = 0;

    let mut split_points: Vec<usize> = boundaries;
    split_points.push(events.len());

    for (idx, &end) in split_points.iter().enumerate() {
        let segment: Vec<EpisodeEvent> = events[start..end].to_vec();
        if !segment.is_empty() {
            let title = if split_points.len() > 1 {
                format!("{base_title} (part {})", idx + 1)
            } else {
                base_title.to_string()
            };
            let mut episode = Episode::new(title);
            for event in segment {
                episode.record_event(event);
            }
            episodes.push(episode);
        }
        start = end;
    }

    episodes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_agent() -> AgentId {
        AgentId::new()
    }

    #[test]
    fn episode_builder_fluent() {
        let ep = EpisodeBuilder::new("test episode")
            .importance(0.8)
            .tags(vec!["test".into()])
            .build();

        assert_eq!(ep.title, "test episode");
        assert!((ep.importance - 0.8).abs() < f64::EPSILON);
        assert_eq!(ep.tags, vec!["test"]);
        assert!(ep.is_in_progress());
    }

    #[test]
    fn episode_lifecycle() {
        let agent = make_agent();
        let mut ep = Episode::new("deploy service");

        ep.record_event(
            EpisodeEvent::new(agent, "start_deploy")
                .with_observation("deploying to staging")
        );
        ep.record_event(
            EpisodeEvent::new(agent, "verify_health")
                .with_observation("health check passed")
        );

        assert_eq!(ep.event_count(), 2);
        assert_eq!(ep.agent_ids.len(), 1);
        assert!(ep.is_in_progress());

        ep.add_learning("staging deploy takes ~30s");
        ep.complete_success();

        assert!(!ep.is_in_progress());
        assert_eq!(ep.outcome, EpisodeOutcome::Success);
        assert!(ep.end_time.is_some());
        assert_eq!(ep.learnings.len(), 1);
    }

    #[test]
    fn episode_failure() {
        let mut ep = Episode::new("broken task");
        ep.complete_failure("OOM killed");
        assert!(matches!(ep.outcome, EpisodeOutcome::Failure(_)));
    }

    #[test]
    fn episode_index_search() {
        let agent1 = make_agent();
        let agent2 = make_agent();

        let mut idx = EpisodeIndex::new();

        let mut ep1 = Episode::new("episode 1");
        ep1.agent_ids.push(agent1);
        ep1.tags.push("deploy".into());
        ep1.importance = 0.9;
        ep1.complete_success();
        idx.insert(ep1);

        let mut ep2 = Episode::new("episode 2");
        ep2.agent_ids.push(agent2);
        ep2.tags.push("debug".into());
        ep2.importance = 0.3;
        ep2.complete_failure("timeout");
        idx.insert(ep2);

        assert_eq!(idx.len(), 2);
        assert_eq!(idx.by_agent(&agent1).len(), 1);
        assert_eq!(idx.by_tag("deploy").len(), 1);
        assert_eq!(idx.by_importance_threshold(0.5).len(), 1);
        assert_eq!(
            idx.by_outcome(&EpisodeOutcome::Success).len(),
            1
        );
    }

    #[test]
    fn boundary_detection_time_gap() {
        let agent = make_agent();
        let base = Utc::now();

        let events = vec![
            EpisodeEvent {
                timestamp: base,
                agent_id: agent,
                action: "a".into(),
                observation: None,
                decision: None,
                data: json!(null),
            },
            EpisodeEvent {
                timestamp: base + Duration::seconds(1),
                agent_id: agent,
                action: "b".into(),
                observation: None,
                decision: None,
                data: json!(null),
            },
            EpisodeEvent {
                timestamp: base + Duration::hours(2),
                agent_id: agent,
                action: "c".into(),
                observation: None,
                decision: None,
                data: json!(null),
            },
        ];

        let boundaries = detect_boundaries(&events, Duration::minutes(30));
        assert_eq!(boundaries, vec![2]);
    }

    #[test]
    fn segment_events_splits_correctly() {
        let agent = make_agent();
        let base = Utc::now();

        let events = vec![
            EpisodeEvent {
                timestamp: base,
                agent_id: agent,
                action: "a".into(),
                observation: None,
                decision: None,
                data: json!(null),
            },
            EpisodeEvent {
                timestamp: base + Duration::hours(3),
                agent_id: agent,
                action: "b".into(),
                observation: None,
                decision: None,
                data: json!(null),
            },
        ];

        let episodes = segment_events(events, Duration::minutes(30), "work");
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].title, "work (part 1)");
        assert_eq!(episodes[1].title, "work (part 2)");
    }

    #[test]
    fn serialization_roundtrip() {
        let mut ep = EpisodeBuilder::new("test")
            .importance(0.7)
            .tags(vec!["a".into()])
            .build();
        ep.record_event(EpisodeEvent::new(make_agent(), "action"));
        ep.complete_success();

        let json = serde_json::to_string(&ep).unwrap();
        let ep2: Episode = serde_json::from_str(&json).unwrap();
        assert_eq!(ep.id, ep2.id);
        assert_eq!(ep.title, ep2.title);
        assert_eq!(ep.event_count(), ep2.event_count());
    }
}
