use crate::Core;
use crate::app::run::{self, Command};
use orbit_common::OrbitError;
use orbit_tools::ReservationOwnerContext;
use orbit_types::workflow::JobRunTrigger;
use serde_json::Value;

pub(super) fn invoke(
    core: &Core,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
    trigger: JobRunTrigger,
) -> Result<Value, OrbitError> {
    run::exec(
        core,
        Command::pipeline_invoke(input, agent, model, reservation_owner, trigger),
    )
    .map(|outcome| outcome.value)
}

pub(super) fn wait(
    core: &Core,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    run::exec(core, Command::pipeline_wait(input, agent, model)).map(|outcome| outcome.value)
}
