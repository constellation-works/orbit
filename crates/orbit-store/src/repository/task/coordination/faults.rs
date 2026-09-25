//! Test fault injection at the commit protocol's crash points.

use super::CoordinationFault;
#[cfg(test)]
use super::INJECTED_FAULTS;
use orbit_common::OrbitError;

#[cfg(test)]
pub(crate) fn inject_coordination_faults(faults: &[CoordinationFault]) {
    INJECTED_FAULTS.with(|cell| {
        *cell.borrow_mut() = faults.iter().copied().collect();
    });
}

pub(super) fn fail_if_injected(_fault: CoordinationFault) -> Result<(), OrbitError> {
    #[cfg(test)]
    {
        let hit = INJECTED_FAULTS.with(|cell| cell.borrow_mut().remove(&_fault));
        if hit {
            return Err(OrbitError::Store(format!(
                "injected coordination failure at {_fault:?}"
            )));
        }
    }
    Ok(())
}
