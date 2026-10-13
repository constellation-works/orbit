//! Fixed retry waits; fixtures observe the requested delays without wall time.

use std::time::Duration;

#[cfg(feature = "test-support")]
mod fixture {
    use std::{cell::RefCell, rc::Rc, time::Duration};

    type Wait = Rc<dyn Fn(Duration)>;
    thread_local! {
        static WAIT: RefCell<Option<Wait>> = const { RefCell::new(None) };
    }

    /// Replace fixed VCS retry waits on this thread for one fixture operation.
    /// Real subprocess deadlines and process cleanup retain their real clocks.
    pub fn with_vcs_retry_wait<T>(
        wait: impl Fn(Duration) + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<Wait>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WAIT.with(|slot| slot.replace(self.0.take()));
            }
        }
        let _restore = Restore(WAIT.with(|slot| slot.replace(Some(Rc::new(wait)))));
        operation()
    }

    pub(super) fn wait(duration: Duration) -> bool {
        let wait = WAIT.with(|slot| slot.borrow().clone());
        if let Some(wait) = wait {
            wait(duration);
            true
        } else {
            false
        }
    }
}

#[cfg(feature = "test-support")]
pub use fixture::with_vcs_retry_wait;

pub(super) fn sleep(duration: Duration) {
    #[cfg(feature = "test-support")]
    if fixture::wait(duration) {
        return;
    }
    std::thread::sleep(duration);
}
