//! Deterministic interleavings at the public owner pull boundary.

use std::cell::RefCell;

thread_local! {
    static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

pub(super) fn notify() {
    let hook = HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// Run a hook after pull selection reads holds and before transactional admission.
pub fn with_pull_admission_checkpoint<T>(
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
    let _guard = Clear;
    operation()
}
