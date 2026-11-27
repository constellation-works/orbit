//! The experiment tool handler.
//!
//! Experiment verbs are declared once as data in
//! `orbit_common::governance::experiment::operations`. The handler half of the
//! table is the experiment app (`crate::app::experiment`), joined to the spec half
//! by [`ExperimentVerb`]: this entry turns the tool call into the app's command and
//! calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::experiment::ExperimentVerb;
use serde_json::Value;

use crate::Core;
use crate::app::experiment::{self, Command, Input, Outcome};
use crate::app::tool_call::Caller;

/// Run one experiment verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: ExperimentVerb,
    input: Value,
    caller: Caller,
) -> Result<Value, OrbitError> {
    experiment::exec(
        core,
        Command::new(verb, Input::from_tool_entry(input, caller)),
    )
    .map(Outcome::into_value)
}
