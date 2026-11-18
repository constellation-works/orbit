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
//!
//! [ORB-25667] The same judgement answers for a reviewer whose envelope
//! declared failure: when its report is a valid incomplete verdict whose only
//! gap is external evidence it names, the engine lets the gate's settlement
//! decide the evidence hold instead of failing the reviewer step.

use orbit_common::OrbitError;
use orbit_engine::ReviewReportCorrectionRequest;
use orbit_engine::review_gate::{candidate_identity_at, revision, uncommitted_paths};
use orbit_types::workflow::{
    REVIEW_MANIFEST_ARTIFACT, ReviewAttemptState, ReviewManifest, ReviewVerdict,
};
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
    let Some(judged) = judge_open_attempt(runtime, request)? else {
        return Ok(None);
    };
    Ok(judged
        .judgement
        .correctable_defect(&judged.scope)
        .map(|defect| defect.reason()))
}

/// [ORB-25667] The external checks the open attempt's report names, when a
/// reviewer that declared failure still left a report settlement can hold on:
/// a readable report for this attempt on the admitted candidate (uncommitted
/// reviewer repairs included), of a single task, with an `incomplete`
/// verdict, no open finding, and records that establish the candidate over
/// the settlement scope once every named check passes. `None` for anything
/// else — no or a mismatched report, a reject, an open finding, a gap other
/// than the named evidence — keeps the reviewer step failed. Settlement
/// still judges the report on its own afterwards; this only decides that it
/// is worth judging.
pub(crate) fn review_report_evidence_hold(
    runtime: &OrbitRuntime,
    request: &ReviewReportCorrectionRequest,
) -> Result<Option<String>, OrbitError> {
    let Some(judged) = judge_open_attempt(runtime, request)? else {
        return Ok(None);
    };
    let judgement = &judged.judgement;
    if judged.task_count != 1
        || judgement.host_refused
        || judgement.verdict != ReviewVerdict::Incomplete
        || judgement.has_open_findings()
    {
        return Ok(None);
    }
    let Some(requirements) =
        super::super::evidence::canonical_requirements(&judgement.external_evidence)
            .filter(|requirements| !requirements.is_empty())
    else {
        return Ok(None);
    };
    if !judgement.evidence_would_complete(&requirements, &judged.scope) {
        return Ok(None);
    }
    Ok(Some(
        requirements
            .iter()
            .map(|required| format!("`{}`", required.command))
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

/// The open attempt's report, judged as settlement would read it, and the
/// scope settlement would judge it over.
struct OpenAttemptJudgement {
    judgement: Judgement,
    scope: Vec<String>,
    task_count: usize,
}

/// `None` when there is no open, unreleased attempt with its manifest to
/// judge, or when the candidate moved under the reviewer.
fn judge_open_attempt(
    runtime: &OrbitRuntime,
    request: &ReviewReportCorrectionRequest,
) -> Result<Option<OpenAttemptJudgement>, OrbitError> {
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
    Ok(Some(OpenAttemptJudgement {
        judgement,
        scope,
        task_count: context.task_ids.len(),
    }))
}
