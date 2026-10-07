//! Hold external checks without accepting a candidate or blocking for repairs.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_automation::review::{
    ValidationContext, combined_task_meaning_digest, task_meaning_digest, validation_evidence,
};
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ArtifactWriter, Task, TaskStatus, canonical_artifact_path, validate_relative_artifact_path,
};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    FindingDisposition, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_EVIDENCE_RECEIVED_EVENT,
    REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, ReviewCertificate, ReviewEvidenceCarried, ReviewEvidenceHold,
    ReviewEvidenceKind, ReviewEvidenceRequirement, ReviewEvidenceRerequestReason,
    ReviewExternalEvidence, ReviewValidation, ValidationOutcome, ValidationRole,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams;

/// Only named, unavailable external checks may hold a review. Failed checks,
/// open defects, malformed roles and dropped obligations remain escalations.
pub(super) fn evidence_only(
    certificate: &ReviewCertificate,
    requirements: &[ReviewEvidenceRequirement],
) -> bool {
    if certificate.verdict.passed()
        || certificate.task_ids.len() != 1
        || requirements.is_empty()
        || certificate
            .findings
            .iter()
            .any(|finding| finding.disposition == FindingDisposition::Open)
    {
        return false;
    }
    let Some(validation) = with_external_checks_passed(&certificate.validation, requirements)
    else {
        return false;
    };
    validation_evidence(
        &validation,
        &ValidationContext {
            scope: &certificate.validation_scope,
            obligations: &certificate.retained_obligations,
            retired: &certificate.retired_validation,
            required_validation_commands: certificate.required_validation_commands.as_deref(),
        },
    )
    .is_ok()
}

/// Check the requirement shape and simulate receipt, without hiding failures
/// or treating an unnamed missing check as an external requirement.
pub(super) fn with_external_checks_passed(
    records: &[ReviewValidation],
    requirements: &[ReviewEvidenceRequirement],
) -> Option<Vec<ReviewValidation>> {
    let mut validation = records.to_vec();
    for required in canonical_requirements(requirements)? {
        let mut matched = false;
        for record in &mut validation {
            if record.command == required.command && record.role == ValidationRole::Required {
                if !matches!(
                    record.outcome,
                    ValidationOutcome::NotRun | ValidationOutcome::Denied
                ) {
                    return None;
                }
                record.outcome = ValidationOutcome::Passed;
                matched = true;
            }
        }
        if !matched {
            return None;
        }
    }
    Some(validation)
}

/// Use the store's key for every evidence locator guard. Keep the review
/// contract's refusal of leading `./`, even though the store strips it.
pub(super) fn evidence_artifact_path(raw: &str) -> Option<String> {
    validate_relative_artifact_path(raw).ok()?;
    let path = canonical_artifact_path(raw).ok()?;
    (!reserved_artifact(&path)).then_some(path)
}

/// Normalize locators before persisting a hold or comparing requirements.
pub(super) fn canonical_requirements(
    requirements: &[ReviewEvidenceRequirement],
) -> Option<Vec<ReviewEvidenceRequirement>> {
    let mut seen = std::collections::BTreeSet::new();
    requirements
        .iter()
        .map(|required| {
            let artifact = evidence_artifact_path(&required.artifact)?;
            // [ORB-14478] A host-run check must name the OS it runs on.
            if required.name.trim().is_empty()
                || required.command.trim().is_empty()
                || (required.kind == ReviewEvidenceKind::HostSandboxTest && required.os.is_none())
                || !seen.insert(artifact.clone())
            {
                return None;
            }
            Some(ReviewEvidenceRequirement {
                artifact,
                ..required.clone()
            })
        })
        .collect()
}

