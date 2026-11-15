use orbit_common::OrbitError;
#[cfg(test)]
use std::sync::{Arc, Barrier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerFault {
    AfterClaim,
    AfterMintBeforeCheckpoint,
    CheckpointWrite,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULT: std::cell::RefCell<Option<SchedulerFault>> =
        const { std::cell::RefCell::new(None) };
    static ADMISSION_BARRIER: std::cell::RefCell<Option<Arc<Barrier>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_LOAD_BARRIERS: std::cell::RefCell<Option<(Arc<Barrier>, Arc<Barrier>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_scheduler_fault(fault: Option<SchedulerFault>) {
    INJECTED_FAULT.with(|cell| *cell.borrow_mut() = fault);
}

#[cfg(test)]
pub(crate) fn set_admission_overlap_barrier(barrier: Option<Arc<Barrier>>) {
    ADMISSION_BARRIER.with(|cell| *cell.borrow_mut() = barrier);
}

#[cfg(test)]
pub(crate) fn set_after_load_barriers(barriers: Option<(Arc<Barrier>, Arc<Barrier>)>) {
    AFTER_LOAD_BARRIERS.with(|cell| *cell.borrow_mut() = barriers);
}

#[cfg(test)]
pub(super) fn wait_after_load() {
    let barriers = AFTER_LOAD_BARRIERS.with(|cell| cell.borrow().clone());
    if let Some((loaded, resume)) = barriers {
        loaded.wait();
        resume.wait();
    }
}

pub(super) fn fail_if_injected(_fault: SchedulerFault) -> Result<(), OrbitError> {
    #[cfg(test)]
    {
        let hit = INJECTED_FAULT.with(|cell| {
            let current = *cell.borrow();
            if current == Some(_fault) {
                *cell.borrow_mut() = None;
                true
            } else {
                false
            }
        });
        if hit {
            return Err(OrbitError::Store(format!(
                "injected scheduler interruption at {_fault:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn wait_for_admission_overlap() {
    let barrier = ADMISSION_BARRIER.with(|cell| cell.borrow().clone());
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}
