//! Scoped monotonic clocks for boundary fixtures exercising delivery evidence.

use std::{cell::RefCell, rc::Rc, time::Instant};

type Clock = Rc<dyn Fn() -> Instant>;

thread_local! {
    static CLOCK: RefCell<Option<Clock>> = const { RefCell::new(None) };
}

/// Use a monotonic clock for all source budgets on this thread during `operation`.
/// Size limits and child completion checks still apply. Restore the previous
/// clock on return or unwind; other threads and production builds are unaffected.
pub fn with_delivery_source_clock<T>(
    clock: impl Fn() -> Instant + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let _restore = Restore(CLOCK.with(|slot| slot.replace(Some(Rc::new(clock)))));
    operation()
}

struct Restore(Option<Clock>);

impl Drop for Restore {
    fn drop(&mut self) {
        CLOCK.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn now() -> Option<Instant> {
    // Release the borrow before calling the clock, allowing nested scopes.
    CLOCK
        .with(|slot| slot.borrow().clone())
        .map(|clock| clock())
}
