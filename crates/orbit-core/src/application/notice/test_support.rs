//! Scoped owner time for public notice-post regression fixtures.

use std::cell::Cell;

use chrono::{DateTime, Utc};

thread_local! {
    static POST_TIME: Cell<Option<DateTime<Utc>>> = const { Cell::new(None) };
}

/// Run notice posts at a fixed owner time on this thread, restoring the
/// previous time on return or unwind. Other threads keep the real clock.
pub fn with_notice_post_time<T>(now: DateTime<Utc>, operation: impl FnOnce() -> T) -> T {
    let _restore = Restore(POST_TIME.replace(Some(now)));
    operation()
}

struct Restore(Option<DateTime<Utc>>);

impl Drop for Restore {
    fn drop(&mut self) {
        POST_TIME.set(self.0);
    }
}

pub(super) fn notice_post_time() -> DateTime<Utc> {
    POST_TIME.get().unwrap_or_else(Utc::now)
}
