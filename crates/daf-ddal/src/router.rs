//! # Message Routing
//!
//! The router is responsible for deciding *where* a frame goes once it arrives
//! at a node. It maintains a routing table mapping agent identifiers to
//! channels, supports topic-based pub/sub, and implements load-balancing
//! strategies for agent groups.
//!
//! ## Routing strategies
//!
//! | Strategy       | Use case                                       |
//! |----------------|-------------------------------------------------|
//! | Direct         | Point-to-point: source knows the target agent   |
//! | Topic pub/sub  | Fan-out: publish to a topic, all subscribers get |
//! | Round-robin    | Load-balance across a group of equivalent agents |
//! | Least-loaded   | Route to the agent with fewest pending messages  |
//!
//! ## Thread safety
//!
//! All data structures use [`DashMap`] for lock-free concurrent reads with
//! fine-grained write locks, making the router safe for multi-threaded
//! dispatch without a global mutex.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

use daf_core::AgentId;

// ---------------------------------------------------------------------------
// RouteEntry
// ---------------------------------------------------------------------------

/// A single entry in the routing table, representing a path to a remote agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    /// The agent this route leads to.
    pub target_agent: AgentId,
    /// The channel ID to use when sending frames to this agent.
    pub channel_id: u32,
    /// Measured round-trip latency in microseconds. Updated by health probes.
    pub latency_us: u64,
    /// Routing priority (lower = preferred). Used for failover ordering.
    pub priority: u8,
    /// Wall-clock time of the last frame received from this agent.
    pub last_seen: DateTime<Utc>,
    /// Number of messages currently in-flight to this agent.
    pub pending_count: u64,
}

impl RouteEntry {
    /// Create a new route entry with default latency and priority.
    pub fn new(target_agent: AgentId, channel_id: u32) -> Self {
        Self {
            target_agent,
            channel_id,
            latency_us: 0,
            priority: 128, // middle of the range
            last_seen: Utc::now(),
            pending_count: 0,
        }
    }

    /// Update the latency measurement.
    pub fn with_latency(mut self, latency_us: u64) -> Self {
        self.latency_us = latency_us;
        self
    }

    /// Set the routing priority.
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Mark the route as recently active.
    pub fn touch(&mut self) {
        self.last_seen = Utc::now();
    }

    /// Returns `true` if the route has not been seen within `timeout`.
    pub fn is_stale(&self, timeout: chrono::Duration) -> bool {
        Utc::now() - self.last_seen > timeout
    }
}

impl fmt::Display for RouteEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Route({} via ch={}, lat={}us, pri={})",
            self.target_agent, self.channel_id, self.latency_us, self.priority,
        )
    }
}

// ---------------------------------------------------------------------------
// BalancingStrategy
// ---------------------------------------------------------------------------

/// Load-balancing strategy for distributing messages across a group of agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalancingStrategy {
    /// Cycle through agents in order. Simple, predictable, no hot-spots on
    /// uniform workloads.
    RoundRobin,
    /// Pick the agent with the fewest pending messages. Adapts to heterogeneous
    /// processing speeds at the cost of tracking in-flight counts.
    LeastLoaded,
    /// Pick the agent with the lowest measured latency. Good for geo-distributed
    /// deployments where network cost dominates.
    LowestLatency,
    /// Pick a random agent. Statistically uniform but can produce short-term
    /// imbalance.
    Random,
}

impl Default for BalancingStrategy {
    fn default() -> Self {
        Self::RoundRobin
    }
}

// ---------------------------------------------------------------------------
// RoutingTable
// ---------------------------------------------------------------------------

/// Core routing table mapping agent IDs to their route entries.
///
/// An agent may have multiple routes (e.g. via different connections or through
/// different relay nodes), so the table maps each agent to a `Vec<RouteEntry>`.
#[derive(Debug)]
pub struct RoutingTable {
    /// Agent ID → list of known routes.
    routes: DashMap<AgentId, Vec<RouteEntry>>,
}

