//! The `orbit.search` tool handler.
//!
//! Search verbs are declared once as data in
//! `orbit_common::governance::search`. The handler half of the table is the
//! search app (`crate::app::search`), joined to the spec half by
//! [`SearchVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::search::SearchVerb;
use serde_json::Value;

use crate::Core;
use crate::app::search::{self, Command, Input};
use crate::app::tool_call::Caller;

/// Run one search verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: SearchVerb,
    mut input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    // Direct host calls use the same canonical shape as the generic tool entry.
    orbit_common::governance::search::normalize_legacy_input(&mut input);
    search::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )?
    .into_value()
}
