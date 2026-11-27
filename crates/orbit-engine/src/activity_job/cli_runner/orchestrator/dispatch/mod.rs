//! Provider subprocess dispatch phases, with the original admission and completion order.

mod post_run;
mod preparation;
mod provider;
mod subprocess;

use std::sync::Arc;

use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::Value;

use crate::context::RuntimeHost;

use crate::activity_job::audit_writer::V2AuditWriter;

pub(crate) use preparation::dispatch_cli_invocation;

/// One CLI step invocation's identity: policy authorizes `activity_name`
/// while audit and the worktree boundary report `step_id`.
pub(crate) struct CliDispatchContext<'a, H: ?Sized = dyn RuntimeHost + 'a> {
    pub(crate) host: &'a H,
    pub(crate) spec: &'a AgentLoopSpec,
    pub(crate) step_id: &'a str,
    pub(crate) activity_name: &'a str,
    pub(crate) run_id: &'a str,
    pub(crate) audit: &'a Arc<V2AuditWriter>,
    pub(crate) input: &'a Value,
    pub(crate) fs_profile: Option<&'a str>,
}

impl<H: ?Sized> Copy for CliDispatchContext<'_, H> {}

impl<H: ?Sized> Clone for CliDispatchContext<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}
