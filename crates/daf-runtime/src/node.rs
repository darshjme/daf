//! Node identity and cluster peer tracking.
//!
//! A [`Node`] represents this runtime instance in a cluster. [`NodeInfo`] is
//! the serializable snapshot exchanged during cluster discovery.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::VERSION;

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// Identity of this runtime instance.
#[derive(Debug, Clone)]
pub struct Node {
    /// Unique identifier for this node (generated at boot).
    pub id: Uuid,

    /// Human-readable name (from config).
    pub name: String,

    /// The address this node is listening on.
    pub address: SocketAddr,

    /// Wall-clock time when this node started.
    pub started_at: DateTime<Utc>,

    /// DAF version running on this node.
    pub version: String,

    /// Arbitrary metadata labels.
    pub labels: HashMap<String, String>,
}

impl Node {
    /// Create a new node identity.
    pub fn new(name: impl Into<String>, address: SocketAddr) -> Self {
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            address,
            started_at: Utc::now(),
            version: VERSION.to_string(),
            labels: HashMap::new(),
        }
    }

    /// Attach a label.
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// How long this node has been running.
    pub fn uptime(&self) -> Duration {
        (Utc::now() - self.started_at)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }

    /// Produce a serializable [`NodeInfo`] snapshot.
    pub fn info(&self) -> NodeInfo {
        NodeInfo {
            id: self.id,
            name: self.name.clone(),
            address: self.address,
            started_at: self.started_at,
            version: self.version.clone(),
            labels: self.labels.clone(),
        }
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Node({name} @ {addr}, id={id})",
            name = self.name,
            addr = self.address,
            id = &self.id.to_string()[..8],
        )
    }
}

// ---------------------------------------------------------------------------
// NodeInfo
// ---------------------------------------------------------------------------

/// Serializable node information exchanged during cluster discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique node identifier.
    pub id: Uuid,

    /// Human-readable name.
    pub name: String,

    /// The address this node is reachable at.
    pub address: SocketAddr,

    /// When this node was started.
    pub started_at: DateTime<Utc>,

    /// DAF version.
    pub version: String,

    /// Arbitrary labels.
    pub labels: HashMap<String, String>,
}

impl fmt::Display for NodeInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{name} ({addr}) v{ver}",
            name = self.name,
            addr = self.address,
            ver = self.version,
        )
    }
}

// ---------------------------------------------------------------------------
// PeerStatus
// ---------------------------------------------------------------------------

/// Status of a peer node in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    /// Peer is reachable and healthy.
    Alive,
    /// Peer has not responded recently but is not yet considered dead.
    Suspected,
    /// Peer has been unreachable for longer than the stale threshold.
    Dead,
    /// Peer announced it is leaving the cluster.
    Left,
}

impl fmt::Display for PeerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Alive => "alive",
            Self::Suspected => "suspected",
            Self::Dead => "dead",
            Self::Left => "left",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// PeerEntry
// ---------------------------------------------------------------------------

/// A tracked peer in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerEntry {
    /// Information about the peer.
    pub info: NodeInfo,
    /// Current status.
    pub status: PeerStatus,
    /// When we last heard from this peer.
    pub last_seen: DateTime<Utc>,
    /// Number of consecutive failed health checks.
    pub failed_checks: u32,
}

impl PeerEntry {
    /// Create a new peer entry marked as alive.
    pub fn new(info: NodeInfo) -> Self {
        Self {
            info,
            status: PeerStatus::Alive,
            last_seen: Utc::now(),
            failed_checks: 0,
        }
    }

    /// Record a successful heartbeat.
    pub fn heartbeat(&mut self) {
        self.last_seen = Utc::now();
        self.failed_checks = 0;
        self.status = PeerStatus::Alive;
    }

    /// Record a failed health check.
    pub fn fail_check(&mut self) {
        self.failed_checks += 1;
        if self.failed_checks >= 3 {
            self.status = PeerStatus::Dead;
        } else {
            self.status = PeerStatus::Suspected;
        }
    }

    /// Duration since last contact.
    pub fn since_last_seen(&self) -> Duration {
        (Utc::now() - self.last_seen)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }
}

// ---------------------------------------------------------------------------
// PeerTracker
// ---------------------------------------------------------------------------

