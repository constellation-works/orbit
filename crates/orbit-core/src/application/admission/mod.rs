//! Backlog admission: which backlog tasks automatic dispatch may admit, and
//! when.

pub(crate) mod auto_admission;

pub(crate) mod backlog_exclusion;

pub(crate) mod census;

pub(crate) mod claim_holders;

pub(crate) mod cpu_light;

mod distributed_port;

pub(crate) mod forecast;

pub(crate) mod gap_fit;

mod job_port;

pub(crate) mod leaf_occupancy;

pub(crate) mod pass_error;

pub(crate) mod perf_lane;

mod published_delivery;

mod recovery_context;

pub(crate) mod scan_unresolved;

pub(crate) mod stuck_head;

pub(super) mod surface_reservation;

pub(crate) mod unknown_footprint;
