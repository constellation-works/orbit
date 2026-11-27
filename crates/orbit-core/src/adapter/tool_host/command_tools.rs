use orbit_common::OrbitError;
use serde_json::Value;

use crate::Core;
use crate::app::remote_command::{self, Command, Outcome};

pub(super) fn exec(
    core: &Core,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    remote_command::exec(
        core,
        Command::Exec {
            input,
            agent,
            model,
        },
    )
    .map(Outcome::into_value)
}
