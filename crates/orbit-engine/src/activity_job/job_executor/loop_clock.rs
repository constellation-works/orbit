//! Monotonic loop timing, with a thread-scoped deterministic fixture seam.
use std::time::{Duration, Instant};

pub(super) fn now() -> Instant {
    #[cfg(feature = "test-support")]
    if let Some(clock) = fixture::clock() {
        return (clock.0)();
    }
    Instant::now()
}

pub(super) fn sleep(duration: Duration) {
    #[cfg(feature = "test-support")]
    if let Some(clock) = fixture::clock() {
        (clock.1)(duration);
        return;
    }
    std::thread::sleep(duration);
}

#[cfg(feature = "test-support")]
mod fixture {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    pub(super) type Clock = Rc<(Box<dyn Fn() -> Instant>, Box<dyn Fn(Duration)>)>;
    thread_local! {
        static CLOCK: RefCell<Option<Clock>> = const { RefCell::new(None) };
    }

    /// Replace loop time and sleep on this thread until `run` returns or unwinds.
    /// Other deadlines and subprocess supervision retain real time.
    pub fn with_loop_clock<T>(
        now: impl Fn() -> Instant + 'static,
        sleep: impl Fn(Duration) + 'static,
        run: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<Clock>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CLOCK.with(|slot| slot.replace(self.0.take()));
            }
        }
        let _restore = Restore(
            CLOCK.with(|slot| slot.replace(Some(Rc::new((Box::new(now), Box::new(sleep)))))),
        );
        run()
    }

    pub(super) fn clock() -> Option<Clock> {
        CLOCK.with(|slot| slot.borrow().clone())
    }
}

#[cfg(feature = "test-support")]
pub use fixture::with_loop_clock;
