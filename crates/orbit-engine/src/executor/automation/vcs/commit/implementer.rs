//! Refuse a missing claimed clean-tree handoff at the implement step, before
//! commit would discover that the response fields were lost.

use orbit_common::OrbitError;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, refuses_implementer_writes};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::git::git_output;
use super::scope::{NewPathPolicy, task_candidate_paths};

/// Response fields stay advisory for edited candidates and owner runs, whose
/// evidence is already attached to durable task state. A claimed clean base
/// needs scratch artifact references because only the commit step can import
/// its evidence to the owner. This checks presence, not the report's truth;
/// the existing clean-tree verifier remains the delivery authority.
pub(crate) fn require_implementer_no_diff_artifacts<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    output: &Value,
) -> Result<(), OrbitError> {
    if input.get("claimed").and_then(Value::as_bool) != Some(true)
        || output
            .get("no_diff_artifacts")
            .and_then(Value::as_array)
            .is_some_and(|artifacts| !artifacts.is_empty())
    {
        return Ok(());
    }
    let required = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| OrbitError::InvalidInput(format!("{key} is required")))
    };
    let workspace = std::path::Path::new(required("workspace_path")?);
    let base = required("base_sha")?;
    if git_output(workspace, &["rev-parse", "HEAD"])? != base
        || !task_candidate_paths(workspace, NewPathPolicy::Claimed)?.is_empty()
    {
        return Ok(());
    }
    let task = host.get_task(required("task_id")?)?;
    if refuses_implementer_writes(task.status)
        || task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
    {
        return Ok(());
    }
    let parse_error = output
        .get("response_envelope_error")
        .and_then(Value::as_str)
        .filter(|error| !error.trim().is_empty())
        .map(|error| format!("; {error}"))
        .unwrap_or_default();
    Err(OrbitError::Execution(format!(
        "claimed implementation left its pinned base unchanged but returned no \
         `no_diff_artifacts`. Return a JSON response result containing the clean-tree report \
         and every referenced validation log's scratch path{parse_error}"
    )))
}