pub(crate) fn evidence_hold(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<ReviewEvidenceHold>, OrbitError> {
    runtime
        .get_task_artifact(task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)?
        .map(|artifact| {
            serde_json::from_slice::<ReviewEvidenceHold>(&artifact.content).map_err(|error| {
                OrbitError::Execution(format!("review evidence hold is unreadable: {error}"))
            })
        })
        .transpose()
}

/// Re-read evidence rather than trusting a tag, task comment or summary.
pub(crate) fn evidence_ready(
    runtime: &OrbitRuntime,
    task_id: &str,
    hold: &ReviewEvidenceHold,
) -> Result<bool, OrbitError> {
    if hold.schema_version != 1 || hold.requirements.is_empty() {
        return Ok(false);
    }
    let Some(requirements) = canonical_requirements(&hold.requirements) else {
        return Ok(false);
    };
    let evidence = satisfied_external_evidence(runtime, task_id, &hold.candidate)?;
    Ok(requirements.iter().all(|required| {
        evidence
            .values()
            .any(|result| result.matches_requirement(required, &hold.candidate))
    }))
}

/// [ORB-14530] Whether `writer` may supply a result of `kind`, or its log.
/// An operator may supply every kind; Orbit's own machinery only CodeQL, the
/// one check owner fulfilment runs. An agent's put, a claimed worker's
/// evidence and an unclassified artifact never count, so the agent whose
/// candidate is held can never satisfy the check that holds it. A claimed
/// leaf's own `host_sandbox_test` runs [ORB-14478] count only in the
/// settlement that ran them, never through a later read of the artifact.
fn accepted_writer(kind: ReviewEvidenceKind, writer: Option<ArtifactWriter>) -> bool {
    match writer {
        Some(ArtifactWriter::Operator) => true,
        Some(ArtifactWriter::System) => kind == ReviewEvidenceKind::CodeQl,
        None => false,
    }
}

/// Collect passing result/log pairs on this tree from accepted writers.
/// Artifact paths and display names may change between reviews; they are
/// locators, not check identity.
pub(super) fn satisfied_external_evidence(
    runtime: &OrbitRuntime,
    task_id: &str,
    candidate: &SourceRevision,
) -> Result<BTreeMap<String, ReviewExternalEvidence>, OrbitError> {
    let writers: BTreeMap<String, Option<ArtifactWriter>> = runtime
        .get_task_artifact_manifest(task_id)?
        .into_iter()
        .map(|file| (file.path, file.writer))
        .collect();
    let mut satisfied = BTreeMap::new();
    for (path, writer) in &writers {
        if reserved_artifact(path) || writer.is_none() {
            continue;
        }
        let Some(artifact) = runtime.get_task_artifact(task_id, path)? else {
            continue;
        };
        let Ok(mut evidence) = serde_json::from_slice::<ReviewExternalEvidence>(&artifact.content)
        else {
            continue;
        };
        let Some(log_artifact) = evidence_artifact_path(&evidence.log_artifact) else {
            continue;
        };
        if evidence.schema_version != 1
            || candidate.tree.is_empty()
            || evidence.candidate.tree != candidate.tree
            || evidence.command.trim().is_empty()
            || evidence.outcome != ValidationOutcome::Passed
            || log_artifact == *path
            || !accepted_writer(evidence.kind, *writer)
            || !writers
                .get(&log_artifact)
                .is_some_and(|log_writer| accepted_writer(evidence.kind, *log_writer))
        {
            continue;
        }
        if runtime
            .get_task_artifact(task_id, &log_artifact)?
            .is_none_or(|log| log.content.is_empty())
        {
            continue;
        }
        evidence.log_artifact = log_artifact;
        satisfied.insert(path.clone(), evidence);
    }
    Ok(satisfied)
}

/// [ORB-14450] Whether evidence checked on the task's last settled candidate
/// counts for `head`, a candidate on `base` with another tree.
pub(super) enum EvidenceCarry {
    /// Nothing to carry: no evidence on an earlier tree, or the same tree.
    None,
    /// The patch is unchanged, so that evidence counts for `head`.
    Carried(ReviewEvidenceCarried),
    /// The evidence does not count for `head` and is requested again.
    Rerequested {
        from_tree: String,
        to_tree: String,
        reason: ReviewEvidenceRerequestReason,
    },
}

impl EvidenceCarry {
    pub(super) fn carried(&self) -> Option<&ReviewEvidenceCarried> {
        match self {
            Self::Carried(carried) => Some(carried),
            Self::None | Self::Rerequested { .. } => None,
        }
    }

    /// The admission output's `evidence_carry`.
    pub(super) fn to_json(&self) -> Value {
        match self {
            Self::None => Value::Null,
            Self::Carried(carried) => json!({"outcome": "carried", "carried": carried}),
            Self::Rerequested {
                from_tree,
                to_tree,
                reason,
            } => json!({
                "outcome": "rerequested",
                "from_tree": from_tree,
                "to_tree": to_tree,
                "reason": reason,
            }),
        }
    }
}

/// Compare `head`'s patch over `base` with the patch of the task's last
/// settled candidate over its base, when passing external evidence exists
/// on that candidate's tree or on the tree it was already carried from. A resumed held candidate on a moved base and a
/// completion-step rebase both land here: the evidence carries only when
/// `git patch-id --stable` of the whole change is unchanged.
pub(super) fn evidence_carry(
    runtime: &OrbitRuntime,
    task_id: &str,
    workspace_path: &Path,
    base: &SourceRevision,
    head: &SourceRevision,
) -> Result<EvidenceCarry, OrbitError> {
    let Some(certificate) = runtime
        .get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)?
        .and_then(|artifact| serde_json::from_slice::<ReviewCertificate>(&artifact.content).ok())
    else {
        return Ok(EvidenceCarry::None);
    };
    let source = &certificate.final_candidate;
    if source.tree.is_empty() || source.tree == head.tree {
        return Ok(EvidenceCarry::None);
    }
    // The evidence was checked on the settled tree itself, or on the tree
    // that settlement already carried it from.
    let on_tree = |tree: &str| {
        let revision = SourceRevision {
            commit: String::new(),
            tree: tree.to_string(),
        };
        satisfied_external_evidence(runtime, task_id, &revision).map(|found| !found.is_empty())
    };
    let evidence_tree = if on_tree(&source.tree)? {
        source.tree.clone()
    } else {
        match certificate
            .evidence_carried
            .as_ref()
            .filter(|carried| carried.to_tree == source.tree)
        {
            Some(carried) if carried.from_tree != head.tree && on_tree(&carried.from_tree)? => {
                carried.from_tree.clone()
            }
            _ => return Ok(EvidenceCarry::None),
        }
    };
    let rerequested = |reason| EvidenceCarry::Rerequested {
        from_tree: evidence_tree.clone(),
        to_tree: head.tree.clone(),
        reason,
    };
    let Ok(from) = orbit_engine::review_gate::patch_id(
        workspace_path,
        &certificate.base.commit,
        &source.commit,
    ) else {
        return Ok(rerequested(
            ReviewEvidenceRerequestReason::SourceUnavailable,
        ));
    };
    let to = orbit_engine::review_gate::patch_id(workspace_path, &base.commit, &head.commit)?;
    Ok(match (from, to) {
        (Some(from), Some(to)) if from == to => EvidenceCarry::Carried(ReviewEvidenceCarried {
            from_tree: evidence_tree.clone(),
            to_tree: head.tree.clone(),
            patch_id: to,
        }),
        _ => rerequested(ReviewEvidenceRerequestReason::PatchChanged),
    })
}

