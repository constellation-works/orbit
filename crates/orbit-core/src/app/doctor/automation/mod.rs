//! Delivery automation, pull-drain, routine, environment and review rows of
//! `orbit doctor`. Each check family has its own file; this module declares
//! them and re-exports the entry points that `doctor/mod.rs` imports.

mod artifacts;
mod environment;
mod pull;
mod review;
mod schedule;
mod stalled;

pub(super) use artifacts::{doctor_check_definition_artifacts, doctor_check_task_relations};
pub(super) use environment::{doctor_check_env_pass, doctor_check_validation_env};
pub(super) use pull::{
    doctor_check_pull_gates, doctor_check_pull_protocol, doctor_check_pull_settlements,
};
pub(super) use review::doctor_check_review;
pub(super) use schedule::{doctor_check_host_shutdown, doctor_check_routine_freshness};
pub(super) use stalled::{doctor_check_job_runs, doctor_check_stalled_automation};
