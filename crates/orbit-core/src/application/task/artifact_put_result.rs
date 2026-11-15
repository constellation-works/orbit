//! The compact default artifact attachment result, shared by CLI transports.
//!
//! MCP serves the complete write response. CLI transports apply this policy
//! only after handling their explicit full/field profiles, so those profiles
//! keep their existing contracts.

use orbit_common::governance::task::{MINIMAL_TASK_FIELDS, select_fields};
use serde_json::Value;

const ARTIFACT_PUT_WRITE_SIDECARS: &[&str] = &[
    "warnings",
    "warning",
    "cycle_routine_disabled",
    "cycle_routine_name",
    "audit_warning",
    "context_files_unverified",
    "redactions_applied",
    "redactions",
];

/// Finalize a tool's default CLI result. Artifact attachment keeps the compact
/// task fields and every write sidecar; other tools keep their own result.
/// Full and explicit field profiles must bypass this default projection.
pub fn artifact_put_default_result(tool_name: &str, output: Value) -> Value {
    if tool_name != "orbit.task.artifact.put" {
        return output;
    }
    let Value::Object(object) = output else {
        return output;
    };
    let fields = MINIMAL_TASK_FIELDS
        .iter()
        .chain(ARTIFACT_PUT_WRITE_SIDECARS)
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    Value::Object(select_fields(object, &fields))
}
