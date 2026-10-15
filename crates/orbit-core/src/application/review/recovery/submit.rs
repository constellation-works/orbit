//! Atomic admission and payload-bound lost-reply reconciliation.
use super::{observe, refused, reservations};
use crate::OrbitRuntime;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::required_string;
use orbit_store::contracts::{TaskCoordinationCommitOutcome, TaskCoordinationCommitParams};
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    HELD_RECOVERY_ADMITTED, HeldRecoveryAdmission, HeldRecoveryBinding, PR_AUTO_FIX_BUDGET,
};
use serde_json::{Value, json};

pub(super) fn submit(
    runtime: &OrbitRuntime,
    id: &str,
    input: &Value,
    actor: &str,
    provenance: &str,
) -> Result<Value, OrbitError> {
    let key = required_string(input, &["request_key"], "request_key")?;
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err(refused(
            "request_key must be 1-128 ASCII letters, digits, '-', '_', '.' or ':'",
        ));
    }
    let binding: HeldRecoveryBinding = serde_json::from_value(input["binding"].clone())
        .map_err(|e| refused(format!("inspect and supply the exact binding: {e}")))?;
    if binding.task_id != id || binding.workspace_id != runtime.workspace_id()? {
        return Err(refused("foreign task or workspace binding"));
    }
    if input
        .get("disposition")
        .is_some_and(|v| serde_json::to_value(binding.disposition).ok().as_ref() != Some(v))
        || input.get("original").is_some()
        || input.get("repair_selectors").is_some()
    {
        return Err(refused(
            "submit only the inspected binding, request key and rationale; disposition must agree with the binding",
        ));
    }
    let reason = required_string(input, &["reason"], "reason")?;
    if reason.trim().is_empty() || reason.len() > 32768 {
        return Err(refused("reason must be 1-32768 bytes"));
    }
    let records = reservations(runtime, id)?;
    // Resolve a key before re-observing a task already rebound to its new
    // claim. Replaying is read-only, including after that claim terminates.
    if let Some(existing) = records.iter().find(|r| r.request_key == key) {
        if existing.binding != binding
            || existing.reason != reason
            || existing.authorized_by != actor
            || existing.authorizer_provenance != provenance
        {
            return Err(refused(
                "request_key is already bound to different input or operator identity",
            ));
        }
        return Ok(
            json!({"id":id,"admission":existing,"replayed":true,"continuations":super::continuations(runtime,id)?}),
        );
    }
    let task = runtime.get_task(id)?;
    let runs = runtime.stores().jobs().list_all_pending_or_running_runs()?;
    if runs.iter().any(|run| {
        !run.state.is_terminal()
            && run.input.as_ref().is_some_and(|i| {
                i["task_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|value| value == id))
            })
    }) {
        return Err(refused(
            "another delivery or preparation is active for this task",
        ));
    }
    let mut inspect = json!({"disposition":binding.disposition,"original":binding.original,"repair_selectors":binding.repair_selectors});
    // The disposition in the submitted payload must agree with the inspected
    // one; it is never inferred from a failed validation outcome.
    if input
        .get("disposition")
        .is_some_and(|v| *v != inspect["disposition"])
    {
        return Err(refused("submitted disposition differs from the binding"));
    }
    inspect["id"] = json!(id);
    let observation = observe::observe(runtime, id, &inspect)?;
    if observation.binding != binding {
        return Err(refused(
            "binding is stale; inspect source, meaning, scope and lease again",
        ));
    }
    if records.iter().any(|r| {
        r.binding.claim_id == binding.claim_id && r.binding.candidate_sha == binding.candidate_sha
    }) {
        return Err(refused(
            "this source candidate already has an admission; replay its request_key",
        ));
    }
    if records
        .iter()
        .filter(|r| r.binding.pull_request_url == binding.pull_request_url)
        .count()
        >= PR_AUTO_FIX_BUDGET as usize
    {
        return Err(refused("pull request recovery budget exhausted"));
    }
    let admission = HeldRecoveryAdmission {
        request_key: key,
        binding,
        reason,
        evidence: observation.diagnostics,
        authorized_by: actor.into(),
        authorizer_provenance: provenance.into(),
        authorized_at: Utc::now(),
    };
    let note = serde_json::to_string(&admission).map_err(|e| refused(e.to_string()))?;
    let committed =
        runtime
            .admission_boundary()?
            .commit_task_transition(&TaskCoordinationCommitParams {
                task_id: id.into(),
                actor: actor.into(),
                expected_status: vec![task.status],
                status: Some(TaskStatus::Backlog),
                status_event: Some("held_recovery_queued".into()),
                status_note: Some(
                    "operator admitted one prospective recovery; completion authority unchanged"
                        .into(),
                ),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: HELD_RECOVERY_ADMITTED.into(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            })?;
    if !matches!(committed, TaskCoordinationCommitOutcome::Committed(_)) {
        return Err(refused("task or reservation changed during admission"));
    }
    Ok(json!({"id":id,"admission":admission,"replayed":false,"queued":true}))
}
