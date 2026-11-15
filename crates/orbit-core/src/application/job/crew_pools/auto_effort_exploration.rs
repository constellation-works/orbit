//! Switch off `<crew>@auto` effort exploration in tests.
//!
//! A fixed share of seeded `@auto` draws runs another effort the bands name,
//! and a task pilot's seed carries its run id, which changes every minute, so
//! whether a draw explores changes from run to run. A test that asserts the
//! band's effort pins it here.

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide, because a pipeline run resolves its crews on worker threads.
static EXPLORATION_OFF: AtomicBool = AtomicBool::new(false);

/// Run `operation` with every `<crew>@auto` resolution in this process taking
/// its band's effort, never an explored one. The previous setting is
/// restored on return or unwind. Use it only in a test that owns its process
/// (each nextest test does), since every thread sees the switch.
pub fn without_auto_effort_exploration<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            EXPLORATION_OFF.store(self.0, Ordering::SeqCst);
        }
    }
    let _restore = Restore(EXPLORATION_OFF.swap(true, Ordering::SeqCst));
    operation()
}

/// Whether a test switched exploration off in this process.
pub(super) fn auto_effort_exploration_off() -> bool {
    EXPLORATION_OFF.load(Ordering::SeqCst)
}
