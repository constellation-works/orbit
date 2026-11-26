use crate::Core;
use crate::app::run::{self, Command};
use orbit_common::OrbitError;
use orbit_common::governance::drain::DrainVerb;
use orbit_types::tool::ToolSessionContext;
pub(super) use run::prepare_input;
use serde_json::Value;

pub(super) fn dispatch(
    core: &Core,
    session: &ToolSessionContext,
    verb: DrainVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    run::exec(core, Command::drain(session, verb, input)).map(|outcome| outcome.value)
}
