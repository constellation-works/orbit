//! Evidence a workspace rule owes for a claimed leaf's candidate, derived by
//! Orbit rather than read from the reviewer's report.
//!
//! A `[[review.host_evidence]]` rule names a check, the paths that owe it and
//! the OS it runs on. When a claimed leaf's candidate changes a matching path
//! on a host that cannot run the check in its agent lane — a `codeql` rule on
//! a host of another OS, a `host_sandbox_test` rule on a host of its own OS —
//! the requirement is owed:
//!
//! - admission hands it to the reviewer in the manifest, as owner-fulfilled
//!   evidence it must record `not_run` and never attempt;
//! - settlement adds it to the reviewer's named evidence whatever the report
//!   says, so a reviewer that omitted it, or claimed a pass for a check its
//!   host cannot run, never ships the candidate unverified;
//! - a verdict whose only gaps are owed checks settles into the evidence
//!   hold, which the host or the owner fulfils; any other gap, or an open
//!   finding, blocks as before.
//!
//! Once every owed check has arrived for the held candidate, the task's next
//! run that rebuilds the same tree on the same base settles the held review
//! without a reviewer ([`owed_hold_received`]): its findings, records and
//! verdict are the held certificate's, and the arrived evidence passes it.
//!
//! [ORB-15630] A claimed before-landing review on a host of a known OS other
//! than Linux, such as macOS, never holds for Linux CodeQL. Its candidate is
//! already published: the pull request's hosted CI runs CodeQL on it, and
//! the owner's landing decision reads that run. Admission and settlement record the requirement
//! as owner-owed instead ([`split_owner_owed`], [`Judgement::defer_to_owner`]):
//! the reviewer neither attempts nor names it, a CodeQL record it left
//! short of a pass is excluded in its favour, and the certificate names it in
//! `owner_owed_evidence`. A failed CodeQL record still blocks. A before-PR
//! review keeps the hold: nothing hosted runs before the pull request opens.

use orbit_common::OrbitError;
use orbit_engine::review_gate::{CandidateIdentity, committed_paths};
use orbit_types::task::HostOs;
use orbit_types::workflow::{
    CommitIdentity, EvidenceHostOs, REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewEvidenceHold,
    ReviewEvidenceKind, ReviewEvidenceRequirement, ReviewValidation, ReviewVerdict,
    ValidationOutcome, ValidationRole, owed_requirements,
};

use crate::OrbitRuntime;

use super::context::GateContext;
use super::judgement::Judgement;

/// What the run's captured host-evidence rules owe for `commits` on this
/// host. Only a claimed leaf owes evidence: a local run's host is the owner.
pub(super) fn owed_evidence<'a>(
    runtime: &OrbitRuntime,
    context: &GateContext,
    commits: impl IntoIterator<Item = &'a CommitIdentity>,
) -> Result<Vec<ReviewEvidenceRequirement>, OrbitError> {
    let rules = match &context.admission {
        Some(admission) if context.claimed && !admission.host_evidence.is_empty() => {
            &admission.host_evidence
        }
        _ => return Ok(Vec::new()),
    };
    let mut changed = Vec::new();
    for commit in commits {
        changed.extend(committed_paths(&context.workspace_path, &commit.commit)?);
    }
    Ok(owed_requirements(
        rules,
        &changed,
        EvidenceHostOs::of_host(runtime.host_os()),
    ))
}

/// Whether this review's host leaves Linux CodeQL to the owner and the pull
/// request's hosted CI: a claimed before-landing review on a host of a
/// known OS other than Linux.
pub(super) fn defers_linux_codeql(runtime: &OrbitRuntime, context: &GateContext) -> bool {
    context.claimed
        && context
            .admission
            .as_ref()
            .is_some_and(|admission| admission.gates_landing())
        && runtime.host_os().is_some_and(|os| os != HostOs::Linux)
}

