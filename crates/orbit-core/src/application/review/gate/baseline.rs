//! A failed required check the pinned base fails the same way [ORB-14434].
//!
//! Delivery validation already tells a red base from a candidate's own
//! failure for the owner's required commands [ORB-14258]. A reviewer also
//! runs checks the workspace requires beyond those, and the integration
//! branch has no merge gates, so one of them can fail only because the base
//! is red. The reviewer may then attach a `baseline` claim to the failed
//! `required` record: the base commit it reran the check on, the outcome
//! there, and the failures both name, with `sources` outside the candidate's
//! scope.
//!
//! Settlement does not take the claim on trust. It reruns the check through
//! [`verify_base_failure`] — on the final candidate, and on the base through
//! the result cache delivery validation shares — and accepts it only when
//! the base fails exactly as the candidate does. Commands run on the host,
//! outside the reviewer's sandbox, so only a command the host already trusts
//! is rerun: a `workflow.required_validation_commands` entry or a
//! `review.baseline_commands` entry, matched by the same identity rule as
//! the host-required checks, and the trusted text runs, never the
//! reviewer's. Both lists are the owner's, captured with the run's admission
//! and recorded on the certificate, so the commands settlement reruns are
//! the commands a failed diagnostic may not name [ORB-14684].
//!
//! - A confirmed claim on every failed required check, with no open finding,
//!   no pending external evidence and every other record consistent, holds
//!   the task under `baseline_red_hold` exactly as a red gate step does: the
//!   verdict stays what the reviewer reported, the certificate records the
//!   holds, and the step fails typed `[baseline_red]` so the failure handoff
//!   keeps the candidate and moves the task to the backlog.
//! - A candidate that fails beyond the base keeps that failure: the verdict
//!   stands and the review blocks as before.
//! - A check the host passes on the final candidate [ORB-15122] was the
//!   reviewer's environment failing, not the candidate: with no open finding
//!   and no pending external evidence, the record counts as passed on the
//!   host's run, the certificate records the override in `host_overrides`,
//!   and a review whose claims all resolve that way settles `accept` (or
//!   `accept_with_fixes` over a repair). A passing run whose summary shows it
//!   executed no counted test is inconclusive instead. An open finding or
//!   pending evidence keeps the refusal below.
//! - A check that fails on the candidate when the host runs it, while the
//!   base passes a comparable run, is the candidate's own failure: the review
//!   settles `reject` under `baseline_refuted`, naming the command.
//! - A claim the host otherwise contradicts or cannot check is refused, and
//!   the review settles `incomplete`.
//! - A claim whose base run is not comparable with the candidate's — it
//!   tested another selection, or passed without executing a counted test
//!   [ORB-15131] — is neither refuted nor confirmed: the review settles
//!   `incomplete` under `baseline_not_comparable`.

use orbit_automation::review::{in_scope, same_host_command, validation_evidence};
use orbit_common::OrbitError;
use orbit_engine::review_gate::{BaseFailureVerdict, verify_base_failure};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    BaselineRedHold, CommitIdentity, HostCandidateOverride, REVIEW_BASELINE_ARTIFACT,
    ReviewBaselineClaim, ReviewValidation, ReviewVerdict, ValidationOutcome, ValidationRole,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::context::GateContext;
use super::judgement::{Judgement, extend_note, write_artifact};

