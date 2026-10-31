//! Deterministic interleavings at the public owner pull and settlement
//! boundaries.

use std::cell::RefCell;

thread_local! {
    static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    static SETTLE_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

type HookSlot = std::thread::LocalKey<RefCell<Option<Box<dyn FnOnce()>>>>;

/// Take the hook before running it, so a hook that re-enters the
/// checkpointed operation does not run itself again.
fn take_and_run(slot: &'static HookSlot) {
    let hook = slot.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

fn with_hook<T>(
    slot: &'static HookSlot,
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Clear(&'static HookSlot);
    impl Drop for Clear {
        fn drop(&mut self) {
            self.0.with(|slot| *slot.borrow_mut() = None);
        }
    }
    slot.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let _guard = Clear(slot);
    operation()
}

pub(super) fn notify() {
    take_and_run(&HOOK);
}

pub(super) fn notify_settle() {
    take_and_run(&SETTLE_HOOK);
}

/// Run a hook after pull selection reads holds and before transactional admission.
pub fn with_pull_admission_checkpoint<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    with_hook(&HOOK, hook, operation)
}

/// Run a hook after a claim settlement reads the claim and before it reads
/// the task, so another settlement can commit in between.
pub fn with_claim_settle_checkpoint<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    with_hook(&SETTLE_HOOK, hook, operation)
}
