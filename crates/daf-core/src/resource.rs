//! Resource management primitives.
//!
//! The resource system prevents agents from consuming unbounded compute,
//! memory, or network capacity. [`ResourcePool`] tracks per-kind budgets
//! and hands out [`ResourceGuard`] RAII tokens that automatically release
//! capacity when dropped.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::{DafError, DafResult};

// ---------------------------------------------------------------------------
// ResourceKind
// ---------------------------------------------------------------------------

/// Classification of a resource being tracked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// CPU cores or vCPUs.
    Compute,
    /// Persistent disk capacity (bytes).
    Storage,
    /// Network bandwidth or connection count.
    Network,
    /// Heap / working-set memory (bytes).
    Memory,
    /// Extension point for domain-specific resources.
    Custom(String),
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compute => write!(f, "compute"),
            Self::Storage => write!(f, "storage"),
            Self::Network => write!(f, "network"),
            Self::Memory => write!(f, "memory"),
            Self::Custom(s) => write!(f, "custom:{s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceLimit
// ---------------------------------------------------------------------------

/// Budget for a single resource kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimit {
    /// Which resource this limit governs.
    pub kind: ResourceKind,
    /// Maximum units available.
    pub max_amount: u64,
    /// Currently consumed units.
    pub current_usage: u64,
}

impl ResourceLimit {
    /// Create a new limit with zero initial usage.
    pub fn new(kind: ResourceKind, max_amount: u64) -> Self {
        Self {
            kind,
            max_amount,
            current_usage: 0,
        }
    }

    /// Available capacity.
    pub fn available(&self) -> u64 {
        self.max_amount.saturating_sub(self.current_usage)
    }

    /// Utilization as a fraction in `[0.0, 1.0]`.
    pub fn utilization(&self) -> f64 {
        if self.max_amount == 0 {
            return 0.0;
        }
        self.current_usage as f64 / self.max_amount as f64
    }

    /// Returns `true` if usage has reached or exceeded the limit.
    pub fn is_exhausted(&self) -> bool {
        self.current_usage >= self.max_amount
    }
}

impl fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}/{} ({:.1}%)",
            self.kind,
            self.current_usage,
            self.max_amount,
            self.utilization() * 100.0
        )
    }
}

// ---------------------------------------------------------------------------
// ResourcePool
// ---------------------------------------------------------------------------

/// Thread-safe pool of resource budgets.
///
/// The pool tracks multiple [`ResourceLimit`]s and provides atomic
/// acquire/release operations. Callers should prefer [`acquire_guard`]
/// which returns an RAII [`ResourceGuard`] that auto-releases on drop.
///
/// [`acquire_guard`]: ResourcePool::acquire_guard
#[derive(Debug, Clone)]
pub struct ResourcePool {
    inner: Arc<Mutex<PoolInner>>,
}

#[derive(Debug)]
struct PoolInner {
    limits: HashMap<ResourceKind, ResourceLimit>,
}

impl ResourcePool {
    /// Create an empty pool. Add limits with [`add_limit`](Self::add_limit).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                limits: HashMap::new(),
            })),
        }
    }

    /// Register a resource budget.
    pub fn add_limit(&self, limit: ResourceLimit) {
        let mut inner = self.inner.lock();
        inner.limits.insert(limit.kind.clone(), limit);
    }

    /// Try to acquire `amount` units of the given resource.
    ///
    /// Returns `Ok(())` on success or [`DafError::ResourceExhausted`] if
    /// insufficient capacity is available. The operation is atomic: either
    /// the full amount is reserved or nothing is.
    pub fn acquire(&self, kind: &ResourceKind, amount: u64) -> DafResult<()> {
        let mut inner = self.inner.lock();
        let limit = inner.limits.get_mut(kind).ok_or_else(|| {
            DafError::NotFound {
                entity: "resource_limit".into(),
                id: kind.to_string(),
            }
        })?;

        if limit.available() < amount {
            return Err(DafError::ResourceExhausted {
                resource: format!(
                    "{}: requested {} but only {} available",
                    kind,
                    amount,
                    limit.available()
                ),
            });
        }

        limit.current_usage += amount;
        Ok(())
    }

    /// Release `amount` units of the given resource.
    ///
    /// Panics in debug mode if the release would underflow.
    pub fn release(&self, kind: &ResourceKind, amount: u64) {
        let mut inner = self.inner.lock();
        if let Some(limit) = inner.limits.get_mut(kind) {
            debug_assert!(
                limit.current_usage >= amount,
                "resource release underflow for {kind}: releasing {amount} but only {} in use",
                limit.current_usage
            );
            limit.current_usage = limit.current_usage.saturating_sub(amount);
        }
    }

    /// Acquire `amount` units and return an RAII guard that releases them
    /// automatically when dropped.
    pub fn acquire_guard(
        &self,
        kind: ResourceKind,
        amount: u64,
    ) -> DafResult<ResourceGuard> {
        self.acquire(&kind, amount)?;
        Ok(ResourceGuard {
            pool: self.clone(),
            kind,
            amount,
            released: false,
        })
    }

    /// Snapshot the current state of all limits.
    pub fn snapshot(&self) -> Vec<ResourceLimit> {
        let inner = self.inner.lock();
        inner.limits.values().cloned().collect()
    }

    /// Get the current usage for a specific resource kind.
    pub fn usage(&self, kind: &ResourceKind) -> Option<ResourceLimit> {
        let inner = self.inner.lock();
        inner.limits.get(kind).cloned()
    }

    /// Returns `true` if any tracked resource is exhausted.
    pub fn any_exhausted(&self) -> bool {
        let inner = self.inner.lock();
        inner.limits.values().any(|l| l.is_exhausted())
    }

    /// Reset all usage counters to zero.
    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        for limit in inner.limits.values_mut() {
            limit.current_usage = 0;
        }
    }
}

