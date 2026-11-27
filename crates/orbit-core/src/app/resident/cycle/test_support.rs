//! Deterministic control writes between observing a mandate and admitting a cycle.

use std::cell::RefCell;

thread_local! {
    static ADMISSION_CHECKPOINT: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// Run `checkpoint` once before live-cycle publication acquires the assignment
/// lock. This lets a control write commit after observe's initial active-state
/// read. Scoped to this thread, with the previous hook restored on unwind.
pub fn with_resident_cycle_admission_checkpoint<T>(
    checkpoint: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Box<dyn FnOnce()>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ADMISSION_CHECKPOINT.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore =
        Restore(ADMISSION_CHECKPOINT.with(|slot| slot.replace(Some(Box::new(checkpoint)))));
    operation()
}

pub(super) fn before_resident_cycle_publication() {
    let checkpoint = ADMISSION_CHECKPOINT.with(|slot| slot.borrow_mut().take());
    if let Some(checkpoint) = checkpoint {
        checkpoint();
    }
}