impl RoutingTable {
    /// Create an empty routing table.
    pub fn new() -> Self {
        Self {
            routes: DashMap::new(),
        }
    }

    /// Register a route to an agent.
    ///
    /// If a route to the same agent via the same channel already exists, it
    /// is replaced (updated). Otherwise, the new route is appended.
    pub fn insert(&self, entry: RouteEntry) {
        let agent = entry.target_agent;
        self.routes
            .entry(agent)
            .and_modify(|entries| {
                if let Some(existing) = entries
                    .iter_mut()
                    .find(|e| e.channel_id == entry.channel_id)
                {
                    *existing = entry.clone();
                } else {
                    entries.push(entry.clone());
                }
            })
            .or_insert_with(|| vec![entry]);
    }

    /// Remove all routes to a specific agent.
    pub fn remove_agent(&self, agent: &AgentId) -> Option<Vec<RouteEntry>> {
        self.routes.remove(agent).map(|(_, entries)| entries)
    }

    /// Remove a specific route (by channel_id) to an agent.
    pub fn remove_route(&self, agent: &AgentId, channel_id: u32) -> bool {
        if let Some(mut entries) = self.routes.get_mut(agent) {
            let before = entries.len();
            entries.retain(|e| e.channel_id != channel_id);
            let removed = entries.len() < before;

            // If no routes remain, remove the agent entirely.
            if entries.is_empty() {
                drop(entries); // release the DashMap ref before removing
                self.routes.remove(agent);
            }
            removed
        } else {
            false
        }
    }

    /// Look up all routes to an agent.
    pub fn get(&self, agent: &AgentId) -> Option<Vec<RouteEntry>> {
        self.routes.get(agent).map(|r| r.value().clone())
    }

    /// Look up the best (lowest-priority) route to an agent.
    pub fn best_route(&self, agent: &AgentId) -> Option<RouteEntry> {
        self.routes.get(agent).and_then(|entries| {
            entries.iter().min_by_key(|e| e.priority).cloned()
        })
    }

    /// Number of distinct agents in the table.
    pub fn agent_count(&self) -> usize {
        self.routes.len()
    }

    /// Total number of route entries across all agents.
    pub fn route_count(&self) -> usize {
        self.routes
            .iter()
            .map(|entry| entry.value().len())
            .sum()
    }

    /// Remove all stale routes older than `timeout`.
    pub fn evict_stale(&self, timeout: chrono::Duration) -> usize {
        let mut evicted = 0;
        let mut empty_agents = Vec::new();

        for mut entry in self.routes.iter_mut() {
            let before = entry.value().len();
            entry.value_mut().retain(|e| !e.is_stale(timeout));
            evicted += before - entry.value().len();
            if entry.value().is_empty() {
                empty_agents.push(*entry.key());
            }
        }

        for agent in empty_agents {
            self.routes.remove(&agent);
        }

        evicted
    }

    /// Iterate over all agents with routes.
    pub fn agents(&self) -> Vec<AgentId> {
        self.routes.iter().map(|e| *e.key()).collect()
    }
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TopicSubscription
// ---------------------------------------------------------------------------

/// Topic-based pub/sub subscription table.
///
/// Topics are arbitrary strings (e.g. `"llm.responses"`, `"task.completed"`).
/// Multiple agents can subscribe to the same topic, and a single agent can
/// subscribe to multiple topics.
#[derive(Debug)]
pub struct TopicRegistry {
    /// Topic name → set of subscribed agents.
    subscriptions: DashMap<String, Vec<AgentId>>,
}

impl TopicRegistry {
    /// Create an empty topic registry.
    pub fn new() -> Self {
        Self {
            subscriptions: DashMap::new(),
        }
    }

