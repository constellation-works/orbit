//! The selected-checkout cycle status read resident mandates need.

/// Read the versioned cycle switch without exposing scheduler status types.
pub(in crate::application) trait ResidentRoutineStatusPort {
    /// Return the disabled resident-cycle routine's name, or `None` when
    /// enabled, missing, or unreadable. A failed lookup never fails a mandate read.
    fn disabled_resident_cycle_routine_name(&self) -> Option<String>;
}