impl Judgement {
    /// Check every baseline claim in the report and return the holds the
    /// review settles under, or none when it settles as before. `repair` is
    /// the reviewer's repair commit, which an accept override must name.
    pub(super) fn verify_baseline_claims(
        &mut self,
        runtime: &OrbitRuntime,
        context: &GateContext,
        base: &SourceRevision,
        base_ref: &str,
        scope: &[String],
        repair: Option<&CommitIdentity>,
    ) -> Result<Vec<BaselineRedHold>, OrbitError> {
        let claimed = self
            .validation
            .iter()
            .filter(|record| record.baseline.is_some())
            .cloned()
            .collect::<Vec<_>>();
        if claimed.is_empty() {
            return Ok(Vec::new());
        }
        let [task_id] = context.task_ids.as_slice() else {
            self.downgrade(
                "baseline_claim_refused: a bundle's review is never held for a red base",
            );
            return Ok(Vec::new());
        };
        let trusted = self
            .required_validation_commands
            .iter()
            .flatten()
            .chain(&self.baseline_commands)
            .cloned()
            .collect::<Vec<_>>();
        let mut holds = Vec::new();
        let mut confirmed = Vec::new();
        let mut overrides = Vec::new();
        let mut checks = Vec::new();
        let mut held = true;
        for record in &claimed {
            let command = match admissible(record, base, scope, &trusted) {
                Ok(command) => command,
                Err(reason) => {
                    self.refuse(record, &reason);
                    checks.push(json!({
                        "record": record.record_id(), "command": record.command,
                        "decision": "refused", "reason": reason,
                    }));
                    held = false;
                    continue;
                }
            };
            let claim = record
                .baseline
                .as_ref()
                .map_or(&[][..], |claim| &claim.failures);
            let check = verify_base_failure(
                runtime,
                &context.workspace_path,
                &base.commit,
                &command,
                claim,
                &context.run_id,
            )?;
            let (decision, detail) = match &check.verdict {
                BaseFailureVerdict::Reproduced { failures } => {
                    holds.push(BaselineRedHold {
                        base_ref: base_ref.to_string(),
                        base_sha: base.commit.clone(),
                        command: command.clone(),
                        run_id: context.run_id.clone(),
                        selection: check.selection.clone(),
                    });
                    confirmed.push(record.clone());
                    ("confirmed", json!({ "failures": failures }))
                }
                BaseFailureVerdict::CandidateAdds { failures } => {
                    self.escalate(&format!(
                        "baseline_exceeded: `{command}` fails on the candidate beyond base {}: {}",
                        base.commit,
                        failures.join(", ")
                    ));
                    held = false;
                    ("exceeded", json!({ "failures": failures }))
                }
                BaseFailureVerdict::CandidatePasses { tests_run } => {
                    match self.override_blocker() {
                        Some(blocker) => {
                            let reason = format!(
                                "`{command}` passes on the final candidate when the host runs \
                                 it, but {blocker}"
                            );
                            self.refuse(record, &reason);
                            held = false;
                            (
                                "refused",
                                json!({ "reason": reason, "tests_run": tests_run }),
                            )
                        }
                        None => {
                            overrides.push((
                                record.clone(),
                                HostCandidateOverride {
                                    command: command.clone(),
                                    record_id: record.record_id().map(str::to_string),
                                    reviewer_outcome: record.outcome,
                                    run_id: context.run_id.clone(),
                                    tests_run: *tests_run,
                                    evidence_artifact: REVIEW_BASELINE_ARTIFACT.to_string(),
                                },
                            ));
                            ("candidate_passed", json!({ "tests_run": tests_run }))
                        }
                    }
                }
                BaseFailureVerdict::BasePasses(reason) => {
                    // A downgraded review stays incomplete; the reason still
                    // names the candidate's own failure.
                    if !self.host_refused {
                        self.verdict = ReviewVerdict::Reject;
                    }
                    self.validation_complete = false;
                    self.escalate(&format!("baseline_refuted: {reason}"));
                    held = false;
                    ("rejected", json!({ "reason": reason }))
                }
                BaseFailureVerdict::NotComparable(reason) => {
                    self.downgrade(&format!(
                        "baseline_not_comparable: `{}`: {reason}",
                        record.command
                    ));
                    held = false;
                    ("not_comparable", json!({ "reason": reason }))
                }
                BaseFailureVerdict::Contradicted(reason)
                | BaseFailureVerdict::Inconclusive(reason) => {
                    self.refuse(record, reason);
                    held = false;
                    ("refused", json!({ "reason": reason }))
                }
            };
            checks.push(json!({
                "record": record.record_id(), "command": command, "base_sha": check.base_sha,
                "decision": decision, "detail": detail,
                "candidate": check.candidate_log, "base": check.base_log,
            }));
        }
        let evidence = json!({
            "schema_version": 1,
            "run_id": context.run_id,
            "base": base,
            "checks": checks,
        });
        write_artifact(
            runtime,
            task_id,
            &context.run_id,
            REVIEW_BASELINE_ARTIFACT,
            &serde_json::to_vec_pretty(&evidence).map_err(|error| {
                OrbitError::Execution(format!("serialize baseline evidence: {error}"))
            })?,
        )?;
        let counted = confirmed
            .iter()
            .chain(overrides.iter().map(|(record, _)| record))
            .cloned()
            .collect::<Vec<_>>();
        if !held || !self.holdable(scope, &counted) {
            return Ok(Vec::new());
        }
        for (overridden, record) in overrides {
            self.override_record(&overridden, &record);
            self.host_overrides.push(record);
        }
        if holds.is_empty() {
            self.accept(repair);
            return Ok(Vec::new());
        }
        self.escalate(&format!(
            "baseline_red: {} fail(s) on base {} exactly as on the candidate; held until the base \
             passes",
            holds
                .iter()
                .map(|hold| format!("`{}`", hold.command))
                .collect::<Vec<_>>()
                .join(", "),
            base.commit
        ));
        Ok(holds)
    }

