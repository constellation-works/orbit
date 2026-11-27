//! The resident tool handler.
//!
//! Resident verbs are declared once as data in
//! `orbit_common::governance::resident::operations`. The handler half of the
//! table is the resident app (`crate::app::resident`), joined to the spec half
//! by [`ResidentVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::resident::ResidentVerb;
use serde_json::Value;

use crate::Core;
use crate::app::resident::{self, Command, Input, Outcome};
use crate::app::tool_call::Caller;

/// Run one resident verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: ResidentVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    resident::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )
    .map(Outcome::into_value)
}
