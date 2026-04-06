//! Generic connection pool.
//!
//! [`ConnectionPool`] manages a set of reusable connections with configurable
//! limits, idle timeout, max lifetime, periodic health checking, and
//! round-robin selection.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::connection::Connection;
use crate::error::{TransportError, TransportResult};

// ---------------------------------------------------------------------------
// Pool config
// ---------------------------------------------------------------------------

/// Configuration for a connection pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Minimum number of connections to maintain (warm pool).
    pub min_connections: usize,
    /// Maximum number of connections allowed.
    pub max_connections: usize,
    /// Close idle connections after this duration.
    pub idle_timeout: Duration,
    /// Close connections older than this regardless of activity.
    pub max_lifetime: Duration,
    /// Interval between health check sweeps.
    pub health_check_interval: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            min_connections: 2,
            max_connections: 64,
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(3600),
            health_check_interval: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// Pool entry
// ---------------------------------------------------------------------------

/// Wrapper around a pooled connection with lifecycle timestamps.
struct PoolEntry<C: Connection> {
    conn: C,
    created_at: Instant,
    last_used: Instant,
}

impl<C: Connection> PoolEntry<C> {
    fn new(conn: C) -> Self {
        let now = Instant::now();
        Self {
            conn,
            created_at: now,
            last_used: now,
        }
    }

    /// Whether this entry has exceeded its maximum lifetime.
    fn is_expired(&self, max_lifetime: Duration) -> bool {
        self.created_at.elapsed() > max_lifetime
    }

    /// Whether this entry has been idle too long.
    fn is_idle(&self, idle_timeout: Duration) -> bool {
        self.last_used.elapsed() > idle_timeout
    }

    /// Whether the underlying connection is still alive.
    fn is_alive(&self) -> bool {
        self.conn.is_alive()
    }
}

// ---------------------------------------------------------------------------
// Connection pool
// ---------------------------------------------------------------------------

/// A generic connection pool providing reuse, health checks, and limits.
///
/// Connections are selected round-robin. Dead, idle, or expired connections
/// are evicted during [`get`](Self::get) and during periodic
/// [`health_check`](Self::health_check) sweeps.
pub struct ConnectionPool<C: Connection> {
    config: PoolConfig,
    entries: Mutex<Vec<PoolEntry<C>>>,
    /// Round-robin index.
    next: AtomicUsize,
    /// Total connections ever created (for diagnostics).
    total_created: AtomicUsize,
}

impl<C: Connection> ConnectionPool<C> {
    /// Create a new empty pool with the given configuration.
    pub fn new(config: PoolConfig) -> Self {
        Self {
            config,
            entries: Mutex::new(Vec::new()),
            next: AtomicUsize::new(0),
            total_created: AtomicUsize::new(0),
        }
    }

    /// Add a connection to the pool.
    ///
    /// Returns an error if the pool is at capacity.
    pub async fn put(&self, conn: C) -> TransportResult<()> {
        let mut entries = self.entries.lock().await;
        if entries.len() >= self.config.max_connections {
            return Err(TransportError::PoolExhausted {
                pool_size: self.config.max_connections,
            });
        }
        entries.push(PoolEntry::new(conn));
        self.total_created.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Get the next available connection from the pool using round-robin.
    ///
    /// Evicts dead, idle, and expired entries along the way. Returns `None`
    /// if the pool is empty after eviction.
    pub async fn get(&self) -> Option<PooledConnection<'_, C>> {
        let mut entries = self.entries.lock().await;

        // Evict unhealthy entries first.
        entries.retain(|e| {
            e.is_alive()
                && !e.is_expired(self.config.max_lifetime)
                && !e.is_idle(self.config.idle_timeout)
        });

        if entries.is_empty() {
            return None;
        }

        let idx = self.next.fetch_add(1, Ordering::Relaxed) % entries.len();
        entries[idx].last_used = Instant::now();

        // We return a handle that borrows the pool. The caller can use the
        // connection through it.
        Some(PooledConnection {
            _pool: self,
            index: idx,
        })
    }

    /// Run a health check sweep, evicting dead, expired, and idle connections.
    ///
    /// Returns the number of connections evicted.
    pub async fn health_check(&self) -> usize {
        let mut entries = self.entries.lock().await;
        let before = entries.len();

        entries.retain(|e| {
            e.is_alive()
                && !e.is_expired(self.config.max_lifetime)
                && !e.is_idle(self.config.idle_timeout)
        });

        let evicted = before - entries.len();
        if evicted > 0 {
            tracing::debug!(evicted, remaining = entries.len(), "pool health check");
        }
        evicted
    }

