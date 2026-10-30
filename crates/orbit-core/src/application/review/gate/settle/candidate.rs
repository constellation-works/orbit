use orbit_common::OrbitError;
use orbit_engine::review_gate::committed_paths;
use orbit_types::workflow::{CommitIdentity, ReviewCertificate};

use crate::OrbitRuntime;
use crate::application::review::gate::baseline;
use crate::application::review::gate::context::GateContext;

use super::model::Settled;
use super::{output, publication};

/// Publish a claimed leaf's held candidate on `origin`, where the owner
/// fetches it to run a named check. Best-effort: a failed push leaves the
/// hold intact, and the owner then reports the candidate unreachable instead
/// of fulfilling it unless the durable carry fallback succeeds. Returns the
/// ref it was published to.
pub(super) fn publish_held_candidate(context: &GateContext, commit: &str) -> Option<String> {
    match orbit_engine::review_gate::publish_held_candidate(&context.workspace_path, commit) {
        Ok(target) => {
            tracing::info!(
                target: "orbit.core.review",
                run_id = %context.run_id,
                target = %target,
                commit,
                "published the held candidate for owner evidence"
            );
            Some(target)
        }
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.review",
                run_id = %context.run_id,
                commit,
                "could not publish the held candidate for owner evidence: {error}"
            );
            None
        }
    }
}

/// Evidence holds bypass the failure activity, so a failed evidence-branch
/// push must carry the reviewed tip here, before recording the hold.
pub(super) fn carry_evidence_candidate(
    runtime: &OrbitRuntime,
    context: &GateContext,
    commit: &str,
) -> Result<Option<orbit_types::workflow::ReviewEvidenceCandidateCarry>, OrbitError> {
    let Some(task_id) = context.task_ids.first() else {
        return Ok(None);
    };
    let state = runtime.read_run_state(&context.run_id)?;
    let published_sha = state.as_ref().and_then(|state| {
        ["landing_push", "push"].into_iter().find_map(|step| {
            state
                .pipeline
                .get(step)?
                .get("local_sha")?
                .as_str()
                .map(str::to_string)
        })
    });
    orbit_engine::review_gate::carry_evidence_candidate(
        runtime,
        &context.workspace_path,
        commit,
        task_id,
        &context.run_id,
        published_sha,
    )
    .map(Some)
}

/// What validation sources are judged against: every bundle task's
/// selectors, as widened for reviewer repairs, plus a `file:` selector for
/// every path the implementation and repair commits changed. `pending` adds
/// reviewer edits settlement has not committed yet, which it would commit
/// as the repair.
pub(in crate::application::review::gate) fn validation_scope(
    context: &GateContext,
    implementation: &[CommitIdentity],
    repair: Option<&CommitIdentity>,
    pending: &[String],
) -> Result<Vec<String>, OrbitError> {
    let mut scope = context
        .tasks
        .iter()
        .flat_map(|task| task.context_files.iter().cloned())
        .collect::<Vec<_>>();
    for commit in implementation.iter().chain(repair) {
        scope.extend(
            committed_paths(&context.workspace_path, &commit.commit)?
                .into_iter()
                .map(|path| format!("file:{path}")),
        );
    }
    scope.extend(pending.iter().map(|path| format!("file:{path}")));
    scope.sort();
    scope.dedup();
    Ok(scope)
}

pub(super) fn reconcile_settled(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: ReviewCertificate,
    selection: Option<&orbit_types::workflow::SelectiveReviewReceipt>,
) -> Result<Settled, OrbitError> {
    let attempt_id = certificate.attempt_id.as_str();
    if certificate.verdict.passed() {
        if context.task_digests.1 != certificate.task_meaning_digest {
            return Err(OrbitError::Execution(format!(
                "review_gate_stale: task_meaning_changed: task meaning no longer matches \
                 settled certificate {attempt_id}"
            )));
        }
        let head = orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?;
        if head != certificate.final_candidate {
            return Err(OrbitError::Execution(format!(
                "review_gate_stale: candidate_changed: the worktree head is {} but certificate \
                 {attempt_id} settled {}",
                head.commit, certificate.final_candidate.commit
            )));
        }
    }
    // Settlement may have stopped before every task carried the evidence
    // or its known-red decisions. Both writes reconcile only missing entries.
    output::validate_selection(&certificate, selection)?;
    publication::publish_certificate(runtime, context, &certificate)?;
    if certificate.verdict.passed() {
        baseline::record_known_red(runtime, &context.task_ids, &certificate.known_red)?;
    }
    output::settled_outcome(runtime, context, certificate, selection)
}
