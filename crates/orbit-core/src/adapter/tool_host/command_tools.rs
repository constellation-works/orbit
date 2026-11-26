use crate::Core;
use crate::app::run::{self, Command};
use orbit_common::OrbitError;
use serde_json::Value;

pub(super) fn exec(
    core: &Core,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    run::exec(core, Command::remote_command(input, agent, model)).map(|outcome| outcome.value)
}