/// [ORB-14450] The passing result/log pairs on the tree `carry` came from,
/// restated for `candidate`. Empty unless `carry` leads to its tree.
pub(super) fn carried_external_evidence(
    runtime: &OrbitRuntime,
    task_id: &str,
    candidate: &SourceRevision,
    carry: Option<&ReviewEvidenceCarried>,
) -> Result<BTreeMap<String, ReviewExternalEvidence>, OrbitError> {
    let Some(carry) = carry.filter(|carry| carry.to_tree == candidate.tree) else {
        return Ok(BTreeMap::new());
    };
    let source = SourceRevision {
        commit: String::new(),
        tree: carry.from_tree.clone(),
    };
    Ok(satisfied_external_evidence(runtime, task_id, &source)?
        .into_iter()
        .map(|(path, mut evidence)| {
            evidence.candidate = candidate.clone();
            (path, evidence)
        })
        .collect())
}

fn reserved_artifact(path: &str) -> bool {
    matches!(
        path,
        REVIEW_EVIDENCE_HOLD_ARTIFACT
            | REVIEW_GATE_ARTIFACT
            | REVIEW_MANIFEST_ARTIFACT
            | REVIEW_REPORT_ARTIFACT
            | REVIEW_REPORT_HISTORY_ARTIFACT
    )
}

