//! Backlog admission: which backlog tasks automatic dispatch may admit, and
//! when.

mod auto_admission;

mod backlog_exclusion;

mod claim_holder;

mod cpu_light;

mod distributed_port;

mod executor_order;

mod forecast;

mod gap_fit;

mod job_port;

mod leaf_occupancy;

mod pass_error;

mod perf_lane;

mod published_delivery;

mod recovery_context;

mod scan_unresolved;

mod stuck_head;

mod surface_reservation;

mod unknown_footprint;

pub(crate) use auto_admission::{
    AdmissionConflict, AdmissionHolders, AdmissionSelection, DeferralReason, SelectionContext,
    select_admissions,
};
pub(crate) use backlog_exclusion::{
    BacklogSnapshot, BacklogTaskExclusion, BacklogTaskExclusionReason, allowlist_from_input,
    backlog_snapshot, backlog_snapshot_observed, list_backlog_tasks, no_drain_exclusion,
    sort_tasks_for_automatic_dispatch, workspace_ship_mode,
};
pub(crate) use claim_holder::ClaimHolders;
pub(crate) use cpu_light::{
    CPU_LIGHT_BUDGET_FULL, CPU_LIGHT_LEAVES, LightBudget, ResourceGate, is_cpu_light,
};
pub(crate) use distributed_port::DistributedPort;
pub(crate) use executor_order::record_owner_census;
pub(crate) use forecast::{
    ForecastAdmission, ForecastDrain, ForecastHostEvidence, ForecastPassScratch, ForecastTaskSet,
    ForecastWait, RESOURCE_RECHECK_HOURS, forecast_admission,
};
pub(crate) use gap_fit::{GapFit, GapFitMode, HeadWait, task_lookup};
pub(crate) use job_port::JobPort;
pub(crate) use leaf_occupancy::{occupancy_json, read_leaf_occupancy};
pub(crate) use pass_error::PassError;
pub(crate) use published_delivery::acquire_deferred_footprint;
pub(crate) use scan_unresolved::scan_unresolved_work;
pub(crate) use stuck_head::report_stuck_head;
pub(crate) use unknown_footprint::{FootprintGuard, FootprintWait, whole_tree_holder};
