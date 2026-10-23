//! The owner's required gates, decided by the claim host's own runs
//! [ORB-15630].
//!
//! A claimed leaf runs the owner's required commands (`claim_validate`) on
//! its host, natively and outside any agent sandbox, before it publishes.
//! Its before-landing reviewer then reviews that same published head from
//! inside the agent sandbox, where nested-sandbox tests skip or fail and a
//! loaded host turns timing-sensitive tests red. A reviewer that reran the
//! gates there judged its own sandbox, not the candidate. So the review's
//! admission reads the host's results from the run's validation step and
//! hands them to the reviewer, which neither reruns nor judges those gates;
//! it may still run focused tests for its findings. Settlement decides each
//! gate from the host's result:
//!
//! - a pass counts as the check's required record, replacing whatever the
//!   reviewer recorded for it (a sandbox failure, a skip, a deferral, or
//!   nothing);
//! - a known-red result counts as known red, its record failed;
//! - any other failure is the candidate's own and rejects the review.
//!
//! A review whose only gaps those results decided, with no open finding and
//! no pending evidence, settles `accept` (`accept_with_fixes` over a repair).
//! The results describe the reviewed candidate. A reviewer repair is
//! revalidated by the host before it is pushed (`landing_review_validate`),
//! and a failure there is the review's reject, so a pass still counts for
//! the repaired head; a known-red result binds to its tested commit and so
//! counts only without a repair.

use orbit_automation::review::same_host_command;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    HostGateResult, HostKnownRed, KnownRedValidation, REVIEW_GATE_ARTIFACT, ReviewBaselineClaim,
    ReviewEvidenceKind, ReviewGateAdmitted, ReviewValidation, ReviewVerdict, ValidationOutcome,
    ValidationRole,
};
use serde_json::Value;

use crate::OrbitRuntime;

use super::context::GateContext;
use super::judgement::{Judgement, extend_note};

/// The admission input naming the run's pre-review validation step.
pub(super) const HOST_VALIDATION_STEP_INPUT: &str = "host_validation_step";

/// The claim host's results for the owner's `trusted` commands on `head`,
/// as the run's `step` recorded them. Empty when the run is not a claimed
/// leaf, the step recorded nothing, or it validated another commit.
pub(super) fn recorded_host_gates(
    runtime: &OrbitRuntime,
    context: &GateContext,
    step: &str,
    head: &str,
    trusted: &[String],
) -> Result<Vec<HostGateResult>, OrbitError> {
    if !context.claimed {
        return Ok(Vec::new());
    }
    let state = runtime.stores().jobs().read_run_state(&context.run_id)?;
    let Some(output) = state.as_ref().and_then(|state| state.pipeline.get(step)) else {
        return Ok(Vec::new());
    };
    Ok(host_gates_from_output(
        output,
        &context.run_id,
        step,
        head,
        trusted,
    ))
}

/// The results a `claim_validate` output records for `head`, restricted to
/// `trusted` commands.
fn host_gates_from_output(
    output: &Value,
    run_id: &str,
    step: &str,
    head: &str,
    trusted: &[String],
) -> Vec<HostGateResult> {
    if output.get("tested_head").and_then(Value::as_str) != Some(head) {
        return Vec::new();
    }
    let known_red = output
        .get("known_red")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<KnownRedValidation>>(value).ok())
        .unwrap_or_default();
    let results = output
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    results
        .iter()
        .filter_map(|result| {
            let command = result.get("command").and_then(Value::as_str)?.trim();
            if !trusted.iter().any(|trusted| trusted.trim() == command) {
                return None;
            }
            // Outputs recorded before known red applied to claims carried
            // only passed commands, without an exit status.
            let exit_code = result
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(0);
            let known_red = known_red
                .iter()
                .find(|known| known.command.trim() == command && known.tested_head == head)
                .cloned();
            Some(HostGateResult {
                command: command.to_string(),
                outcome: if exit_code == 0 && known_red.is_none() {
                    ValidationOutcome::Passed
                } else {
                    ValidationOutcome::Failed
                },
                exit_code,
                tested_head: head.to_string(),
                run_id: run_id.to_string(),
                step: step.to_string(),
                known_red,
            })
        })
        .collect()
}

/// The host results an admission carries for `reviewed`, the candidate the
/// attempt admitted.
pub(super) fn admitted_host_gates(
    admitted: &ReviewGateAdmitted,
    reviewed: &str,
) -> Vec<HostGateResult> {
    admitted
        .host_validation
        .iter()
        .filter(|gate| gate.tested_head == reviewed)
        .cloned()
        .collect()
}