/// Whether `hold` is still the in-progress task's latest delivery decision:
/// the task is linked to the held run, its meaning is unchanged, and no later
/// status decision or review superseded the hold. A stale artifact left
/// behind after an operator changed the task or a later review failed is
/// never current.
pub(crate) fn hold_is_current(
    runtime: &OrbitRuntime,
    task: &Task,
    hold: &ReviewEvidenceHold,
) -> Result<bool, OrbitError> {
    if task.status != TaskStatus::InProgress
        || hold.schema_version != 1
        || task.job_run_id.as_deref() != Some(&hold.run_id)
        || combined_task_meaning_digest(&[(
            task.id.to_string(),
            task_meaning_digest(task).map_err(super::automation_error)?,
        )])
        .map_err(super::automation_error)?
            != hold.task_meaning_digest
    {
        return Ok(false);
    }
    let history = runtime.get_task_history(&task.id)?;
    if history.last().is_some_and(|entry| {
        matches!(
            entry.to_status,
            Some(
                TaskStatus::Blocked
                    | TaskStatus::Done
                    | TaskStatus::Archived
                    | TaskStatus::Rejected
            )
        )
    }) {
        return Ok(false);
    }
    Ok(history
        .iter()
        .rev()
        .find(|entry| entry.to_status.is_some() || entry.event == "review_awaiting_evidence")
        .is_some_and(|entry| entry.event == "review_awaiting_evidence"))
}

/// Called under the task write lock after an artifact update. All requirements
/// must match the held candidate and unchanged task meaning before requeueing.
/// This schedules another review; it never converts incomplete into accept.
pub(crate) fn resume_evidence_hold(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<(), OrbitError> {
    let task = runtime.get_task(task_id)?;
    if task.status != TaskStatus::InProgress {
        return Ok(());
    }
    let Some(hold) = evidence_hold(runtime, task_id)? else {
        return Ok(());
    };
    let Some(artifact) = runtime.get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)? else {
        return Ok(());
    };
    let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
        return Ok(());
    };
    if certificate.attempt_id != hold.attempt_id
        || certificate.final_candidate != hold.candidate
        || certificate.task_meaning_digest != hold.task_meaning_digest
        || !evidence_only(&certificate, &hold.requirements)
        || !hold_is_current(runtime, &task, &hold)?
        || !evidence_ready(runtime, task_id, &hold)?
    {
        return Ok(());
    }
    runtime.with_mutation(|| {
        let updated = runtime.stores().task_records().update(task_id, TaskRecordUpdateParams {
            actor: "system".into(),
            status: Some(TaskStatus::Backlog),
            expected_status: Some(vec![TaskStatus::InProgress]),
            status_event: Some(REVIEW_EVIDENCE_RECEIVED_EVENT.into()),
            status_note: Some(format!("run={}; all named external checks arrived for the held candidate; queued for fresh review.", hold.run_id)),
            ..Default::default()
        })?;
        Ok((updated, OrbitEvent::TaskUpdated { id: task_id.into() }))
    })?;
    Ok(())
}
