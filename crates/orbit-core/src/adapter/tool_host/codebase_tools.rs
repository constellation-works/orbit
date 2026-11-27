//! The `orbit.codebase.*` tool handler.
//!
//! Codebase verbs are declared once as data in
//! `orbit_common::governance::codebase`. The handler half of the table is the
//! codebase app (`crate::app::codebase`), joined to the spec half by
//! [`CodebaseVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::codebase::CodebaseVerb;
use serde_json::Value;

use crate::Core;
use crate::app::codebase::{self, Command, Input};
use crate::app::tool_call::Caller;

/// Run one codebase verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: CodebaseVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    codebase::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, caller)),
    )?
    .into_value()
}
