//! Return a report's shape defect to the reviewer before settlement
//! [ORB-14616].
//!
//! Settlement judges the reviewer's records once, after the reviewer has
//! stopped, so a defect in a record's shape — a missing note, or the file a
//! counterfactual mutated named as its source — used to spend the attempt on
//! bookkeeping. The engine asks here as soon as the reviewer returns. The
//! answer judges the report exactly as settlement would, over the scope
//! settlement would derive, without committing, writing or settling
//! anything; only a defect the reviewer can correct without rerunning a
//! check is returned.

use orbit_common::OrbitError;
use orbit_engine::ReviewReportCorrectionRequest;
use orbit_engine::review_gate::{candidate_identity_at, revision, uncommitted_paths};
use orbit_types::workflow::{REVIEW_MANIFEST_ARTIFACT, ReviewAttemptState, ReviewManifest};
use serde_json::json;

use crate::OrbitRuntime;

use super::context::GateContext;
use super::judgement::Judgement;
use super::settle::validation_scope;

/// The escalation reason settlement would record for the open attempt's
/// report, when the reviewer can still correct it; `None` when the report
/// would settle on its merits, when there is no open attempt to judge, or
/// when the candidate moved under the reviewer (settlement refuses that on
/// its own).
pub(crate) fn review_report_correction(
    runtime: &OrbitRuntime,
    request: &ReviewReportCorrectionRequest,
) -> Result<Option<String>, OrbitError> {
    let context = GateContext::load(
        runtime,
        &json!({
            "job_run_id": request.run_id,
            "completed_task_ids": request.task_ids,
            "workspace_path": request.workspace_path,
        }),
        None,
    )?;
    let Some(ledger) = runtime
        .review_store()?
        .review_ledger(&context.workspace_id, &request.lineage_key)?
    else {
        return Ok(None);
    };
    let Some(attempt) = ledger
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == request.attempt_id)
        .filter(|attempt| {
            attempt.state == ReviewAttemptState::Open && attempt.released_at.is_none()
        })
    else {
        return Ok(None);
    };
    let Some(manifest) = context
        .task_ids
        .first()
        .map(|task_id| runtime.get_task_artifact(task_id, REVIEW_MANIFEST_ARTIFACT))
        .transpose()?
        .flatten()
        .and_then(|artifact| serde_json::from_slice::<ReviewManifest>(&artifact.content).ok())
        .filter(|manifest| manifest.attempt_id == attempt.attempt_id)
    else {
        return Ok(None);
    };
    if revision(&context.workspace_path, "HEAD")?.commit != attempt.candidate.commit {
        return Ok(None);
    }
    let reviewed = candidate_identity_at(
        &context.workspace_path,
        &manifest.base.commit,
        &attempt.candidate.commit,
    )?;
    let judgement = Judgement::from_report(runtime, &context, attempt)?;
    let scope = validation_scope(
        &context,
        &reviewed.commits,
        None,
        &uncommitted_paths(&context.workspace_path)?,
    )?;
    Ok(judgement
        .correctable_defect(&scope)
        .map(|defect| defect.reason()))
}
