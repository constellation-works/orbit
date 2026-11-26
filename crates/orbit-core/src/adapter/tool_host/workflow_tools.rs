use crate::Core;
use crate::app::run::{self, Command};
use orbit_common::OrbitError;
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_types::workflow::JobRunTrigger;
use serde_json::Value;

pub(super) fn dispatch(
    core: &Core,
    session: &orbit_types::tool::ToolSessionContext,
    verb: WorkflowVerb,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    trigger: JobRunTrigger,
) -> Result<Value, OrbitError> {
    run::exec(
        core,
        Command::workflow(session, verb, input, agent, model, trigger),
    )
    .map(|outcome| outcome.value)
}
