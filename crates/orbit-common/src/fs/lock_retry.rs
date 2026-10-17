//! Bounded retry of a lock acquisition that lost to its deadline, for a
//! caller that opted its thread in.
//!
//! An advisory-lock acquisition gives up after its own deadline
//! ([`DEFAULT_FILE_LOCK_TIMEOUT`](super::file_lock::DEFAULT_FILE_LOCK_TIMEOUT))
//! so a hung holder never wedges its waiters, and most callers want exactly
//! that. A late step of a long delivery does not: when a holder that a
//! saturated disk kept past the deadline failed its acquisition, the step
//! failed with it and the finished candidate went back to the queue. Such a
//! step runs inside [`with_lock_busy_retry`]. Within that scope an outermost
//! acquisition made through [`retry_busy_acquisition`] is tried again, after a
//! growing pause, until the scope has spent its cap of waiting; the record it
//! returns says how many retries that took and how long they waited.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::OrbitError;

/// How long a scope may keep retrying acquisitions that lost to a held lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockBusyRetryPolicy {
    /// Most time the scope spends in lost acquisitions and the pauses after
    /// them. Once it is spent, the next lost acquisition fails its caller.
    pub cap: Duration,
    /// Pause after the first lost acquisition. Each later pause doubles, up
    /// to [`Self::longest_pause`], and never runs past the cap.
    pub first_pause: Duration,
    /// Longest pause between two attempts.
    pub longest_pause: Duration,
}

/// What a [`with_lock_busy_retry`] scope spent waiting on held locks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockBusyRetryRecord {
    /// Acquisitions repeated after losing to a held lock.
    pub retries: u32,
    /// Time spent in acquisitions that lost to a held lock, and in the pauses
    /// after them.
    pub waited: Duration,
    /// The scope's cap on [`Self::waited`].
    pub cap: Duration,
    /// Whether an acquisition lost once the cap was spent, so its failure
    /// reached the caller.
    pub exhausted: bool,
}

struct Scope {
    policy: LockBusyRetryPolicy,
    record: LockBusyRetryRecord,
    next_pause: Duration,
}

thread_local! {
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// Run `op` with lock-busy retry enabled on this thread, and return what the
/// retries cost.
///
/// A scope nested in another runs `op` under the outer one, which keeps the
/// accounting; the inner record is empty.
pub fn with_lock_busy_retry<T>(
    policy: LockBusyRetryPolicy,
    op: impl FnOnce() -> T,
) -> (T, LockBusyRetryRecord) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            SCOPE.with(|scope| scope.borrow_mut().take());
        }
    }

    #[cfg(feature = "test-support")]
    let policy = test_support::policy().unwrap_or(policy);
    let installed = SCOPE.with(|scope| {
        let mut scope = scope.borrow_mut();
        if scope.is_some() {
            return false;
        }
        *scope = Some(Scope {
            policy,
            record: LockBusyRetryRecord {
                cap: policy.cap,
                ..LockBusyRetryRecord::default()
            },
            next_pause: policy.first_pause,
        });
        true
    });
    if !installed {
        return (op(), LockBusyRetryRecord::default());
    }
    let _clear = Clear;
    let value = op();
    let record = SCOPE.with(|scope| {
        scope
            .borrow()
            .as_ref()
            .map(|scope| scope.record)
            .unwrap_or_default()
    });
    (value, record)
}

/// Whether this thread is inside a [`with_lock_busy_retry`] scope.
pub fn lock_busy_retry_active() -> bool {
    SCOPE.with(|scope| scope.borrow().is_some())
}

/// Make an acquisition, retrying one that lost to a held lock while this
/// thread is in a [`with_lock_busy_retry`] scope with time left.
///
/// `attempt` returns `Err` only when it lost before reaching the work the
/// lock protects, so repeating it decides nothing twice; `Ok` carries the
/// work's own result, which is returned as it is. Only an outermost
/// acquisition is retried: one made while this thread holds another advisory
/// lock would keep that lock from its waiters for the whole retry. Outside a
/// scope this is a single attempt.
pub fn retry_busy_acquisition<T>(
    mut attempt: impl FnMut() -> Result<Result<T, OrbitError>, OrbitError>,
) -> Result<T, OrbitError> {
    let retrying = lock_busy_retry_active() && !super::io::holds_file_lock();
    loop {
        let started = Instant::now();
        let error = match attempt() {
            Ok(result) => return result,
            Err(error) => error,
        };
        if !retrying || !error.is_lock_busy() {
            return Err(error);
        }
        let Some((pause, record)) = next_pause(started.elapsed()) else {
            return Err(error);
        };
        crate::tracing::warn!(
            target: "orbit.common.fs.lock_retry",
            retry = record.retries,
            pause_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
            waited_ms = u64::try_from(record.waited.as_millis()).unwrap_or(u64::MAX),
            cap_ms = u64::try_from(record.cap.as_millis()).unwrap_or(u64::MAX),
            error = %error,
            "lock acquisition lost to a held lock; retrying",
        );
        std::thread::sleep(pause);
    }
}

/// Charge a lost attempt of `lost` to the scope and return the pause before
/// the next one, or `None` once the cap is spent.
fn next_pause(lost: Duration) -> Option<(Duration, LockBusyRetryRecord)> {
    SCOPE.with(|scope| {
        let mut scope = scope.borrow_mut();
        let scope = scope.as_mut()?;
        scope.record.waited = scope.record.waited.saturating_add(lost);
        let remaining = scope.policy.cap.saturating_sub(scope.record.waited);
        if remaining.is_zero() {
            scope.record.exhausted = true;
            return None;
        }
        let pause = scope.next_pause.min(remaining);
        scope.next_pause = scope
            .next_pause
            .saturating_mul(2)
            .min(scope.policy.longest_pause);
        scope.record.waited = scope.record.waited.saturating_add(pause);
        scope.record.retries += 1;
        Some((pause, scope.record))
    })
}

/// Substitute the policy of every [`with_lock_busy_retry`] scope opened on
/// this thread while `op` runs, so a test can spend a cap in seconds. Each
/// scope still really waits and really retries.
#[cfg(feature = "test-support")]
pub fn with_lock_busy_retry_policy<T>(policy: LockBusyRetryPolicy, op: impl FnOnce() -> T) -> T {
    test_support::with_policy(policy, op)
}

#[cfg(feature = "test-support")]
mod test_support {
    use std::cell::Cell;

    use super::LockBusyRetryPolicy;

    thread_local! {
        static POLICY: Cell<Option<LockBusyRetryPolicy>> = const { Cell::new(None) };
    }

    pub(super) fn policy() -> Option<LockBusyRetryPolicy> {
        POLICY.with(Cell::get)
    }

    pub(super) fn with_policy<T>(policy: LockBusyRetryPolicy, op: impl FnOnce() -> T) -> T {
        struct Restore(Option<LockBusyRetryPolicy>);
        impl Drop for Restore {
            fn drop(&mut self) {
                POLICY.with(|slot| slot.set(self.0));
            }
        }
        let _restore = Restore(POLICY.with(|slot| slot.replace(Some(policy))));
        op()
    }
}
