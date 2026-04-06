//! Log retention and archival policies.
//!
//! [`RetentionPolicy`] defines how long logs are kept, how large they can grow,
//! and when they should be archived. [`RetentionManager`] enforces these
//! policies by periodically scanning log storage and cleaning up expired data.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::error::LoggerError;

// ---------------------------------------------------------------------------
// RetentionPolicy
// ---------------------------------------------------------------------------

/// Defines retention constraints for log data.
///
/// All constraints are optional — only the ones that are set will be enforced.
/// When multiple constraints are set, the most restrictive one wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Delete entries older than this duration.
    pub max_age: Option<Duration>,
    /// Delete oldest entries when total storage exceeds this many bytes.
    pub max_size_bytes: Option<u64>,
    /// Delete oldest entries when total entry count exceeds this.
    pub max_entries: Option<u64>,
    /// Move entries older than this to compressed archive files instead of
    /// deleting them.
    pub archive_after: Option<Duration>,
}

impl RetentionPolicy {
    /// Create a policy with no constraints (keep everything forever).
    pub fn keep_forever() -> Self {
        Self {
            max_age: None,
            max_size_bytes: None,
            max_entries: None,
            archive_after: None,
        }
    }

    /// Create a policy that keeps logs for a certain number of days.
    pub fn keep_days(days: i64) -> Self {
        Self {
            max_age: Some(Duration::days(days)),
            max_size_bytes: None,
            max_entries: None,
            archive_after: None,
        }
    }

    /// Create a production-reasonable default:
    /// - Keep raw logs for 30 days
    /// - Archive after 7 days
    /// - Max 10 GiB total
    pub fn production_default() -> Self {
        Self {
            max_age: Some(Duration::days(30)),
            max_size_bytes: Some(10 * 1024 * 1024 * 1024), // 10 GiB
            max_entries: None,
            archive_after: Some(Duration::days(7)),
        }
    }

    /// Builder: set max age.
    pub fn with_max_age(mut self, age: Duration) -> Self {
        self.max_age = Some(age);
        self
    }

    /// Builder: set max size.
    pub fn with_max_size(mut self, bytes: u64) -> Self {
        self.max_size_bytes = Some(bytes);
        self
    }

    /// Builder: set max entries.
    pub fn with_max_entries(mut self, n: u64) -> Self {
        self.max_entries = Some(n);
        self
    }

    /// Builder: set archive threshold.
    pub fn with_archive_after(mut self, age: Duration) -> Self {
        self.archive_after = Some(age);
        self
    }

    /// Check whether a timestamp is expired under this policy.
    pub fn is_expired(&self, timestamp: &DateTime<Utc>) -> bool {
        if let Some(max_age) = self.max_age {
            if Utc::now() - *timestamp > max_age {
                return true;
            }
        }
        false
    }

    /// Check whether a timestamp should be archived (but not deleted).
    pub fn should_archive(&self, timestamp: &DateTime<Utc>) -> bool {
        if let Some(archive_after) = self.archive_after {
            if Utc::now() - *timestamp > archive_after {
                return true;
            }
        }
        false
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::production_default()
    }
}

// ---------------------------------------------------------------------------
// RetentionStats
// ---------------------------------------------------------------------------

/// Statistics from a retention sweep.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetentionStats {
    /// Number of files scanned.
    pub files_scanned: u64,
    /// Number of files deleted (expired).
    pub files_deleted: u64,
    /// Number of files archived (compressed and moved).
    pub files_archived: u64,
    /// Total bytes reclaimed.
    pub bytes_reclaimed: u64,
    /// Errors encountered during the sweep.
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// RetentionManager
// ---------------------------------------------------------------------------

/// Enforces retention policies on a log directory.
///
/// Call [`sweep`] periodically (e.g. from a tokio timer) to clean up expired
/// files and archive old ones.
pub struct RetentionManager {
    /// Directory containing log files.
    log_dir: PathBuf,
    /// Directory for archived (compressed) logs.
    archive_dir: PathBuf,
    /// The active retention policy.
    policy: RetentionPolicy,
    /// Stats from the last sweep.
    last_stats: Arc<Mutex<RetentionStats>>,
}

