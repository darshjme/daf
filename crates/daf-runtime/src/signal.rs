//! Signal handling for graceful shutdown and configuration reload.
//!
//! The [`ShutdownSignal`] wraps a [`tokio_util::sync::CancellationToken`]
//! and listens for OS signals (`SIGTERM`, `SIGINT`, `SIGHUP`) to coordinate
//! a clean teardown of the runtime.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// ShutdownSignal
// ---------------------------------------------------------------------------

/// Cooperative shutdown coordination primitive.
///
/// The runtime holds a `ShutdownSignal` and passes clones to every subsystem.
/// When a shutdown-triggering OS signal arrives (or [`trigger`] is called
/// programmatically), all holders are notified and should begin their teardown
/// sequence.
///
/// A forced-shutdown deadline is enforced: if the graceful phase exceeds the
/// configured timeout, [`is_forced`] returns `true` and subsystems should
/// abort immediately.
///
/// [`trigger`]: ShutdownSignal::trigger
/// [`is_forced`]: ShutdownSignal::is_forced
#[derive(Clone)]
pub struct ShutdownSignal {
    inner: Arc<ShutdownInner>,
}

struct ShutdownInner {
    /// Notified when shutdown is requested.
    notify: Notify,
    /// Whether shutdown has been triggered.
    triggered: std::sync::atomic::AtomicBool,
    /// Whether the forced-shutdown deadline has passed.
    forced: std::sync::atomic::AtomicBool,
    /// Notified when a SIGHUP (config reload) is received.
    reload_notify: Notify,
    /// Maximum time for graceful shutdown before forced kill.
    timeout: Duration,
}

impl ShutdownSignal {
    /// Create a new shutdown signal with the given graceful-shutdown timeout.
    pub fn new(timeout: Duration) -> Self {
        Self {
            inner: Arc::new(ShutdownInner {
                notify: Notify::new(),
                triggered: std::sync::atomic::AtomicBool::new(false),
                forced: std::sync::atomic::AtomicBool::new(false),
                reload_notify: Notify::new(),
                timeout,
            }),
        }
    }

    /// Programmatically trigger shutdown.
    pub fn trigger(&self) {
        if !self.inner.triggered.swap(true, std::sync::atomic::Ordering::SeqCst) {
            info!("shutdown signal triggered");
            self.inner.notify.notify_waiters();
        }
    }

    /// Wait until shutdown has been triggered.
    ///
    /// Returns immediately if shutdown was already requested.
    pub async fn wait(&self) {
        if self.is_triggered() {
            return;
        }
        self.inner.notify.notified().await;
    }

    /// Returns `true` if shutdown has been requested.
    pub fn is_triggered(&self) -> bool {
        self.inner.triggered.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns `true` if the graceful-shutdown timeout has expired and
    /// subsystems should abort immediately.
    pub fn is_forced(&self) -> bool {
        self.inner.forced.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Mark the shutdown as forced (deadline exceeded).
    pub fn force(&self) {
        self.inner.forced.store(true, std::sync::atomic::Ordering::SeqCst);
        // Re-notify in case anyone is waiting.
        self.inner.notify.notify_waiters();
    }

    /// The configured graceful-shutdown timeout.
    pub fn timeout(&self) -> Duration {
        self.inner.timeout
    }

    /// Wait until a config-reload signal (SIGHUP) is received.
    pub async fn wait_reload(&self) {
        self.inner.reload_notify.notified().await;
    }

    /// Notify that a config reload was requested.
    pub fn trigger_reload(&self) {
        info!("configuration reload requested");
        self.inner.reload_notify.notify_waiters();
    }

    /// Install OS signal handlers and return a future that resolves when
    /// `SIGTERM` or `SIGINT` is received.
    ///
    /// On Unix, `SIGHUP` triggers a config reload notification instead of
    /// shutdown.
    pub async fn listen_for_signals(&self) {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
            let mut sigint =
                signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
            let mut sighup =
                signal(SignalKind::hangup()).expect("failed to install SIGHUP handler");

            loop {
                tokio::select! {
                    _ = sigterm.recv() => {
                        info!("received SIGTERM — initiating graceful shutdown");
                        self.trigger();
                        break;
                    }
                    _ = sigint.recv() => {
                        if self.is_triggered() {
                            warn!("received second SIGINT — forcing immediate shutdown");
                            self.force();
                            break;
                        }
                        info!("received SIGINT — initiating graceful shutdown");
                        self.trigger();
                    }
                    _ = sighup.recv() => {
                        self.trigger_reload();
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            // On non-Unix platforms, only handle Ctrl+C.
            let _ = tokio::signal::ctrl_c().await;
            info!("received Ctrl+C — initiating graceful shutdown");
            self.trigger();
        }
    }

    /// Spawn a background task that forces shutdown if the graceful phase
    /// exceeds the configured timeout.
    pub fn spawn_force_deadline(&self) -> tokio::task::JoinHandle<()> {
        let signal = self.clone();
        tokio::spawn(async move {
            signal.wait().await;
            tokio::time::sleep(signal.timeout()).await;
            if !signal.is_forced() {
                warn!(
                    timeout_secs = signal.timeout().as_secs(),
                    "graceful shutdown timeout exceeded — forcing shutdown"
                );
                signal.force();
            }
        })
    }
}

impl std::fmt::Debug for ShutdownSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownSignal")
            .field("triggered", &self.is_triggered())
            .field("forced", &self.is_forced())
            .field("timeout", &self.inner.timeout)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_starts_untriggered() {
        let signal = ShutdownSignal::new(Duration::from_secs(5));
        assert!(!signal.is_triggered());
        assert!(!signal.is_forced());
    }

    #[test]
    fn trigger_sets_flag() {
        let signal = ShutdownSignal::new(Duration::from_secs(5));
        signal.trigger();
        assert!(signal.is_triggered());
    }

    #[test]
    fn double_trigger_is_idempotent() {
        let signal = ShutdownSignal::new(Duration::from_secs(5));
        signal.trigger();
        signal.trigger();
        assert!(signal.is_triggered());
    }

    #[test]
    fn force_sets_flag() {
        let signal = ShutdownSignal::new(Duration::from_secs(5));
        signal.force();
        assert!(signal.is_forced());
    }

    #[test]
    fn clone_shares_state() {
        let a = ShutdownSignal::new(Duration::from_secs(5));
        let b = a.clone();
        a.trigger();
        assert!(b.is_triggered());
    }

    #[test]
    fn timeout_is_accessible() {
        let signal = ShutdownSignal::new(Duration::from_secs(42));
        assert_eq!(signal.timeout(), Duration::from_secs(42));
    }

    #[tokio::test]
    async fn wait_resolves_immediately_if_triggered() {
        let signal = ShutdownSignal::new(Duration::from_secs(5));
        signal.trigger();
        // Should not hang.
        signal.wait().await;
    }

    #[tokio::test]
    async fn wait_resolves_on_trigger() {
        let signal = ShutdownSignal::new(Duration::from_secs(5));
        let s2 = signal.clone();

        let handle = tokio::spawn(async move {
            s2.wait().await;
            true
        });

        // Give the spawned task a moment to start waiting.
        tokio::task::yield_now().await;
        signal.trigger();

        let result = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("timed out")
            .expect("task panicked");
        assert!(result);
    }

    #[test]
    fn debug_format() {
        let signal = ShutdownSignal::new(Duration::from_secs(10));
        let debug = format!("{signal:?}");
        assert!(debug.contains("ShutdownSignal"));
        assert!(debug.contains("triggered: false"));
    }
}
