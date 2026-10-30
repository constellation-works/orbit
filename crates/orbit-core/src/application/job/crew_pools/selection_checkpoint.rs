//! Deterministic interleaving at the admission selection/insertion boundary.

use std::cell::RefCell;

thread_local! {
    static SELECTION_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

/// Run `hook` after crew selection and before durable run insertion, so tests
/// can attempt a concurrent scope edit without scheduling or load assumptions.
pub fn with_crew_selection_checkpoint<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            SELECTION_HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
    SELECTION_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let _guard = Clear;
    operation()
}

pub(super) fn notify_crew_selection_checkpoint() {
    let hook = SELECTION_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