/// Thread-safe tracker for cluster peers.
///
/// Maintains a map of known peers and their status. Used by the runtime
/// for cluster membership and health monitoring.
#[derive(Debug, Clone)]
pub struct PeerTracker {
    peers: DashMap<Uuid, PeerEntry>,
}

impl PeerTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            peers: DashMap::new(),
        }
    }

    /// Register a new peer or update an existing one.
    pub fn upsert(&self, info: NodeInfo) {
        let id = info.id;
        self.peers
            .entry(id)
            .and_modify(|entry| {
                entry.info = info.clone();
                entry.heartbeat();
            })
            .or_insert_with(|| {
                debug!(peer_id = %id, "new peer registered");
                PeerEntry::new(info)
            });
    }

    /// Record a heartbeat from a peer.
    pub fn heartbeat(&self, peer_id: Uuid) {
        if let Some(mut entry) = self.peers.get_mut(&peer_id) {
            entry.heartbeat();
        }
    }

    /// Record a failed health check for a peer.
    pub fn fail_check(&self, peer_id: Uuid) {
        if let Some(mut entry) = self.peers.get_mut(&peer_id) {
            entry.fail_check();
            if entry.status == PeerStatus::Dead {
                warn!(peer_id = %peer_id, "peer marked as dead");
            }
        }
    }

    /// Mark a peer as having left the cluster.
    pub fn mark_left(&self, peer_id: Uuid) {
        if let Some(mut entry) = self.peers.get_mut(&peer_id) {
            entry.status = PeerStatus::Left;
            info!(peer_id = %peer_id, "peer left the cluster");
        }
    }

    /// Remove a peer from tracking.
    pub fn remove(&self, peer_id: &Uuid) -> Option<PeerEntry> {
        self.peers.remove(peer_id).map(|(_, v)| v)
    }

    /// Get a snapshot of a specific peer.
    pub fn get(&self, peer_id: &Uuid) -> Option<PeerEntry> {
        self.peers.get(peer_id).map(|e| e.clone())
    }

    /// Return all peers with the given status.
    pub fn peers_with_status(&self, status: PeerStatus) -> Vec<PeerEntry> {
        self.peers
            .iter()
            .filter(|e| e.status == status)
            .map(|e| e.clone())
            .collect()
    }

    /// Return all known alive peers.
    pub fn alive_peers(&self) -> Vec<PeerEntry> {
        self.peers_with_status(PeerStatus::Alive)
    }

    /// Total number of tracked peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Returns `true` if no peers are tracked.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Snapshot all peers.
    pub fn snapshot(&self) -> Vec<PeerEntry> {
        self.peers.iter().map(|e| e.clone()).collect()
    }

    /// Sweep peers that have been dead or left, removing them from tracking.
    pub fn sweep_dead(&self) -> Vec<PeerEntry> {
        let dead_ids: Vec<Uuid> = self
            .peers
            .iter()
            .filter(|e| matches!(e.status, PeerStatus::Dead | PeerStatus::Left))
            .map(|e| e.info.id)
            .collect();

        let mut removed = Vec::new();
        for id in dead_ids {
            if let Some(entry) = self.remove(&id) {
                removed.push(entry);
            }
        }
        removed
    }
}

impl Default for PeerTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Cluster operations
// ---------------------------------------------------------------------------

/// Announce this node to the provided peer addresses.
///
/// In the current implementation this is a no-op placeholder that logs the
/// intent. Once `daf-transport` exposes connection primitives, this will
/// send a `NodeInfo` announcement message to each peer.
pub async fn cluster_join(node: &Node, peers: &[SocketAddr]) {
    if peers.is_empty() {
        info!(node = %node, "starting in standalone mode (no cluster peers)");
        return;
    }
    info!(
        node = %node,
        peer_count = peers.len(),
        "announcing to cluster peers"
    );
    for peer in peers {
        debug!(peer = %peer, "sending join announcement");
        // TODO: send NodeInfo over transport once daf-transport is wired up.
    }
}

