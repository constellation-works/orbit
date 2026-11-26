use super::{Command, Operation};
use crate::runtime::stored::locks::lease;
use orbit_common::governance::authorization::{AuthorizationDenial, DASHBOARD_LEASE_WRITE};
use serde_json::Value;

pub enum LeaseWrite {
    Request,
    Renew,
    Expand,
    Release,
}

impl Command<'static, Value> {
    /// Admit the dashboard write before opening the selected workspace.
    pub fn lease_write(
        action: LeaseWrite,
        input: Value,
        operator: bool,
    ) -> Result<Self, AuthorizationDenial> {
        super::exec::dashboard_authorize(&DASHBOARD_LEASE_WRITE, operator)?;
        let name = match action {
            LeaseWrite::Request => "request",
            LeaseWrite::Renew => "renew",
            LeaseWrite::Expand => "expand",
            LeaseWrite::Release => "release",
        };
        Ok(Self::new(Operation::command("lease", name), move |core| {
            let rt = core.runtime();
            match action {
                LeaseWrite::Request => rt.request_lease(input, None, None),
                LeaseWrite::Renew => lease::renew(rt, input),
                LeaseWrite::Expand => rt.expand_lease(input, None, None),
                LeaseWrite::Release => lease::release(rt, input, None, None),
            }
        }))
    }
}

impl<'a> Command<'a, Value> {
    /// Run the lease show operation.
    pub fn lease_show(input: Value) -> Self {
        Self::new(Operation::command("lease", "show"), move |core| {
            lease::show(core.runtime(), input)
        })
    }
}

impl<'a> Command<'a, Value> {
    /// Run the lease list operation.
    pub fn lease_list() -> Self {
        Self::new(Operation::command("lease", "list"), move |core| {
            lease::list(core.runtime())
        })
    }
}

impl<'a> Command<'a, Value> {
    /// Run the lease check operation.
    pub fn lease_check(input: Value) -> Self {
        Self::new(Operation::command("lease", "check"), move |core| {
            lease::check(core.runtime(), input)
        })
    }
}

impl<'a> Command<'a, Value> {
    /// Run the lease notices operation.
    pub fn lease_notices(input: Value) -> Self {
        Self::new(Operation::command("lease", "notices"), move |core| {
            lease::notices(core.runtime(), input)
        })
    }
}

/// A live notice feed for one admitted workspace and filter.
#[derive(Clone)]
pub struct LeaseNoticeFeed {
    runtime: crate::OrbitRuntime,
    input: Value,
}

impl LeaseNoticeFeed {
    /// Continue the same query after its previous page.
    pub fn after(&self, cursor: Value) -> Result<Value, crate::OrbitError> {
        let mut input = self.input.clone();
        input["after"] = cursor;
        lease::notices(&self.runtime, input)
    }
}

impl Command<'static, (Value, LeaseNoticeFeed)> {
    /// Open the feed and read its first page before sending the SSE headers.
    pub fn lease_notice_feed(input: Value) -> Self {
        Self::new(Operation::command("lease", "notices"), move |core| {
            let runtime = core.runtime();
            let first = lease::notices(runtime, input.clone())?;
            Ok((
                first,
                LeaseNoticeFeed {
                    runtime: runtime.clone(),
                    input,
                },
            ))
        })
    }
}