impl Default for ResourcePool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ResourceGuard
// ---------------------------------------------------------------------------

/// RAII guard that releases acquired resources when dropped.
///
/// Created by [`ResourcePool::acquire_guard`]. The guard can also be
/// explicitly released via [`release`](Self::release) to handle the
/// result of the release operation.
#[derive(Debug)]
pub struct ResourceGuard {
    pool: ResourcePool,
    kind: ResourceKind,
    amount: u64,
    released: bool,
}

impl ResourceGuard {
    /// Explicitly release the held resources.
    pub fn release(mut self) {
        if !self.released {
            self.pool.release(&self.kind, self.amount);
            self.released = true;
        }
    }

    /// The kind of resource held.
    pub fn kind(&self) -> &ResourceKind {
        &self.kind
    }

    /// The amount of resource held.
    pub fn amount(&self) -> u64 {
        self.amount
    }
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        if !self.released {
            self.pool.release(&self.kind, self.amount);
            self.released = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_limit_basics() {
        let mut limit = ResourceLimit::new(ResourceKind::Memory, 1024);
        assert_eq!(limit.available(), 1024);
        assert!(!limit.is_exhausted());
        assert_eq!(limit.utilization(), 0.0);

        limit.current_usage = 512;
        assert_eq!(limit.available(), 512);
        assert!((limit.utilization() - 0.5).abs() < f64::EPSILON);

        limit.current_usage = 1024;
        assert!(limit.is_exhausted());
    }

    #[test]
    fn resource_limit_display() {
        let limit = ResourceLimit {
            kind: ResourceKind::Compute,
            max_amount: 100,
            current_usage: 75,
        };
        let s = limit.to_string();
        assert!(s.contains("compute"));
        assert!(s.contains("75/100"));
    }

    #[test]
    fn pool_acquire_release() {
        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(ResourceKind::Memory, 100));

        pool.acquire(&ResourceKind::Memory, 60).unwrap();
        assert_eq!(pool.usage(&ResourceKind::Memory).unwrap().current_usage, 60);

        pool.release(&ResourceKind::Memory, 30);
        assert_eq!(pool.usage(&ResourceKind::Memory).unwrap().current_usage, 30);
    }

    #[test]
    fn pool_exhaustion() {
        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(ResourceKind::Compute, 4));

        pool.acquire(&ResourceKind::Compute, 4).unwrap();
        let result = pool.acquire(&ResourceKind::Compute, 1);
        assert!(result.is_err());
        assert!(pool.any_exhausted());
    }

    #[test]
    fn pool_unknown_resource() {
        let pool = ResourcePool::new();
        let result = pool.acquire(&ResourceKind::Storage, 1);
        assert!(result.is_err());
    }

    #[test]
    fn guard_releases_on_drop() {
        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(ResourceKind::Network, 10));

        {
            let _guard = pool.acquire_guard(ResourceKind::Network, 5).unwrap();
            assert_eq!(
                pool.usage(&ResourceKind::Network).unwrap().current_usage,
                5
            );
        }
        // Guard dropped, should be released.
        assert_eq!(
            pool.usage(&ResourceKind::Network).unwrap().current_usage,
            0
        );
    }

    #[test]
    fn guard_explicit_release() {
        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(ResourceKind::Memory, 100));

        let guard = pool.acquire_guard(ResourceKind::Memory, 50).unwrap();
        assert_eq!(guard.amount(), 50);
        assert_eq!(*guard.kind(), ResourceKind::Memory);

        guard.release();
        assert_eq!(
            pool.usage(&ResourceKind::Memory).unwrap().current_usage,
            0
        );
    }

    #[test]
    fn pool_reset() {
        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(ResourceKind::Memory, 100));
        pool.acquire(&ResourceKind::Memory, 80).unwrap();
        pool.reset();
        assert_eq!(
            pool.usage(&ResourceKind::Memory).unwrap().current_usage,
            0
        );
    }

    #[test]
    fn pool_snapshot() {
        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(ResourceKind::Memory, 100));
        pool.add_limit(ResourceLimit::new(ResourceKind::Compute, 8));

        let snap = pool.snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn custom_resource_kind() {
        let kind = ResourceKind::Custom("gpu".into());
        assert_eq!(kind.to_string(), "custom:gpu");

        let pool = ResourcePool::new();
        pool.add_limit(ResourceLimit::new(kind.clone(), 2));
        pool.acquire(&kind, 1).unwrap();
        assert_eq!(pool.usage(&kind).unwrap().current_usage, 1);
    }
}