/// Deregister this node from the cluster.
///
/// Sends a leave announcement to all tracked peers so they can update their
/// membership tables immediately rather than waiting for a stale timeout.
pub async fn cluster_leave(node: &Node, tracker: &PeerTracker) {
    let alive = tracker.alive_peers();
    if alive.is_empty() {
        return;
    }
    info!(
        node = %node,
        notifying = alive.len(),
        "sending cluster leave announcement"
    );
    for peer in &alive {
        debug!(peer = %peer.info.name, "notifying peer of departure");
        // TODO: send leave message over transport.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr() -> SocketAddr {
        "127.0.0.1:9400".parse().unwrap()
    }

    #[test]
    fn node_identity() {
        let node = Node::new("test-node", test_addr());
        assert_eq!(node.name, "test-node");
        assert_eq!(node.address, test_addr());
        assert!(!node.version.is_empty());
    }

    #[test]
    fn node_info_roundtrip() {
        let node = Node::new("n1", test_addr()).with_label("env", "test");
        let info = node.info();
        let json = serde_json::to_string(&info).unwrap();
        let back: NodeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, info.id);
        assert_eq!(back.name, "n1");
        assert_eq!(back.labels.get("env").unwrap(), "test");
    }

    #[test]
    fn node_display() {
        let node = Node::new("alpha", test_addr());
        let display = node.to_string();
        assert!(display.contains("alpha"));
        assert!(display.contains("127.0.0.1:9400"));
    }

    #[test]
    fn peer_tracker_upsert_and_get() {
        let tracker = PeerTracker::new();
        let node = Node::new("peer1", test_addr());
        let info = node.info();
        let id = info.id;

        tracker.upsert(info);
        assert_eq!(tracker.len(), 1);

        let entry = tracker.get(&id).unwrap();
        assert_eq!(entry.status, PeerStatus::Alive);
        assert_eq!(entry.info.name, "peer1");
    }

    #[test]
    fn peer_tracker_heartbeat() {
        let tracker = PeerTracker::new();
        let info = Node::new("p", test_addr()).info();
        let id = info.id;
        tracker.upsert(info);

        tracker.fail_check(id);
        assert_eq!(tracker.get(&id).unwrap().status, PeerStatus::Suspected);

        tracker.heartbeat(id);
        assert_eq!(tracker.get(&id).unwrap().status, PeerStatus::Alive);
    }

    #[test]
    fn peer_tracker_dead_after_3_failures() {
        let tracker = PeerTracker::new();
        let info = Node::new("p", test_addr()).info();
        let id = info.id;
        tracker.upsert(info);

        tracker.fail_check(id);
        tracker.fail_check(id);
        assert_eq!(tracker.get(&id).unwrap().status, PeerStatus::Suspected);

        tracker.fail_check(id);
        assert_eq!(tracker.get(&id).unwrap().status, PeerStatus::Dead);
    }

    #[test]
    fn peer_tracker_mark_left() {
        let tracker = PeerTracker::new();
        let info = Node::new("p", test_addr()).info();
        let id = info.id;
        tracker.upsert(info);

        tracker.mark_left(id);
        assert_eq!(tracker.get(&id).unwrap().status, PeerStatus::Left);
    }

    #[test]
    fn peer_tracker_sweep_dead() {
        let tracker = PeerTracker::new();

        let alive_info = Node::new("alive", test_addr()).info();
        let dead_info = Node::new("dead", "127.0.0.1:9401".parse().unwrap()).info();
        let dead_id = dead_info.id;

        tracker.upsert(alive_info);
        tracker.upsert(dead_info);
        tracker.fail_check(dead_id);
        tracker.fail_check(dead_id);
        tracker.fail_check(dead_id);

        let swept = tracker.sweep_dead();
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].info.name, "dead");
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn alive_peers_filter() {
        let tracker = PeerTracker::new();
        let a = Node::new("a", test_addr()).info();
        let b = Node::new("b", "127.0.0.1:9401".parse().unwrap()).info();
        let b_id = b.id;

        tracker.upsert(a);
        tracker.upsert(b);
        tracker.mark_left(b_id);

        let alive = tracker.alive_peers();
        assert_eq!(alive.len(), 1);
        assert_eq!(alive[0].info.name, "a");
    }

    #[test]
    fn peer_status_display() {
        assert_eq!(PeerStatus::Alive.to_string(), "alive");
        assert_eq!(PeerStatus::Dead.to_string(), "dead");
        assert_eq!(PeerStatus::Left.to_string(), "left");
        assert_eq!(PeerStatus::Suspected.to_string(), "suspected");
    }
}
