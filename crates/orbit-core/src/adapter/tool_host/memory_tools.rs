//! The `orbit.memory.*` tool handler.
//!
//! Memory verbs are declared once as data in
//! `orbit_common::governance::memory`. The handler half of the table is the
//! crew memory app (`crate::app::crew_memory`), joined to the spec half by
//! [`MemoryVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::memory::MemoryVerb;
use serde_json::Value;

use crate::Core;
use crate::app::crew_memory::{self, Command, Input, Outcome};
use crate::app::tool_call::Caller;

/// Run one memory verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: MemoryVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    crew_memory::exec(
        core,
        Command::new(verb, Input::from_tool_entry(input, caller)),
    )
    .map(Outcome::into_value)
}
