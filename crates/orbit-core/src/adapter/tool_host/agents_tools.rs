//! The `orbit.agents.*` tool handler.
//!
//! Agents verbs are declared once as data in
//! `orbit_common::governance::agents`. The handler half of the table is the
//! agent app (`crate::app::agent`), joined to the spec half by
//! [`AgentsVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::agents::AgentsVerb;
use serde_json::Value;

use crate::Core;
use crate::app::agent::{self, Command, Input};
use crate::app::tool_call::Caller;

/// Run one agents verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: AgentsVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    agent::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )?
    .into_value()
}
