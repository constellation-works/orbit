//! The friction tool handler [ORB-10358].
//!
//! Friction verbs are declared once as data in
//! `orbit_common::governance::friction::operations`. The handler half of the
//! table is the friction app (`crate::app::friction`), joined to the spec
//! half by [`FrictionVerb`]: this entry turns the tool call into the app's
//! command and calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use serde_json::Value;

use crate::Core;
use crate::app::friction::{self, Command, Input, Outcome};

/// Run one friction verb the generic tool entry admitted.
pub(super) fn dispatch(
    core: &Core,
    verb: FrictionVerb,
    input: Value,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    friction::exec(
        core,
        Command::new(verb, Input::from_tool_entry(input, model)),
    )
    .map(Outcome::into_value)
}
