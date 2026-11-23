//! A deterministic interleaving at the answer path: a hook runs after the
//! request lookup and before the task lock under which the answer is judged
//! and applied, so a test can land a competing task write in between.

use std::cell::RefCell;

thread_local! {
    static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

/// Take the hook before running it, so a hook that answers again does
/// not run itself a second time.
pub(super) fn notify() {
    let hook = HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// Run `operation` with `hook` called once, on this thread, between a
/// decision answer's request lookup and its locked apply.
pub fn with_decision_answer_checkpoint<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let _clear = Clear;
    operation()
}
