//! Refuse a missing claimed clean-tree handoff at the implement step, before
//! commit would discover that the response fields were lost.

use orbit_common::OrbitError;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, refuses_implementer_writes};
use orbit_types::workflow::handoff::{ALREADY_RESOLVED_FIELD, AlreadyResolved};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::git::git_output;
use super::scope::{NewPathPolicy, task_candidate_paths};

/// Response fields stay advisory for edited candidates and owner runs, whose
/// evidence is already attached to durable task state. A claimed clean base
/// needs scratch artifact references because only the commit step can import
/// its evidence to the owner. This checks presence, not the report's truth;
/// the existing clean-tree verifier remains the delivery authority.
///
/// [ORB-15747] An already-resolved outcome is a claimed leaf's, whose claim
/// host replays its tests; its selectors are admitted here, before commit,
/// and again where they run.
pub(crate) fn require_implementer_no_diff_artifacts<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    output: &Value,
) -> Result<(), OrbitError> {
    let claimed = input.get("claimed").and_then(Value::as_bool) == Some(true);
    let already_resolved = admit_implementer_already_resolved(output, claimed)?;
    // ORB-22407: a claimed already-resolved outcome is evidence the commit
    // step can settle through its already-resolved checkpoint — the named
    // selectors replay on the base during claimed validation — so it needs
    // no clean-tree report beside it. A summary alone remains refused, as
    // before: it names no check that could show the base resolves the task.
    if !claimed
        || already_resolved
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

/// Admit selectors only on the claimed route, whose host replays them before
/// handoff. Owner implement and commit gates share this refusal so an owner
/// cannot settle an empty stage using tests that nobody executed [ORB-23194].
pub(super) fn admit_implementer_already_resolved(
    output: &Value,
    claimed: bool,
) -> Result<bool, OrbitError> {
    let Some(declared) = AlreadyResolved::from_output(output).map_err(OrbitError::InvalidInput)?
    else {
        return Ok(false);
    };
    if !claimed {
        return Err(OrbitError::InvalidInput(format!(
            "`{ALREADY_RESOLVED_FIELD}` is available only to claimed runs, whose claim host \
             replays the selectors; on an owner run, run validation and attach `no-diff.json` \
             or `already-landed.json` and every referenced validation log with \
             `orbit.task.artifact.put` before returning a clean-tree outcome"
        )));
    }
    declared.admit().map_err(OrbitError::InvalidInput)?;
    Ok(true)
}