/// Split `owed` into what holds the review and what the owner owes instead:
/// with `defers`, every Linux CodeQL requirement is the owner's.
pub(super) fn split_owner_owed(
    owed: Vec<ReviewEvidenceRequirement>,
    defers: bool,
) -> (
    Vec<ReviewEvidenceRequirement>,
    Vec<ReviewEvidenceRequirement>,
) {
    if !defers {
        return (owed, Vec::new());
    }
    owed.into_iter()
        .partition(|required| !linux_codeql(required))
}

/// A CodeQL requirement that runs on Linux, or on no named OS.
fn linux_codeql(required: &ReviewEvidenceRequirement) -> bool {
    required.kind == ReviewEvidenceKind::CodeQl
        && required.os.is_none_or(|os| os == EvidenceHostOs::Linux)
}

/// The admission decision of a run that settles a held review without a
/// reviewer.
pub(super) const EVIDENCE_RECEIVED_DECISION: &str = "evidence_received";

/// The held review `candidate` settles without a reviewer: the task's hold
/// names only checks its certificate recorded as owed, every one of them has
/// arrived for the held tree, and `candidate` is that tree on the held base
/// under the same task meaning. `None` sends the candidate to a reviewer.
pub(super) fn owed_hold_received(
    runtime: &OrbitRuntime,
    context: &GateContext,
    candidate: &CandidateIdentity,
) -> Result<Option<(ReviewEvidenceHold, ReviewCertificate)>, OrbitError> {
    let [task_id] = context.task_ids.as_slice() else {
        return Ok(None);
    };
    let Some(hold) = super::super::evidence::evidence_hold(runtime, task_id)? else {
        return Ok(None);
    };
    let Some(certificate) = runtime
        .get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)?
        .and_then(|artifact| serde_json::from_slice::<ReviewCertificate>(&artifact.content).ok())
    else {
        return Ok(None);
    };
    let owed = |required: &ReviewEvidenceRequirement| {
        certificate.owed_evidence.iter().any(|owed| {
            owed.kind == required.kind
                && owed.command == required.command
                && owed.artifact == required.artifact
        })
    };
    let received = certificate.attempt_id == hold.attempt_id
        && certificate.final_candidate == hold.candidate
        && certificate.task_meaning_digest == hold.task_meaning_digest
        && hold.task_meaning_digest == context.task_digests.1
        && !hold.requirements.is_empty()
        && hold.requirements.iter().all(owed)
        && certificate.repair_commits.is_empty()
        && candidate.head.tree == hold.candidate.tree
        && candidate.base.tree == certificate.base.tree
        && super::super::evidence::evidence_only(&certificate, &hold.requirements)
        && super::super::evidence::evidence_ready(runtime, task_id, &hold)?;
    Ok(received.then_some((hold, certificate)))
}