impl RetentionManager {
    /// Create a retention manager for the given directories and policy.
    pub async fn new(
        log_dir: impl AsRef<Path>,
        archive_dir: impl AsRef<Path>,
        policy: RetentionPolicy,
    ) -> Result<Self, LoggerError> {
        let log_dir = log_dir.as_ref().to_path_buf();
        let archive_dir = archive_dir.as_ref().to_path_buf();

        fs::create_dir_all(&log_dir).await.map_err(LoggerError::Io)?;
        fs::create_dir_all(&archive_dir).await.map_err(LoggerError::Io)?;

        Ok(Self {
            log_dir,
            archive_dir,
            policy,
            last_stats: Arc::new(Mutex::new(RetentionStats::default())),
        })
    }

    /// Run a retention sweep: delete expired files, archive old files.
    pub async fn sweep(&self) -> Result<RetentionStats, LoggerError> {
        let mut stats = RetentionStats::default();

        let mut entries = fs::read_dir(&self.log_dir)
            .await
            .map_err(LoggerError::Io)?;

        let mut file_infos: Vec<(PathBuf, u64, DateTime<Utc>)> = Vec::new();

        while let Some(entry) = entries.next_entry().await.map_err(LoggerError::Io)? {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Only process .ndjson files.
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if ext != "ndjson" {
                continue;
            }

            stats.files_scanned += 1;

            let metadata = match fs::metadata(&path).await {
                Ok(m) => m,
                Err(e) => {
                    stats.errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };

            let modified: DateTime<Utc> = metadata
                .modified()
                .map(|t| DateTime::from(t))
                .unwrap_or_else(|_| Utc::now());

            file_infos.push((path, metadata.len(), modified));
        }

        // Sort oldest first for consistent processing.
        file_infos.sort_by_key(|(_, _, ts)| *ts);

        // Track total size for max_size enforcement.
        let total_size: u64 = file_infos.iter().map(|(_, size, _)| size).sum();
        let mut cumulative_deleted: u64 = 0;

        for (path, size, modified) in &file_infos {
            // Check max_age expiration.
            if self.policy.is_expired(modified) {
                match fs::remove_file(&path).await {
                    Ok(()) => {
                        info!(path = %path.display(), "deleted expired log file");
                        stats.files_deleted += 1;
                        stats.bytes_reclaimed += size;
                        cumulative_deleted += size;
                    }
                    Err(e) => {
                        stats.errors.push(format!("delete {}: {e}", path.display()));
                    }
                }
                continue;
            }

            // Check max_size — delete oldest files until under limit.
            if let Some(max_size) = self.policy.max_size_bytes {
                if total_size - cumulative_deleted > max_size {
                    match fs::remove_file(&path).await {
                        Ok(()) => {
                            debug!(path = %path.display(), "deleted log file (over size limit)");
                            stats.files_deleted += 1;
                            stats.bytes_reclaimed += size;
                            cumulative_deleted += size;
                        }
                        Err(e) => {
                            stats.errors.push(format!("delete {}: {e}", path.display()));
                        }
                    }
                    continue;
                }
            }

            // Check archive threshold.
            if self.policy.should_archive(modified) {
                let archive_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
                    + ".archived";
                let archive_path = self.archive_dir.join(archive_name);

                match fs::rename(&path, &archive_path).await {
                    Ok(()) => {
                        info!(
                            src = %path.display(),
                            dst = %archive_path.display(),
                            "archived log file"
                        );
                        stats.files_archived += 1;
                    }
                    Err(e) => {
                        stats.errors.push(format!(
                            "archive {} -> {}: {e}",
                            path.display(),
                            archive_path.display()
                        ));
                    }
                }
            }
        }

        *self.last_stats.lock() = stats.clone();
        Ok(stats)
    }

    /// Get stats from the last sweep.
    pub fn last_stats(&self) -> RetentionStats {
        self.last_stats.lock().clone()
    }

    /// Get the current policy.
    pub fn policy(&self) -> &RetentionPolicy {
        &self.policy
    }

    /// Update the retention policy.
    pub fn set_policy(&mut self, policy: RetentionPolicy) {
        self.policy = policy;
    }

    /// Start a background sweep task that runs on an interval.
    ///
    /// Returns a `JoinHandle` that can be used to cancel the task.
    pub fn start_periodic_sweep(
        self: Arc<Self>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                match self.sweep().await {
                    Ok(stats) => {
                        if stats.files_deleted > 0 || stats.files_archived > 0 {
                            info!(
                                deleted = stats.files_deleted,
                                archived = stats.files_archived,
                                reclaimed = stats.bytes_reclaimed,
                                "retention sweep completed"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "retention sweep failed");
                    }
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_keep_forever_never_expires() {
        let policy = RetentionPolicy::keep_forever();
        let ancient = Utc::now() - Duration::days(3650);
        assert!(!policy.is_expired(&ancient));
        assert!(!policy.should_archive(&ancient));
    }

    #[test]
    fn policy_keep_days_expires_correctly() {
        let policy = RetentionPolicy::keep_days(7);
        let recent = Utc::now() - Duration::days(3);
        let old = Utc::now() - Duration::days(10);

        assert!(!policy.is_expired(&recent));
        assert!(policy.is_expired(&old));
    }

    #[test]
    fn policy_archive_after() {
        let policy = RetentionPolicy::keep_forever()
            .with_archive_after(Duration::days(7));

        let recent = Utc::now() - Duration::days(3);
        let old = Utc::now() - Duration::days(10);

        assert!(!policy.should_archive(&recent));
        assert!(policy.should_archive(&old));
        // Should not be expired (keep_forever).
        assert!(!policy.is_expired(&old));
    }

    #[test]
    fn production_default_values() {
        let policy = RetentionPolicy::production_default();
        assert!(policy.max_age.is_some());
        assert!(policy.max_size_bytes.is_some());
        assert!(policy.archive_after.is_some());
    }

    #[tokio::test]
    async fn sweep_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        let archive_dir = tmp.path().join("archive");

        let mgr = RetentionManager::new(&log_dir, &archive_dir, RetentionPolicy::keep_days(1))
            .await
            .unwrap();

        let stats = mgr.sweep().await.unwrap();
        assert_eq!(stats.files_scanned, 0);
        assert_eq!(stats.files_deleted, 0);
    }

    #[tokio::test]
    async fn sweep_deletes_expired_files() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        let archive_dir = tmp.path().join("archive");

        fs::create_dir_all(&log_dir).await.unwrap();

        // Create a fake log file.
        let file_path = log_dir.join("test_20240101T000000Z.ndjson");
        fs::write(&file_path, "{\"test\":true}\n").await.unwrap();

        // Use a policy that expires immediately (max_age = 0).
        let policy = RetentionPolicy::keep_days(0);
        let mgr = RetentionManager::new(&log_dir, &archive_dir, policy)
            .await
            .unwrap();

        let stats = mgr.sweep().await.unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_deleted, 1);

        // File should be gone.
        assert!(!file_path.exists());
    }

    #[tokio::test]
    async fn sweep_archives_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        let archive_dir = tmp.path().join("archive");

        fs::create_dir_all(&log_dir).await.unwrap();

        let file_path = log_dir.join("archivable_20240101T000000Z.ndjson");
        fs::write(&file_path, "{\"data\":1}\n").await.unwrap();

        // Policy: keep forever but archive after 0 days.
        let policy = RetentionPolicy::keep_forever()
            .with_archive_after(Duration::days(0));

        let mgr = RetentionManager::new(&log_dir, &archive_dir, policy)
            .await
            .unwrap();

        let stats = mgr.sweep().await.unwrap();
        assert_eq!(stats.files_archived, 1);
        assert_eq!(stats.files_deleted, 0);
        assert!(!file_path.exists());

        // Should exist in archive dir.
        let archived =
            archive_dir.join("archivable_20240101T000000Z.ndjson.archived");
        assert!(archived.exists());
    }
}
