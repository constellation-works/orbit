use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use super::holders::{FileLockHolderInfo, describe_shared_holders};

/// Default upper bound for an advisory file-lock acquisition.
pub const DEFAULT_FILE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

const DEFAULT_FILE_LOCK_WARN_AFTER: Duration = Duration::from_secs(3);

/// Deadline, warning and diagnostic policy for one advisory file-lock
/// acquisition.
#[derive(Debug, Clone, Copy)]
pub struct FileLockOptions {
    /// Fail acquisition after this duration.
    pub timeout: Duration,
    /// Emit one contention warning after this duration.
    pub warn_after: Duration,
    /// Record each shared holder beside the lock file, so a waiter blocked by
    /// readers can name them. An exclusive holder is always recorded. Costs a
    /// file create and remove per shared acquisition, so it is meant for
    /// coordination locks, not per-record locks taken by every read.
    pub record_shared_holders: bool,
    /// Log the label and held duration when a holder releases the lock after
    /// holding it at least this long.
    pub warn_held_after: Option<Duration>,
    /// Give a waiting exclusive acquirer priority over shared acquirers that
    /// arrive after it. A refused exclusive acquirer holds the lock's
    /// turnstile (`<lock>.turnstile`) until it takes the lock, and a shared
    /// acquirer passes through the turnstile before trying the lock, so once
    /// a writer queues, new readers wait behind it while the readers already
    /// inside drain. Without it a writer succeeds only at an instant when no
    /// reader holds the lock, and readers that keep overlapping starve it to
    /// its deadline.
    ///
    /// Every acquirer of the lock must agree on this. A shared acquirer that
    /// already holds a lock the queued writer's blockers may wait on must
    /// not pass the turnstile, or the two waits close a cycle; such a nested
    /// acquisition turns this off.
    pub prefer_exclusive_waiters: bool,
}

impl Default for FileLockOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_FILE_LOCK_TIMEOUT,
            warn_after: DEFAULT_FILE_LOCK_WARN_AFTER,
            record_shared_holders: false,
            warn_held_after: None,
            prefer_exclusive_waiters: false,
        }
    }
}

/// Structured details for an advisory lock acquisition that exceeded its deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileLockTimeout {
    /// Exact, stable lock file that could not be acquired.
    pub lock_path: PathBuf,
    /// Human-readable label of the waiting operation.
    pub label: String,
    /// Injected acquisition deadline in milliseconds.
    pub timeout_ms: u64,
    /// Advisory exclusive-holder metadata, when a complete record was readable.
    pub holder: Option<FileLockHolderInfo>,
    /// Live shared holders, for a lock whose readers record themselves
    /// ([`FileLockOptions::record_shared_holders`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_holders: Vec<FileLockHolderInfo>,
}

impl std::fmt::Display for FileLockTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "timed out after {}ms acquiring {} lock '{}'",
            self.timeout_ms,
            self.label,
            self.lock_path.display()
        )?;
        if let Some(holder) = &self.holder {
            write!(
                formatter,
                " (held by pid {} since {}, op: {})",
                holder.pid, holder.acquired_at, holder.label
            )?;
        } else if !self.shared_holders.is_empty() {
            write!(
                formatter,
                " (held shared by {})",
                describe_shared_holders(&self.shared_holders)
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for FileLockTimeout {}