impl Judgement {
    /// Decide every check `gates` ran from the host's result rather than the
    /// reviewer's record. `base` is the pinned base commit and `repaired`
    /// whether the reviewer committed a repair over the tested candidate.
    pub(super) fn adopt_host_gates(
        &mut self,
        gates: &[HostGateResult],
        base: &str,
        repaired: bool,
    ) {
        let trusted = self
            .required_validation_commands
            .iter()
            .flatten()
            .chain(&self.baseline_commands)
            .map(|command| command.trim().to_string())
            .collect::<Vec<_>>();
        for gate in gates {
            if !trusted.iter().any(|trusted| trusted == gate.command.trim()) {
                continue;
            }
            let known_red = gate
                .known_red
                .as_ref()
                .filter(|known| known.base_sha == base && !known.failures.identities.is_empty());
            if gate.outcome == ValidationOutcome::Failed && gate.known_red.is_some() {
                // Known red binds to the commit it was verified on: a repair
                // is a new commit, and a mismatched or empty report proves
                // nothing. The reviewer's records stand.
                if repaired || known_red.is_none() {
                    continue;
                }
            }
            self.decide_host_gate(gate, known_red);
        }
    }

    fn decide_host_gate(&mut self, gate: &HostGateResult, known_red: Option<&KnownRedValidation>) {
        let baseline = known_red.map(|known| ReviewBaselineClaim {
            base_commit: known.base_sha.clone(),
            outcome: ValidationOutcome::Failed,
            failures: known.failures.identities.clone(),
        });
        let verdict = match (gate.outcome, known_red) {
            (ValidationOutcome::Passed, _) => format!("passed (exit {})", gate.exit_code),
            (_, Some(known)) => format!(
                "failed only tests base {} fails too ({})",
                known.base_sha,
                known.failure_lines().join(", ")
            ),
            _ => format!("failed (exit {})", gate.exit_code),
        };
        let mut decided = false;
        for record in &mut self.validation {
            let decidable = matches!(
                record.role,
                ValidationRole::Required | ValidationRole::Diagnostic | ValidationRole::Excluded
            );
            if !decidable || !same_host_command(record, &gate.command) {
                continue;
            }
            let reviewer = format!("{} {}", record.role.as_str(), record.outcome.as_str());
            if known_red.is_some()
                || record.executed_outcome() != gate.outcome
                || record.role != ValidationRole::Required
            {
                self.host_decided = true;
            }
            record.role = ValidationRole::Required;
            record.outcome = gate.outcome;
            record.deferred.clear();
            record.baseline = baseline.clone();
            extend_note(
                &mut record.note,
                format!(
                    "{}; the reviewer recorded it {reviewer}",
                    host_note(gate, &verdict)
                ),
            );
            decided = true;
            if let Some(known) = known_red {
                self.known_red.push(HostKnownRed {
                    record_id: record.record_id().map(str::to_string),
                    validation: known.clone(),
                    evidence_artifact: REVIEW_GATE_ARTIFACT.to_string(),
                });
            }
        }
        if !decided {
            self.host_decided = true;
            self.validation.push(ReviewValidation {
                id: None,
                command: gate.command.clone(),
                outcome: gate.outcome,
                role: ValidationRole::Required,
                note: Some(host_note(gate, &verdict)),
                check: None,
                control: None,
                sources: Vec::new(),
                mutation_target: Vec::new(),
                repaired_blob: Default::default(),
                deferred: Vec::new(),
                baseline,
            });
            if let Some(known) = known_red {
                self.known_red.push(HostKnownRed {
                    record_id: None,
                    validation: known.clone(),
                    evidence_artifact: REVIEW_GATE_ARTIFACT.to_string(),
                });
            }
        }
        if gate.outcome == ValidationOutcome::Passed {
            // The host's native run is the evidence a reviewer would have
            // named for the gate it could not run in its sandbox.
            let probe = |command: &str| ReviewValidation {
                id: None,
                command: command.to_string(),
                outcome: ValidationOutcome::NotRun,
                role: ValidationRole::Required,
                note: None,
                check: None,
                control: None,
                sources: Vec::new(),
                mutation_target: Vec::new(),
                repaired_blob: Default::default(),
                deferred: Vec::new(),
                baseline: None,
            };
            self.external_evidence.retain(|named| {
                !(matches!(
                    named.kind,
                    ReviewEvidenceKind::HostSandboxTest | ReviewEvidenceKind::NativeOs
                ) && same_host_command(&probe(&named.command), &gate.command))
            });
        } else if known_red.is_none() {
            self.host_gate_failed = true;
            if !self.host_refused {
                self.verdict = ReviewVerdict::Reject;
            }
            self.validation_complete = false;
            self.escalate(&format!(
                "host_gate_failed: `{}` failed (exit {}) on candidate {} when the claim host ran \
                 it outside the agent sandbox in run {}, step `{}`",
                gate.command, gate.exit_code, gate.tested_head, gate.run_id, gate.step
            ));
        }
        self.host_validation.push(gate.clone());
    }

    /// Whether the host's results decided `record`'s check, so no baseline
    /// claim on it is the reviewer's to make.
    pub(super) fn host_gated(&self, record: &ReviewValidation) -> bool {
        self.host_validation
            .iter()
            .any(|gate| same_host_command(record, &gate.command))
    }
}

fn host_note(gate: &HostGateResult, verdict: &str) -> String {
    format!(
        "Host gate: the claim host ran it outside the agent sandbox on candidate {} in run {}, \
         step `{}`: {verdict}",
        gate.tested_head, gate.run_id, gate.step
    )
}