    /// Return current pool size (number of connections held).
    pub async fn size(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Return total connections ever created through this pool.
    pub fn total_created(&self) -> usize {
        self.total_created.load(Ordering::Relaxed)
    }

    /// Return the pool configuration.
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// Drain all connections from the pool, closing each one.
    pub async fn drain(&self) {
        let mut entries = self.entries.lock().await;
        for mut entry in entries.drain(..) {
            entry.conn.close().await.ok();
        }
        tracing::debug!("connection pool drained");
    }
}

// ---------------------------------------------------------------------------
// Pooled connection handle
// ---------------------------------------------------------------------------

/// A handle to a connection borrowed from the pool.
///
/// This is returned by [`ConnectionPool::get`] and provides read-only
/// access to the connection metadata. For mutable I/O operations, take
/// the connection out of the pool, use it, and put it back.
pub struct PooledConnection<'a, C: Connection> {
    _pool: &'a ConnectionPool<C>,
    index: usize,
}

impl<C: Connection> PooledConnection<'_, C> {
    /// The index of this connection in the pool.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Reference to the pool this connection belongs to.
    pub fn pool(&self) -> &ConnectionPool<C> {
        self._pool
    }
}

// ---------------------------------------------------------------------------
// Standalone pool helpers
// ---------------------------------------------------------------------------

/// Spawn a background task that periodically runs health checks on the pool.
///
/// The task runs until the returned [`tokio::task::JoinHandle`] is aborted
/// or the pool is dropped.
pub fn spawn_health_checker<C: Connection>(
    pool: Arc<ConnectionPool<C>>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            pool.health_check().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionInfo, ConnectionState};

    /// Minimal mock connection for pool tests.
    struct MockConn {
        alive: bool,
        info: ConnectionInfo,
    }

    impl MockConn {
        fn new() -> Self {
            let mut info = ConnectionInfo::new(Some("mock".into()));
            info.state = ConnectionState::Connected;
            Self { alive: true, info }
        }

        fn dead() -> Self {
            let mut c = Self::new();
            c.alive = false;
            c.info.state = ConnectionState::Closed;
            c
        }
    }

    #[async_trait::async_trait]
    impl Connection for MockConn {
        async fn read(&mut self, _buf: &mut [u8]) -> TransportResult<usize> {
            Ok(0)
        }
        async fn write(&mut self, _data: &[u8]) -> TransportResult<()> {
            Ok(())
        }
        async fn flush(&mut self) -> TransportResult<()> {
            Ok(())
        }
        async fn close(&mut self) -> TransportResult<()> {
            self.alive = false;
            self.info.state = ConnectionState::Closed;
            Ok(())
        }
        fn peer_addr(&self) -> Option<String> {
            Some("mock".into())
        }
        fn is_alive(&self) -> bool {
            self.alive
        }
        fn info(&self) -> &ConnectionInfo {
            &self.info
        }
    }

    #[tokio::test]
    async fn pool_put_and_get() {
        let pool = ConnectionPool::new(PoolConfig {
            max_connections: 4,
            ..Default::default()
        });

        pool.put(MockConn::new()).await.unwrap();
        pool.put(MockConn::new()).await.unwrap();

        assert_eq!(pool.size().await, 2);

        let handle = pool.get().await;
        assert!(handle.is_some());
    }

    #[tokio::test]
    async fn pool_rejects_when_full() {
        let pool = ConnectionPool::new(PoolConfig {
            max_connections: 1,
            ..Default::default()
        });

        pool.put(MockConn::new()).await.unwrap();
        let result = pool.put(MockConn::new()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TransportError::PoolExhausted { pool_size: 1 }
        ));
    }

    #[tokio::test]
    async fn pool_evicts_dead_connections() {
        let pool = ConnectionPool::new(PoolConfig {
            max_connections: 4,
            ..Default::default()
        });

        pool.put(MockConn::dead()).await.unwrap();
        pool.put(MockConn::new()).await.unwrap();

        let evicted = pool.health_check().await;
        assert_eq!(evicted, 1);
        assert_eq!(pool.size().await, 1);
    }

    #[tokio::test]
    async fn pool_drain() {
        let pool = ConnectionPool::new(PoolConfig::default());
        pool.put(MockConn::new()).await.unwrap();
        pool.put(MockConn::new()).await.unwrap();

        pool.drain().await;
        assert_eq!(pool.size().await, 0);
    }

    #[tokio::test]
    async fn pool_get_empty() {
        let pool: ConnectionPool<MockConn> = ConnectionPool::new(PoolConfig::default());
        assert!(pool.get().await.is_none());
    }

    #[tokio::test]
    async fn pool_round_robin() {
        let pool = ConnectionPool::new(PoolConfig {
            max_connections: 4,
            idle_timeout: Duration::from_secs(9999),
            ..Default::default()
        });

        pool.put(MockConn::new()).await.unwrap();
        pool.put(MockConn::new()).await.unwrap();

        let a = pool.get().await.unwrap().index();
        let b = pool.get().await.unwrap().index();
        // With 2 entries, round-robin should alternate.
        assert_ne!(a, b);
    }

    #[test]
    fn pool_config_defaults() {
        let cfg = PoolConfig::default();
        assert_eq!(cfg.min_connections, 2);
        assert_eq!(cfg.max_connections, 64);
    }
}
