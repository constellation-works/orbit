use std::cell::RefCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum Point {
    /// The graph is discovered and no task lock is held yet.
    Discovered,
    /// Every lock is held and nothing has been read under them.
    Locked,
}

pub(in super::super) type Hook = Box<dyn FnMut(Point)>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
}

pub(in super::super) fn set(hook: Option<Hook>) {
    HOOK.with(|slot| *slot.borrow_mut() = hook);
}

pub(super) fn fire(point: Point) {
    HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(point);
        }
    });
}
