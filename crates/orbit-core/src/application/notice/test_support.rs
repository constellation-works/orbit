//! Scoped owner time and close-snapshot interleaving for notice regressions.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{DateTime, Utc};

thread_local! {
    static POST_TIME: Cell<Option<DateTime<Utc>>> = const { Cell::new(None) };
    static CLOSE_SNAPSHOT_HOOK: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
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

/// Run `operation` while `hook` fires after a close has taken its notice
/// snapshot and before the closing transaction starts. The hook runs on this
/// thread, so it can commit a corroboration the close must keep. The previous
/// hook is restored on return or unwind.
pub fn with_notice_close_snapshot_hook<T>(
    hook: impl Fn() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let _restore =
        CloseHookRestore(CLOSE_SNAPSHOT_HOOK.with(|slot| slot.replace(Some(Rc::new(hook)))));
    operation()
}

struct CloseHookRestore(Option<Rc<dyn Fn()>>);

impl Drop for CloseHookRestore {
    fn drop(&mut self) {
        CLOSE_SNAPSHOT_HOOK.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn after_notice_close_snapshot() {
    let hook = CLOSE_SNAPSHOT_HOOK.with(|slot| slot.borrow().clone());
    if let Some(hook) = hook {
        hook();
    }
}
