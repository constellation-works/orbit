//! Core composition of source facts, existing lifecycle actions and evidence.

use crate::OrbitRuntime;
use orbit_common::OrbitError;

mod after_landing;
mod deadline;
mod direct;
mod evaluation;
pub(crate) mod incidents;
mod inspect;
pub(crate) mod members;
mod ownership;
mod pins;
pub(crate) mod preparation;
mod provider;
mod recovery;
mod reset;
pub(crate) mod source;
mod source_cache;
pub(crate) mod stall;
mod task;

pub use after_landing::{
    AfterLandingHealth, AfterLandingSource, after_landing_health, after_landing_switch,
};
pub(crate) use deadline::expiring_frozen_batch_tasks;
pub(crate) use direct::record_direct_landing_intent;
use evaluation::{auto_task_action_liveness, auto_task_admission_deferral};
pub use evaluation::{evaluate_auto_task, evaluate_routine};
pub(crate) use evaluation::{evaluate_auto_task_with_cache, evaluate_routine_with_cache};
pub use inspect::{
    UnadmittableDefinition, UnresolvableBranch, WedgedConsumer, delivery_ownership_refusal,
    inspect_auto_task, inspect_auto_task_with_open_instance, inspect_routine,
    unadmittable_delivery_definitions, unresolvable_delivery_branches, wedged_delivery_consumers,
};
pub use pins::{AttemptPinCleanup, pin_attempt_source, release_unreferenced_attempt_pins};
pub use recovery::recover_auto_task;
pub use reset::{ConsumerTeardown, reset_auto_task};
pub(crate) use reset::{consumer_teardown_refusals, tear_down_auto_task_consumer};
pub(crate) use source_cache::SourceCache;
pub use stall::{StalledConsumer, stalled_consumers, stalled_minutes};
pub(crate) use task::accepted_action_coverage;

#[cfg(test)]
mod tests;

pub use orbit_types::workflow::automation::COVERAGE_ARTIFACT;

/// Identity is machine/workspace-qualified in the authoritative host database.
pub fn consumer_key(runtime: &OrbitRuntime, kind: &str, name: &str) -> Result<String, OrbitError> {
    let machine = runtime.automation_machine_identity().ok_or_else(|| {
        OrbitError::InvalidInput(
            "delivery automation requires a registered machine identity".into(),
        )
    })?;
    Ok(format!(
        "{machine}/{}/{kind}/{name}",
        runtime.workspace_id()?
    ))
}
