//! Light-review skips, external-evidence holds and completion rebases.

use orbit_common::OrbitError;
use orbit_engine::review_gate::candidate_identity;
use orbit_types::task::SKIP_REVIEW_TAG;
use serde_json::Value;

use super::super::context::GateContext;
use crate::OrbitRuntime;
use crate::application::task::light_delivery::{
    carries, light_delivery_decision, record_light_delivery, review_skip_decisions,
};

/// Why a `skip-review` candidate within the light-delivery paths is not
/// reviewed [ORB-16855].
pub(super) const SKIP_REVIEW_REASON: &str = "skip_review_light_delivery";

/// Judge the checked-out candidate on its pinned base for `skip-review`,
/// record the decision in each task's history, and say whether review is
/// skipped. A run none of whose tasks carry the tag judges nothing.
pub(super) fn light_review_skipped(
    runtime: &OrbitRuntime,
    context: &GateContext,
) -> Result<bool, OrbitError> {
    let gates = runtime.host_validation_gates();
    let any_mapped = context.tasks.iter().any(|task| {
        task.tags.iter().any(|tag| {
            gates
                .review_skip_tags()
                .contains_key(&tag.trim().to_lowercase())
        })
    });
    if !any_mapped
        && !context
            .tasks
            .iter()
            .any(|task| carries(task, SKIP_REVIEW_TAG))
    {
        return Ok(false);
    }
    let base = context.base_sha()?;
    let head = candidate_identity(&context.workspace_path, &base)?
        .head
        .commit;
    let mut skipped = false;
    if let Some(decision) = light_delivery_decision(
        runtime,
        &context.workspace_path,
        &base,
        &head,
        &context.tasks,
        SKIP_REVIEW_TAG,
    )? {
        skipped |= decision.applies();
        record_light_delivery(runtime, &context.task_ids, &decision)?;
    }
    for decision in review_skip_decisions(
        runtime,
        &context.workspace_path,
        &base,
        &head,
        &context.tasks,
    )? {
        skipped |= decision.applies();
        record_light_delivery(runtime, &context.task_ids, &decision)?;
    }
    Ok(skipped)
}

/// Whether a task's held candidate is still waiting for named external
/// evidence: the hold matches its task meaning and tree, and not every
/// requirement has arrived.
pub(super) fn awaiting_external_evidence(
    runtime: &OrbitRuntime,
    context: &GateContext,
) -> Result<bool, OrbitError> {
    for task in &context.tasks {
        if let Some(hold) = super::super::super::evidence::evidence_hold(runtime, &task.id)?
            && hold.task_meaning_digest == context.task_digests.1
            && orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?.tree
                == hold.candidate.tree
            && !super::super::super::evidence::evidence_ready(runtime, &task.id, &hold)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The head a completion step rebased onto a new base and left unpublished
/// for re-review, as that step recorded it for this run.
pub(super) struct CompletionRebase {
    pub(super) head_sha: String,
    pub(super) base_sha: String,
}

/// Whether the completion step `step_id` asked for a re-review. Read from the
/// run's own recorded pipeline rather than a step template: the completion
/// step is skipped on review-only and no-diff routes, and a `when:` may not
/// read a skippable step's output.
pub(super) fn completion_rebase(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    input: &Value,
) -> Result<Option<CompletionRebase>, OrbitError> {
    // `--auto-fix` hands a repaired reviewed head to re-review too, without
    // completion authority.
    let completes = input.get("completion").and_then(Value::as_str) == Some("done")
        || input.get("auto_fix").and_then(Value::as_bool) == Some(true);
    let no_diff = input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !completes || no_diff {
        return Ok(None);
    }
    let state = runtime.stores().jobs().read_run_state(run_id)?;
    let output = state
        .as_ref()
        .and_then(|state| state.pipeline.get(step_id))
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "re-review admission: run '{run_id}' recorded no `{step_id}` checkpoint"
            ))
        })?;
    if output.get("re_review_required").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let sha = |key: &str| {
        output["rebased"]
            .get(key)
            .and_then(Value::as_str)
            .filter(|sha| !sha.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "re-review admission: `{step_id}` asked for a re-review without \
                     rebased.{key}"
                ))
            })
    };
    Ok(Some(CompletionRebase {
        head_sha: sha("head_sha")?,
        base_sha: sha("base_sha")?,
    }))
}
