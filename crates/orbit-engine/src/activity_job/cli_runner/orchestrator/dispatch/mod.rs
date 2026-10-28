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

pub(crate) use preparation::run_cli_backend_for_step;

#[derive(Clone, Copy)]
struct CliDispatchContext<'a> {
    host: &'a dyn RuntimeHost,
    spec: &'a AgentLoopSpec,
    step_id: &'a str,
    activity_name: &'a str,
    run_id: &'a str,
    audit: &'a Arc<V2AuditWriter>,
    input: &'a Value,
    fs_profile: Option<&'a str>,
}
