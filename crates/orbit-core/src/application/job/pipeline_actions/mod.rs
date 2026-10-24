//! Deterministic actions pipeline sequencers use to validate bundles, resolve
//! the delivery Job, check the bundle's footprint and reserve it once its
//! dependencies allow, re-check admission while a child runs, defer a starved
//! gate, and guard pipeline results. Dispatching and waiting on child Jobs is
//! engine transport and stays in `adapter::engine_host::actions::pipeline_actions`,
//! which calls `gate_admission` between waits.

mod bundles;
mod delivery;
mod gate_admission;
mod gate_reservation;
mod gate_starvation;
mod results;

pub(crate) use bundles::validate_bundles;
pub(crate) use delivery::resolve_delivery_job;
pub(crate) use gate_admission::{gate_admission_stop, wait_success_status};
pub(crate) use gate_reservation::{context_conflict_check, reserve_locks};
pub(crate) use gate_starvation::gate_starvation_step;
pub(crate) use results::{pipeline_success_guard, record_pipeline_results_audit};
