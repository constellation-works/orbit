//! Public entry point for CLI backend dispatch.
use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, DispatchOutcome};
use super::dispatch::run_cli_backend_for_step;
use crate::context::RuntimeHost;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::Value;
use std::sync::Arc;

pub fn run_cli_backend(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    activity_name: &str,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
) -> Result<DispatchOutcome, DispatchError> {
    run_cli_backend_for_step(
        host,
        spec,
        activity_name,
        activity_name,
        run_id,
        audit,
        input,
        fs_profile,
    )
}
