//! Owner-side handlers for the shared path-lease registry.

use orbit_common::OrbitError;
use orbit_common::governance::lease::LeaseVerb;
use orbit_common::governance::operation::OperationVerb;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::task::locks::lease;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: LeaseVerb,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    // Even reads answer for the owner's lock state; replicas cannot serve them.
    runtime.ensure_coordination_task_write_permitted()?;
    let input = verb.spec().parse_input(input)?.into_value();
    match verb {
        LeaseVerb::Request => lease::request(runtime, input, agent, model),
        LeaseVerb::Renew => lease::renew(runtime, input),
        LeaseVerb::Release => lease::release(runtime, input, agent, model),
        LeaseVerb::Show => lease::show(runtime, input),
        LeaseVerb::List => lease::list(runtime),
        LeaseVerb::Check => lease::check(runtime, input),
        LeaseVerb::Notices => lease::notices(runtime, input),
    }
}