impl Judgement {
    /// Hold the verdict to the evidence `owed` names. Each owed requirement
    /// replaces any the reviewer named for the same check, and its required
    /// record is `not_run` until the evidence arrives: a reviewer record of
    /// the check is reset, one naming the same program with another command
    /// is excluded in its favour, and a missing one is added. A passing
    /// verdict that has not seen the evidence becomes `incomplete`.
    pub(super) fn require_owed_evidence(&mut self, owed: &[ReviewEvidenceRequirement]) {
        if owed.is_empty() {
            return;
        }
        let host_note = |required: &ReviewEvidenceRequirement| {
            format!(
                "owed by the workspace host-evidence rule `{}`: Orbit runs it on {} for this \
                 candidate",
                required.name,
                required.os.map_or("its host", EvidenceHostOs::as_str)
            )
        };
        for required in owed {
            self.external_evidence.retain(|named| {
                !(named.artifact == required.artifact
                    || (named.kind == required.kind
                        && (named.command == required.command
                            || named.kind == ReviewEvidenceKind::CodeQl)))
            });
            self.external_evidence.push(required.clone());
            let mut recorded = false;
            for record in &mut self.validation {
                if record.role != ValidationRole::Required {
                    continue;
                }
                if record.command.trim() == required.command {
                    recorded = true;
                    record.outcome = ValidationOutcome::NotRun;
                    append_note(record, &host_note(required));
                } else if required.kind == ReviewEvidenceKind::CodeQl
                    && program(&record.command) == program(&required.command)
                {
                    record.role = ValidationRole::Excluded;
                    record.outcome = ValidationOutcome::NotRun;
                    append_note(
                        record,
                        &format!(
                            "superseded by `{}`, {}",
                            required.command,
                            host_note(required)
                        ),
                    );
                }
            }
            if !recorded {
                self.validation.push(ReviewValidation {
                    id: None,
                    command: required.command.clone(),
                    outcome: ValidationOutcome::NotRun,
                    role: ValidationRole::Required,
                    note: Some(host_note(required)),
                    check: None,
                    control: None,
                    sources: Vec::new(),
                    mutation_target: Vec::new(),
                    deferred: Vec::new(),
                    baseline: None,
                });
            }
        }
        if self.verdict.passed() {
            self.verdict = ReviewVerdict::Incomplete;
            self.validation_complete = false;
            self.escalate(&format!(
                "owed_evidence: {} not yet run on the host that owes it",
                owed.iter()
                    .map(|required| format!("`{}`", required.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
}

impl Judgement {
    /// [ORB-15630] Record `owner_owed` — and any Linux CodeQL the reviewer
    /// named itself — as owed to the owner and the pull request's hosted CI
    /// when `defers`. Each requirement leaves the named evidence; a record of
    /// the check that did not fail is excluded in its favour, and a missing
    /// one is added excluded, so the certificate says why it never ran. A
    /// failed record stays and keeps blocking.
    pub(super) fn defer_to_owner(
        &mut self,
        owner_owed: &[ReviewEvidenceRequirement],
        defers: bool,
    ) {
        if !defers {
            return;
        }
        let mut deferred = owner_owed.to_vec();
        for named in self
            .external_evidence
            .extract_if(.., |named| linux_codeql(named))
        {
            if !deferred.iter().any(|owed| owed.kind == named.kind) {
                deferred.push(named);
            }
        }
        for required in &deferred {
            let note = format!(
                "owed to the owner and the pull request's hosted CI (`CodeQL / Analyze (rust)`): \
                 `{}` runs on Linux, not on this review's host, and does not hold a \
                 before-landing review",
                required.name
            );
            let mut recorded = false;
            for record in &mut self.validation {
                // Another invocation of the same CodeQL program is the same
                // check; a shared runner such as `make` is not.
                let same = record.command.trim() == required.command.trim()
                    || (program(&required.command).contains("codeql")
                        && program(&record.command) == program(&required.command));
                if !same || record.role == ValidationRole::ExpectedFailure {
                    continue;
                }
                recorded = true;
                if record.outcome == ValidationOutcome::Failed {
                    continue;
                }
                record.role = ValidationRole::Excluded;
                record.outcome = ValidationOutcome::NotRun;
                record.deferred.clear();
                append_note(record, &note);
            }
            if !recorded {
                self.validation.push(ReviewValidation {
                    id: None,
                    command: required.command.clone(),
                    outcome: ValidationOutcome::NotRun,
                    role: ValidationRole::Excluded,
                    note: Some(note),
                    check: None,
                    control: None,
                    sources: Vec::new(),
                    mutation_target: Vec::new(),
                    deferred: Vec::new(),
                    baseline: None,
                });
            }
        }
        self.host_decided |= !deferred.is_empty();
        self.owner_owed = deferred;
    }
}

/// The program a command runs: its first word.
fn program(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or_default()
}

fn append_note(record: &mut ReviewValidation, note: &str) {
    record.note = Some(match record.note.take() {
        Some(previous) if !previous.trim().is_empty() => format!("{previous}; {note}"),
        _ => note.to_string(),
    });
}
