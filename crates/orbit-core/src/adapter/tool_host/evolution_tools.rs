//! The evolution tool handler.
//!
//! Evolution verbs are declared once as data in
//! `orbit_common::governance::evolution::operations`. The handler half of the
//! table is the evolution app (`crate::app::evolution`), joined to the spec half
//! by [`EvolutionVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::evolution::EvolutionVerb;
use serde_json::Value;

use crate::Core;
use crate::app::evolution::{self, Command, Input, Outcome};
use crate::app::tool_call::Caller;

/// Run one evolution verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: EvolutionVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    evolution::exec(
        core,
        Command::new(verb, Input::from_tool_entry(input, caller)),
    )
    .map(Outcome::into_value)
}
