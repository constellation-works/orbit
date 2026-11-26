//! The automation tool handler.
//!
//! Automation verbs are declared once as data in
//! `orbit_common::governance::automation::operations`. The handler half of the
//! table is the automation app (`crate::app::automation`), joined to the spec half
//! by [`AutomationVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::automation::AutomationVerb;
use serde_json::Value;

use crate::Core;
use crate::app::automation::{self, Command, Input, Outcome};
use crate::app::tool_call::Caller;

/// Run one automation verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: AutomationVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    automation::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )
    .map(Outcome::into_value)
}
