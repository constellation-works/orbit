//! Same-thread rendezvous for the plugin-manifest publish race.
//!
//! A nested runtime open on this thread re-enters the migration lock, which
//! is what lets a test finish the competing pass after this pass has read
//! the consented manifest and before it publishes. Cross-process opens wait
//! on the lock instead.

use std::cell::{Cell, RefCell};

type PublishHook = Box<dyn FnOnce()>;

thread_local! {
    static HOOK: RefCell<Option<PublishHook>> = const { RefCell::new(None) };
    static FAIL_DIGEST_UPSERT: Cell<bool> = const { Cell::new(false) };
    static FAIL_RESTORE: Cell<bool> = const { Cell::new(false) };
}

struct RestoreHook(Option<PublishHook>);

impl Drop for RestoreHook {
    fn drop(&mut self) {
        HOOK.with(|slot| {
            *slot.borrow_mut() = self.0.take();
        });
        FAIL_DIGEST_UPSERT.with(|flag| flag.set(false));
        FAIL_RESTORE.with(|flag| flag.set(false));
    }
}

/// Run `operation` with a one-shot callback immediately before the first
/// consented plugin-manifest publish of a tool-name migration on this
/// thread.
#[cfg(feature = "test-support")]
pub fn with_plugin_manifest_publish_hook<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
    let _restore = RestoreHook(previous);
    operation()
}

/// Fail the next plugin-manifest digest record on this thread as a store
/// error, without writing the row.
///
/// A digest-change trigger cannot inject this once a competing pass has
/// already stored the same digest: the update no longer changes the column.
#[cfg(feature = "test-support")]
pub fn fail_next_plugin_digest_upsert() {
    FAIL_DIGEST_UPSERT.with(|flag| flag.set(true));
}

/// Leave the rewritten file in place when the next digest record fails,
/// as an interrupted or failed rollback would. Scoped to this thread.
#[cfg(feature = "test-support")]
pub fn fail_next_plugin_manifest_restore() {
    FAIL_RESTORE.with(|flag| flag.set(true));
}

pub(super) fn take_restore_failure() -> bool {
    FAIL_RESTORE.with(|flag| flag.replace(false))
}

pub(super) fn before_publish() {
    let hook = HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn take_digest_upsert_failure() -> bool {
    FAIL_DIGEST_UPSERT.with(|flag| flag.replace(false))
}
