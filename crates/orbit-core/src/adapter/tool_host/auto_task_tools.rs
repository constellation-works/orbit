//! The auto-task tool handler.
//!
//! Auto-task verbs are declared once as data in
//! `orbit_common::governance::auto_task::operations`. The handler half of the
//! table is the auto-task app (`crate::app::auto_task`), joined to the spec half
//! by [`AutoTaskVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::auto_task::AutoTaskVerb;
use serde_json::Value;

use crate::Core;
use crate::app::auto_task::{self, Command, Input, Outcome};
use crate::app::tool_call::Caller;

/// Run one auto-task verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: AutoTaskVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    auto_task::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )
    .map(Outcome::into_value)
}
