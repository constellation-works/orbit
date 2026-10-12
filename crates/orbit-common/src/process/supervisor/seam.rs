//! Thread-scoped test seams for supervision, set with
//! [`crate::test_env::with_supervisor_seam`].
//!
//! Supervision reads them on the thread that calls
//! [`SupervisedChild::supervise`](super::SupervisedChild::supervise), so a
//! seam never reaches concurrent supervisors on other threads.

use std::cell::RefCell;
use std::io;
use std::process::{Child, ExitStatus};
use std::rc::Rc;
use std::time::Duration;

use wait_timeout::ChildExt;

/// How long supervision sleeps between polls of an injected wait hook that
/// reports the child still running.
const HOOKED_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A `try_wait`-style poll that replaces the supervisor's blocking wait.
pub type WaitHook = Box<dyn Fn(&mut Child) -> io::Result<Option<ExitStatus>>>;

/// Faults a test injects into supervisions on the current thread.
#[derive(Default)]
pub struct SupervisorSeam {
    /// Poll the child with this instead of blocking in a wait, for example to
    /// fail a wait at a chosen point while the child still runs.
    pub wait: Option<WaitHook>,
    /// Behave as if no wakeup socket pair could be created (`EMFILE`), so
    /// pipe workers fall back to polling their stop flag.
    pub no_wakeup_pair: bool,
}

thread_local! {
    static SEAM: RefCell<Option<Rc<SupervisorSeam>>> = const { RefCell::new(None) };
}

/// Apply `seam` to every supervision this thread runs during `operation`.
/// The previous seam is restored even on unwind.
pub fn with_supervisor_seam<T>(seam: SupervisorSeam, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Rc<SupervisorSeam>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SEAM.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore = Restore(SEAM.with(|slot| slot.replace(Some(Rc::new(seam)))));
    operation()
}

fn current() -> Option<Rc<SupervisorSeam>> {
    SEAM.with(|slot| slot.borrow().clone())
}

#[cfg(unix)]
pub(super) fn no_wakeup_pair() -> bool {
    current().is_some_and(|seam| seam.no_wakeup_pair)
}

/// Wait up to `slice` for the child to exit, through the injected hook when
/// one is set.
pub(super) fn wait_timeout(child: &mut Child, slice: Duration) -> io::Result<Option<ExitStatus>> {
    let Some(seam) = current() else {
        return child.wait_timeout(slice);
    };
    let Some(wait) = seam.wait.as_ref() else {
        return child.wait_timeout(slice);
    };
    let result = wait(child);
    if matches!(result, Ok(None)) {
        std::thread::sleep(slice.min(HOOKED_WAIT_POLL_INTERVAL));
    }
    result
}
