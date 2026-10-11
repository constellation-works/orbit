//! Scoped deadline clocks for real subprocess boundary fixtures.

use std::{cell::RefCell, rc::Rc, time::Instant};

type Clock = Rc<dyn Fn() -> Instant>;

thread_local! {
    static CLOCK: RefCell<Option<Clock>> = const { RefCell::new(None) };
}

/// Use `clock` for CLI supervision deadlines on this thread during `operation`.
/// A fixture can freeze startup time, then advance past the deadline after the
/// child signals readiness. Process cleanup and output-reader guards retain
/// real clocks. The previous clock is restored even on unwind.
pub fn with_cli_supervision_clock<T>(
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
    CLOCK
        .with(|slot| slot.borrow().clone())
        .map(|clock| clock())
}