    /// Subscribe an agent to a topic.
    pub fn subscribe(&self, topic: impl Into<String>, agent: AgentId) {
        let topic = topic.into();
        self.subscriptions
            .entry(topic)
            .and_modify(|subscribers| {
                if !subscribers.contains(&agent) {
                    subscribers.push(agent);
                }
            })
            .or_insert_with(|| vec![agent]);
    }

    /// Unsubscribe an agent from a topic.
    pub fn unsubscribe(&self, topic: &str, agent: &AgentId) -> bool {
        if let Some(mut subscribers) = self.subscriptions.get_mut(topic) {
            let before = subscribers.len();
            subscribers.retain(|a| a != agent);
            let removed = subscribers.len() < before;

            if subscribers.is_empty() {
                drop(subscribers);
                self.subscriptions.remove(topic);
            }

            removed
        } else {
            false
        }
    }

    /// Unsubscribe an agent from all topics.
    pub fn unsubscribe_all(&self, agent: &AgentId) -> usize {
        let mut count = 0;
        let mut empty_topics = Vec::new();

        for mut entry in self.subscriptions.iter_mut() {
            let before = entry.value().len();
            entry.value_mut().retain(|a| a != agent);
            count += before - entry.value().len();
            if entry.value().is_empty() {
                empty_topics.push(entry.key().clone());
            }
        }

        for topic in empty_topics {
            self.subscriptions.remove(&topic);
        }

        count
    }

    /// Get all agents subscribed to a topic.
    pub fn subscribers(&self, topic: &str) -> Vec<AgentId> {
        self.subscriptions
            .get(topic)
            .map(|s| s.value().clone())
            .unwrap_or_default()
    }

    /// Get all topics an agent is subscribed to.
    pub fn topics_for_agent(&self, agent: &AgentId) -> Vec<String> {
        self.subscriptions
            .iter()
            .filter(|entry| entry.value().contains(agent))
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Number of distinct topics.
    pub fn topic_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Total number of subscriptions across all topics.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions
            .iter()
            .map(|entry| entry.value().len())
            .sum()
    }
}

impl Default for TopicRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// The central message router combining direct routing, topic pub/sub, and
/// load-balancing.
///
/// The router does **not** send frames itself — it resolves destinations.
/// The caller (transport layer) takes the resolved channel IDs and pushes
/// frames through the appropriate channels.
pub struct Router {
    /// Direct agent-to-channel routing table.
    pub routing_table: RoutingTable,
    /// Topic-based subscription registry.
    pub topics: TopicRegistry,
    /// Default load-balancing strategy for agent groups.
    balancing_strategy: BalancingStrategy,
    /// Round-robin counter, used atomically.
    rr_counter: AtomicU64,
}

impl Router {
    /// Create a new router with default settings.
    pub fn new() -> Self {
        Self {
            routing_table: RoutingTable::new(),
            topics: TopicRegistry::new(),
            balancing_strategy: BalancingStrategy::RoundRobin,
            rr_counter: AtomicU64::new(0),
        }
    }

    /// Create a router with a specific default balancing strategy.
    pub fn with_strategy(strategy: BalancingStrategy) -> Self {
        Self {
            routing_table: RoutingTable::new(),
            topics: TopicRegistry::new(),
            balancing_strategy: strategy,
            rr_counter: AtomicU64::new(0),
        }
    }

    // -- Route management ---------------------------------------------------

    /// Register a route to an agent.
    #[instrument(skip(self), fields(agent = %entry.target_agent, channel = entry.channel_id))]
    pub fn register_route(&self, entry: RouteEntry) {
        debug!("registering route");
        self.routing_table.insert(entry);
    }

    /// Remove a specific route.
    pub fn remove_route(&self, agent: &AgentId, channel_id: u32) -> bool {
        self.routing_table.remove_route(agent, channel_id)
    }

    /// Remove all routes to an agent and unsubscribe from all topics.
    pub fn deregister_agent(&self, agent: &AgentId) {
        self.routing_table.remove_agent(agent);
        self.topics.unsubscribe_all(agent);
    }

