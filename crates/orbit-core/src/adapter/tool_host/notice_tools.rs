//! The `orbit.notice.*` tool handler.
//!
//! Notice verbs are declared once as data in
//! `orbit_common::governance::notice`. The handler half of the table is the
//! notice app (`crate::app::notice`), joined to the spec half by
//! [`NoticeVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::notice::NoticeVerb;
use serde_json::Value;

use crate::Core;
use crate::app::notice::{self, Command, Input};
use crate::app::tool_call::Caller;

/// Run one notice verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: NoticeVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    notice::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )?
    .into_value()
}
