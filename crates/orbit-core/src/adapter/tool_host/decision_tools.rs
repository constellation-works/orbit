//! The decision tool handler.
//!
//! Decision verbs are declared once as data in
//! `orbit_common::governance::decision::operations`. The handler half of the
//! table lives in the task app (`crate::app::task`), joined to the spec half
//! by [`DecisionVerb`]: this entry turns the tool call into the app's command
//! and calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::decision::DecisionVerb;
use serde_json::Value;

use crate::Core;
use crate::app::task::{self, Command, Input, Outcome, ToolCall};

/// Run one decision verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    call: ToolCall<'_>,
    verb: DecisionVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    task::exec(
        core,
        Command::Decision(verb, Input::from_tool_entry(input, call)),
    )
    .and_then(Outcome::into_tool_answer)
    .map(|(answer, _)| answer)
}