    // -- Resolution ---------------------------------------------------------

    /// Resolve a direct route to a single agent.
    ///
    /// Returns the best (lowest-priority) route entry, or `None` if the agent
    /// is not in the routing table.
    pub fn resolve(&self, target: &AgentId) -> Option<RouteEntry> {
        self.routing_table.best_route(target)
    }

    /// Resolve routes for broadcasting to **all** known agents.
    ///
    /// Returns the best route for each agent in the table. Useful for global
    /// announcements.
    pub fn broadcast(&self) -> Vec<RouteEntry> {
        self.routing_table
            .agents()
            .into_iter()
            .filter_map(|agent| self.routing_table.best_route(&agent))
            .collect()
    }

    /// Resolve routes for multicasting to a specific set of agents.
    pub fn multicast(&self, targets: &[AgentId]) -> Vec<RouteEntry> {
        targets
            .iter()
            .filter_map(|agent| self.routing_table.best_route(agent))
            .collect()
    }

    /// Resolve routes for all subscribers of a topic.
    pub fn resolve_topic(&self, topic: &str) -> Vec<RouteEntry> {
        let subscribers = self.topics.subscribers(topic);
        self.multicast(&subscribers)
    }

    // -- Load balancing -----------------------------------------------------

    /// Select one agent from a group using the configured balancing strategy.
    ///
    /// This is the core load-balancing primitive. Pass in the set of candidate
    /// agents (e.g. all agents with capability `"code_review"`), and the router
    /// picks one.
    pub fn select_from_group(&self, candidates: &[AgentId]) -> Option<RouteEntry> {
        if candidates.is_empty() {
            return None;
        }

        match self.balancing_strategy {
            BalancingStrategy::RoundRobin => self.select_round_robin(candidates),
            BalancingStrategy::LeastLoaded => self.select_least_loaded(candidates),
            BalancingStrategy::LowestLatency => self.select_lowest_latency(candidates),
            BalancingStrategy::Random => self.select_random(candidates),
        }
    }

    fn select_round_robin(&self, candidates: &[AgentId]) -> Option<RouteEntry> {
        let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) as usize;
        let agent = &candidates[idx % candidates.len()];
        self.routing_table.best_route(agent)
    }

    fn select_least_loaded(&self, candidates: &[AgentId]) -> Option<RouteEntry> {
        candidates
            .iter()
            .filter_map(|agent| self.routing_table.best_route(agent))
            .min_by_key(|entry| entry.pending_count)
    }

    fn select_lowest_latency(&self, candidates: &[AgentId]) -> Option<RouteEntry> {
        candidates
            .iter()
            .filter_map(|agent| self.routing_table.best_route(agent))
            .min_by_key(|entry| entry.latency_us)
    }

