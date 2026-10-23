//! Backlog admission: which backlog tasks automatic dispatch may admit, and
//! when.

pub(super) mod auto_admission;
pub(crate) mod backlog_exclusion;
pub(crate) mod census;
pub(crate) mod claim_holders;
pub(super) mod cpu_light;
pub(super) mod gap_fit;
pub(super) mod leaf_occupancy;
pub(crate) mod pass_error;
mod published_delivery;
pub(crate) mod scan_unresolved;
pub(super) mod stuck_head;
pub(super) mod surface_reservation;
pub(super) mod unknown_footprint;
