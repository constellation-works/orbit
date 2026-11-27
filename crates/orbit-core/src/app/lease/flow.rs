//! Path-lease operation flows and live notice feed.

use serde_json::Value;

use super::Intake;
use crate::runtime::stored::locks::lease;
use crate::{Core, OrbitError, OrbitRuntime};

/// A live notice feed for one admitted workspace and query filter.
#[derive(Clone)]
pub struct LeaseNoticeFeed {
    runtime: OrbitRuntime,
    input: Value,
}

impl LeaseNoticeFeed {
    /// Continue the same query after its previous page.
    pub fn after(&self, cursor: Value) -> Result<Value, OrbitError> {
        let mut input = self.input.clone();
        input["after"] = cursor;
        lease::notices(&self.runtime, input)
    }
}

pub(super) fn request(core: &Core, input: Value) -> Result<Value, OrbitError> {
    core.runtime().request_lease(input, None, None)
}

pub(super) fn renew(core: &Core, input: Value) -> Result<Value, OrbitError> {
    lease::renew(core.runtime(), input)
}

pub(super) fn expand(core: &Core, input: Value) -> Result<Value, OrbitError> {
    core.runtime().expand_lease(input, None, None)
}

pub(super) fn release(core: &Core, input: Value) -> Result<Value, OrbitError> {
    lease::release(core.runtime(), input, None, None)
}

pub(super) fn show(core: &Core, input: Value) -> Result<Value, OrbitError> {
    lease::show(core.runtime(), input)
}

pub(super) fn list(core: &Core) -> Result<Value, OrbitError> {
    lease::list(core.runtime())
}

pub(super) fn check(core: &Core, input: Value) -> Result<Value, OrbitError> {
    lease::check(core.runtime(), input)
}

pub(super) fn notices(core: &Core, input: Value) -> Result<Value, OrbitError> {
    lease::notices(core.runtime(), input)
}

pub(super) fn intake(core: &Core, intake: Intake) -> Result<Value, OrbitError> {
    let runtime = core.runtime();
    match intake {
        Intake::Request(input, grant) => runtime.request_lease_with_token(input, &grant),
        Intake::Renew(input, grant) => lease::renew_with_token(runtime, input, &grant),
        Intake::Expand(input, grant) => runtime.expand_lease_with_token(input, &grant),
        Intake::Release(input) => lease::release(runtime, input, None, None),
        Intake::Show(input) => lease::show(runtime, input),
        Intake::List => lease::list(runtime),
        Intake::Check(input) => lease::check(runtime, input),
        Intake::Notices(input) => lease::notices(runtime, input),
        Intake::Audit {
            operation,
            lease_id,
            status,
            payload,
        } => lease::record_lease_intake_audit(
            runtime,
            &operation,
            lease_id.as_deref(),
            status,
            payload,
        )
        .map(|()| Value::Null),
    }
}

pub(super) fn notice_feed(
    core: &Core,
    input: Value,
) -> Result<(Value, LeaseNoticeFeed), OrbitError> {
    let runtime = core.runtime();
    let first = lease::notices(runtime, input.clone())?;
    Ok((
        first,
        LeaseNoticeFeed {
            runtime: runtime.clone(),
            input,
        },
    ))
}
