//! Test-only hook that runs once inside an open tail-read snapshot.

thread_local! {
    static TAIL_SNAPSHOT_RENDEZVOUS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// Install a hook that runs once, after the tail count and before the page
/// select, while the read snapshot is open.
/// Visible to `driver::sqlite` only so sibling tests can force this interleaving.
pub(in crate::driver::sqlite) fn set_tail_snapshot_rendezvous(hook: impl FnOnce() + 'static) {
    TAIL_SNAPSHOT_RENDEZVOUS.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

// Visible to `driver::sqlite` only so sibling tests can clear their rendezvous hook.
pub(in crate::driver::sqlite) fn clear_tail_snapshot_rendezvous() {
    TAIL_SNAPSHOT_RENDEZVOUS.with(|slot| *slot.borrow_mut() = None);
}

pub(super) fn fire_tail_snapshot_rendezvous() {
    if let Some(hook) = TAIL_SNAPSHOT_RENDEZVOUS.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
}
