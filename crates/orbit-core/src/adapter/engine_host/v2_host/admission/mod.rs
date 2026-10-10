//! Admission and filing checks: which backlog tasks automatic dispatch may
//! admit, and whether a sweep finding is already owned by an existing task.

pub(super) mod auto_admission;
pub(super) mod backlog_exclusion;
pub(super) mod cpu_light;
pub(super) mod duplicate_tasks;
pub(super) mod leaf_occupancy;
pub(super) mod scan_unresolved;
pub(super) mod surface_reservation;
pub(super) mod sweep_filing;
pub(super) mod unknown_footprint;
