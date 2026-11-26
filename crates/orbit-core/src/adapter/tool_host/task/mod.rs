//! The task tool handler.
//!
//! Task verbs are declared once as data in
//! `orbit_common::governance::task::operations`. The handler half of the
//! table is the task app (`crate::app::task`), joined to the spec half by
//! [`TaskVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::task::TaskVerb;
use serde_json::Value;

use crate::Core;
use crate::app::task::{self, Command, Input, Outcome, ToolCall};

/// Run one task verb the generic tool entry admitted. A write also answers
/// the id it persisted, for the redaction audit.
pub(super) fn dispatch(
    core: &Core,
    call: ToolCall<'_>,
    verb: TaskVerb,
    input: Value,
) -> Result<(Value, Option<String>), OrbitError> {
    task::exec(
        core,
        Command::Operation(verb, Input::from_tool_entry(input, call)),
    )
    .and_then(Outcome::into_tool_answer)
}
