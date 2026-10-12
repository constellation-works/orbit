//! Lock acquisition timing for synchronous execution scopes.

use std::cell::Cell;
use std::time::Duration;

thread_local! {
    static WAIT: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// Run a synchronous operation and return its accumulated lock wait in milliseconds.
/// Nested scopes own their waits. Other threads and child processes are excluded.
/// The previous scope is restored even if the operation unwinds.
pub fn measure<T>(op: impl FnOnce() -> T) -> (T, u64) {
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WAIT.with(|wait| wait.set(self.0));
        }
    }
    let _restore = Restore(WAIT.with(|wait| wait.replace(Some(Duration::ZERO))));
    let value = op();
    let millis = WAIT.with(|wait| wait.get().unwrap_or_default().as_millis());
    (value, u64::try_from(millis).unwrap_or(u64::MAX))
}

/// Account for an acquisition's elapsed wait in the current scope, if any.
pub fn record(waited: Duration) {
    WAIT.with(|wait| {
        if let Some(total) = wait.get() {
            wait.set(Some(total.saturating_add(waited)));
        }
    });
}

/// Acquire a store mutex and account for time waiting to obtain its guard.
pub fn mutex<T>(
    mutex: &std::sync::Mutex<T>,
) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>> {
    let started = std::time::Instant::now();
    let guard = mutex.lock();
    record(started.elapsed());
    guard
}
