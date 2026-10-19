use std::cell::RefCell;

use orbit_common::OrbitError;

type Failure = Box<dyn FnOnce() -> OrbitError>;

thread_local! {
    static HOOK: RefCell<Option<Failure>> = RefCell::new(None);
}

pub(super) fn take() -> Option<OrbitError> {
    HOOK.with(|slot| slot.borrow_mut().take())
        .map(|fail| fail())
}

/// Run `operation` with the first admission-reservation release a drain
/// pass attempts on this thread failing with `fail()`.
pub fn with_reservation_release_failure<T>(
    fail: impl FnOnce() -> OrbitError + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(fail)));
    let _guard = Clear;
    operation()
}