    /// Whether the confirmed red-base checks, and the checks the host passed
    /// on the candidate, are all that keep the review from passing: nothing
    /// the host refused, no open finding, no pending external evidence, and
    /// every record consistent once they count as passed.
    fn holdable(&mut self, scope: &[String], confirmed: &[ReviewValidation]) -> bool {
        if self.host_refused
            || self.verdict.passed()
            || !self.external_evidence.is_empty()
            || self.has_open_findings()
        {
            return false;
        }
        let records = self
            .validation
            .iter()
            .cloned()
            .map(|mut record| {
                if confirmed.contains(&record) {
                    record.outcome = ValidationOutcome::Passed;
                }
                record
            })
            .collect::<Vec<_>>();
        match validation_evidence(&records, &self.validation_context(scope)) {
            Ok(()) => true,
            Err(defect) => {
                self.escalate(&format!(
                    "baseline_red not held: {} even with the host-verified checks counted",
                    defect.reason()
                ));
                false
            }
        }
    }

    /// Why a check the host passed on the candidate may not stand in for the
    /// reviewer's failed record, if anything: a host run never overrides an
    /// open finding, and evidence still owed elsewhere keeps the verdict open.
    fn override_blocker(&self) -> Option<&'static str> {
        if self.has_open_findings() {
            Some("a finding is still open, so the host's run never accepts the review")
        } else if !self.external_evidence.is_empty() {
            Some("external evidence is still owed, so the host's run cannot settle the review")
        } else {
            None
        }
    }

    /// Count `overridden` as passed on the host's run, noting what replaced
    /// the reviewer's outcome.
    fn override_record(&mut self, overridden: &ReviewValidation, by: &HostCandidateOverride) {
        let tests = by
            .tests_run
            .map(|count| format!(", {count} counted test(s)"))
            .unwrap_or_default();
        let note = format!(
            "Host override: the reviewer recorded `{}` {}; the host passed it on the final \
             candidate in run {}{tests}; log {}",
            by.command,
            by.reviewer_outcome.as_str(),
            by.run_id,
            by.evidence_artifact,
        );
        for record in self
            .validation
            .iter_mut()
            .filter(|record| *record == overridden)
        {
            record.outcome = ValidationOutcome::Passed;
            extend_note(&mut record.note, note.clone());
        }
    }

    fn refuse(&mut self, record: &ReviewValidation, reason: &str) {
        self.downgrade(&format!(
            "baseline_claim_refused: `{}`: {reason}",
            record.command
        ));
    }
}

/// The trusted command a claim may be checked with, or why it may not.
fn admissible(
    record: &ReviewValidation,
    base: &SourceRevision,
    scope: &[String],
    trusted: &[String],
) -> Result<String, String> {
    let Some(ReviewBaselineClaim {
        base_commit,
        outcome,
        ..
    }) = &record.baseline
    else {
        return Err("the record carries no baseline claim".to_string());
    };
    if record.role != ValidationRole::Required || record.outcome != ValidationOutcome::Failed {
        return Err(format!(
            "a baseline claim belongs on a failed required record, not a {} {} one",
            record.outcome.as_str(),
            record.role.as_str()
        ));
    }
    if *outcome != ValidationOutcome::Failed {
        return Err(format!(
            "the claim records the base outcome as {}",
            outcome.as_str()
        ));
    }
    if base_commit.trim() != base.commit {
        return Err(format!(
            "the claim names base {}, but the review pinned base {}",
            base_commit.trim(),
            base.commit
        ));
    }
    let sources = record
        .sources
        .iter()
        .map(|source| source.trim())
        .filter(|source| !source.is_empty())
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return Err("the record names no `sources` for its failures".to_string());
    }
    if scope.is_empty() {
        return Err("the candidate's scope is unknown".to_string());
    }
    if let Some(source) = sources.iter().find(|source| in_scope(source, scope)) {
        return Err(format!(
            "failure source `{source}` is inside the candidate's scope, so the failure is the \
             candidate's own"
        ));
    }
    trusted
        .iter()
        .find(|command| same_host_command(record, command))
        .cloned()
        .ok_or_else(|| {
            "the host reruns only `workflow.required_validation_commands` and \
             `review.baseline_commands`, and this check is neither"
                .to_string()
        })
}

/// The hold a certificate's red-base settlement names, as the step's typed
/// failure text: the first hold, which admission reads to decide when the
/// task may run again.
pub(super) fn baseline_red_refusal(holds: &[BaselineRedHold], attempt_id: &str) -> Option<String> {
    let hold = holds.first()?;
    let commands = holds
        .iter()
        .map(|hold| format!("`{}`", hold.command))
        .collect::<Vec<_>>()
        .join(", ");
    Some(hold.text(&format!(
        "review_gate_baseline_red: attempt {attempt_id} found required check(s) {commands} \
         failing on base {} exactly as on the candidate, and nothing else open. The candidate \
         did not introduce the failure, so it is kept and the task is held until the base \
         passes; a fresh review then judges it.",
        hold.base_sha
    )))
}

/// The JSON a settled red-base outcome records in the gate's audit row.
pub(super) fn audit_outcome(holds: &[BaselineRedHold]) -> Value {
    json!({ "gate": "baseline_red", "holds": holds })
}
