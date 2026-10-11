//! A thread-local rendezvous for deterministic managed-auto-task races.

use std::cell::RefCell;

type ReadHook = Box<dyn FnOnce()>;

thread_local! {
    static AFTER_MANIFEST_READ: RefCell<Option<ReadHook>> = const { RefCell::new(None) };
}

/// Run an operation with a one-shot callback after its first auto-task
/// reconciliation manifest read. The callback runs on the calling thread;
/// other catalogs and threads are unaffected. Prior state is restored even
/// if the callback or operation unwinds.
pub fn with_auto_task_manifest_read_hook<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let _restore = RestoreHook(AFTER_MANIFEST_READ.with(|slot| slot.replace(Some(Box::new(hook)))));
    operation()
}

struct RestoreHook(Option<ReadHook>);

impl Drop for RestoreHook {
    fn drop(&mut self) {
        AFTER_MANIFEST_READ.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn after_auto_task_manifest_read() {
    let hook = AFTER_MANIFEST_READ.with(|slot| slot.take());
    if let Some(hook) = hook {
        hook();
    }
}