    fn select_random(&self, candidates: &[AgentId]) -> Option<RouteEntry> {
        // Use a simple counter-based pseudo-random for determinism in tests.
        // A real deployment would use `rand::thread_rng()` but we avoid the
        // dependency in this crate.
        let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) as usize;
        let agent = &candidates[idx % candidates.len()];
        self.routing_table.best_route(agent)
    }

    // -- Topic management ---------------------------------------------------

    /// Subscribe an agent to a topic.
    pub fn subscribe(&self, topic: impl Into<String>, agent: AgentId) {
        self.topics.subscribe(topic, agent);
    }

    /// Unsubscribe an agent from a topic.
    pub fn unsubscribe(&self, topic: &str, agent: &AgentId) -> bool {
        self.topics.unsubscribe(topic, agent)
    }

    // -- Introspection ------------------------------------------------------

    /// Get the current balancing strategy.
    pub fn balancing_strategy(&self) -> BalancingStrategy {
        self.balancing_strategy
    }

    /// Set the balancing strategy.
    pub fn set_balancing_strategy(&mut self, strategy: BalancingStrategy) {
        self.balancing_strategy = strategy;
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field("agents", &self.routing_table.agent_count())
            .field("routes", &self.routing_table.route_count())
            .field("topics", &self.topics.topic_count())
            .field("strategy", &self.balancing_strategy)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentId {
        AgentId::new()
    }

    // -- RoutingTable -------------------------------------------------------

    #[test]
    fn routing_table_insert_and_get() {
        let table = RoutingTable::new();
        let a = agent();
        let entry = RouteEntry::new(a, 1);

        table.insert(entry.clone());
        let routes = table.get(&a).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].channel_id, 1);
    }

    #[test]
    fn routing_table_multiple_routes_per_agent() {
        let table = RoutingTable::new();
        let a = agent();

        table.insert(RouteEntry::new(a, 1).with_priority(10));
        table.insert(RouteEntry::new(a, 2).with_priority(5));

        let routes = table.get(&a).unwrap();
        assert_eq!(routes.len(), 2);

        let best = table.best_route(&a).unwrap();
        assert_eq!(best.priority, 5);
        assert_eq!(best.channel_id, 2);
    }

    #[test]
    fn routing_table_upsert_same_channel() {
        let table = RoutingTable::new();
        let a = agent();

        table.insert(RouteEntry::new(a, 1).with_latency(100));
        table.insert(RouteEntry::new(a, 1).with_latency(50));

        let routes = table.get(&a).unwrap();
        assert_eq!(routes.len(), 1, "should replace, not duplicate");
        assert_eq!(routes[0].latency_us, 50);
    }

    #[test]
    fn routing_table_remove_agent() {
        let table = RoutingTable::new();
        let a = agent();
        table.insert(RouteEntry::new(a, 1));

        let removed = table.remove_agent(&a);
        assert!(removed.is_some());
        assert_eq!(table.agent_count(), 0);
    }

    #[test]
    fn routing_table_remove_route() {
        let table = RoutingTable::new();
        let a = agent();
        table.insert(RouteEntry::new(a, 1));
        table.insert(RouteEntry::new(a, 2));

        assert!(table.remove_route(&a, 1));
        let routes = table.get(&a).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].channel_id, 2);

        // Remove last route should clean up the agent.
        assert!(table.remove_route(&a, 2));
        assert_eq!(table.agent_count(), 0);
    }

    #[test]
    fn routing_table_evict_stale() {
        let table = RoutingTable::new();
        let a = agent();
        let mut entry = RouteEntry::new(a, 1);
        // Backdate last_seen by 2 hours.
        entry.last_seen = Utc::now() - chrono::Duration::hours(2);
        table.insert(entry);

        let evicted = table.evict_stale(chrono::Duration::hours(1));
        assert_eq!(evicted, 1);
        assert_eq!(table.agent_count(), 0);
    }

    // -- TopicRegistry -------------------------------------------------------

    #[test]
    fn topic_subscribe_and_get() {
        let registry = TopicRegistry::new();
        let a = agent();

        registry.subscribe("events", a);
        let subs = registry.subscribers("events");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0], a);
    }

    #[test]
    fn topic_no_duplicate_subscriptions() {
        let registry = TopicRegistry::new();
        let a = agent();

        registry.subscribe("events", a);
        registry.subscribe("events", a);
        assert_eq!(registry.subscribers("events").len(), 1);
    }

    #[test]
    fn topic_unsubscribe() {
        let registry = TopicRegistry::new();
        let a = agent();

        registry.subscribe("events", a);
        assert!(registry.unsubscribe("events", &a));
        assert!(registry.subscribers("events").is_empty());
        assert_eq!(registry.topic_count(), 0);
    }

    #[test]
    fn topic_unsubscribe_all() {
        let registry = TopicRegistry::new();
        let a = agent();

        registry.subscribe("events", a);
        registry.subscribe("tasks", a);
        let count = registry.unsubscribe_all(&a);
        assert_eq!(count, 2);
        assert_eq!(registry.topic_count(), 0);
    }

    #[test]
    fn topic_topics_for_agent() {
        let registry = TopicRegistry::new();
        let a = agent();

        registry.subscribe("alpha", a);
        registry.subscribe("beta", a);

        let mut topics = registry.topics_for_agent(&a);
        topics.sort();
        assert_eq!(topics, vec!["alpha", "beta"]);
    }

    // -- Router -------------------------------------------------------------

    #[test]
    fn router_resolve_direct() {
        let router = Router::new();
        let a = agent();

        router.register_route(RouteEntry::new(a, 1));
        let resolved = router.resolve(&a).unwrap();
        assert_eq!(resolved.channel_id, 1);
    }

    #[test]
    fn router_broadcast() {
        let router = Router::new();
        let a = agent();
        let b = agent();

        router.register_route(RouteEntry::new(a, 1));
        router.register_route(RouteEntry::new(b, 2));

        let routes = router.broadcast();
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn router_multicast() {
        let router = Router::new();
        let a = agent();
        let b = agent();
        let c = agent();

        router.register_route(RouteEntry::new(a, 1));
        router.register_route(RouteEntry::new(b, 2));
        router.register_route(RouteEntry::new(c, 3));

        let routes = router.multicast(&[a, c]);
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn router_topic_resolve() {
        let router = Router::new();
        let a = agent();
        let b = agent();

        router.register_route(RouteEntry::new(a, 1));
        router.register_route(RouteEntry::new(b, 2));
        router.subscribe("updates", a);
        router.subscribe("updates", b);

        let routes = router.resolve_topic("updates");
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn router_round_robin_selection() {
        let router = Router::new();
        let a = agent();
        let b = agent();

        router.register_route(RouteEntry::new(a, 1));
        router.register_route(RouteEntry::new(b, 2));

        let candidates = vec![a, b];
        let first = router.select_from_group(&candidates).unwrap();
        let second = router.select_from_group(&candidates).unwrap();
        // Should select different agents on consecutive calls.
        assert_ne!(first.channel_id, second.channel_id);
    }

    #[test]
    fn router_least_loaded_selection() {
        let router = Router::with_strategy(BalancingStrategy::LeastLoaded);
        let a = agent();
        let b = agent();

        let mut entry_a = RouteEntry::new(a, 1);
        entry_a.pending_count = 10;
        let mut entry_b = RouteEntry::new(b, 2);
        entry_b.pending_count = 2;

        router.register_route(entry_a);
        router.register_route(entry_b);

        let selected = router.select_from_group(&[a, b]).unwrap();
        assert_eq!(selected.channel_id, 2, "should pick least loaded");
    }

    #[test]
    fn router_lowest_latency_selection() {
        let router = Router::with_strategy(BalancingStrategy::LowestLatency);
        let a = agent();
        let b = agent();

        router.register_route(RouteEntry::new(a, 1).with_latency(500));
        router.register_route(RouteEntry::new(b, 2).with_latency(100));

        let selected = router.select_from_group(&[a, b]).unwrap();
        assert_eq!(selected.channel_id, 2, "should pick lowest latency");
    }

    #[test]
    fn router_deregister_agent() {
        let router = Router::new();
        let a = agent();

        router.register_route(RouteEntry::new(a, 1));
        router.subscribe("events", a);

        router.deregister_agent(&a);
        assert!(router.resolve(&a).is_none());
        assert!(router.topics.subscribers("events").is_empty());
    }

    #[test]
    fn router_empty_group_returns_none() {
        let router = Router::new();
        assert!(router.select_from_group(&[]).is_none());
    }

    #[test]
    fn router_debug_output() {
        let router = Router::new();
        let debug = format!("{router:?}");
        assert!(debug.contains("Router"));
        assert!(debug.contains("agents"));
    }
}
