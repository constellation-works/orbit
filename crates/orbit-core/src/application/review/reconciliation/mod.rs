//! Review reconciliation of an already-merged foreign delivery head.
//!
//! A follower's handed-off pull request can merge at a head other than the
//! candidate it handed off (a base merge, a manual fix). The candidate's
//! validation and review never carry to that head, and the review gate cannot
//! run on it: the head is already merged and immutable. Reconciliation is the
//! supported way to produce evidence for it.
//!
//! - **Submission** (`orbit task reconcile-review submit`) is operator-only.
//!   It binds the stopped execution (run, host, claim, handoff), the merged
//!   pull request and the task's meaning into a record with its own id, then
//!   admits one run of [`REVIEW_RECONCILIATION_JOB`] carrying the reserved
//!   [`REVIEW_RECONCILIATION_ADMISSION_KEY`]. The original run's identity is
//!   never changed. It also freezes the record's contract: the required
//!   commands (the accepted handoff's captured list, or, when that list was
//!   explicitly empty, the owner's configuration at submission, labelled as
//!   such) and the review crew. Every attempt runs under that contract; a
//!   later configuration edit changes nothing until a new request key.
//! - **The run** re-observes the binding, runs every required validation
//!   command at the merged head (and reproduces each failure at the base),
//!   has a read-only reviewer inspect exactly that head, and settles one
//!   outcome. Findings become a follow-up task; the merged PR is never
//!   rewritten.
//! - **Completion** through the desktop consumer accepts a changed head only
//!   when an accepted reconciliation binds exactly the current observation.
//!
//! Records live in the host review store, separate from review-gate
//! certificates.

mod observe;
mod run;
mod submit;

use orbit_common::OrbitError;
use orbit_common::security::redaction::{redact_all, redact_home_dir};
use orbit_types::workflow::{
    ReconciliationOutcome, ReviewReconciliation, handoff::AcceptedHandoff,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::HandoffPullRequest;

pub(crate) use run::{prepare, settle, validate};
pub(crate) use submit::reconcile_review;

/// Audit command for every reconciliation decision.
const RECONCILIATION_AUDIT: &str = "review.reconciliation";

/// Tag carried by the follow-up task a reconciliation files.
const FOLLOW_UP_TAG_PREFIX: &str = "review-reconciliation:";

/// Redact strings in reconciliation reports while keeping the stored command
/// identity available to the evidence-bound disposition checks.
pub(super) fn redact_reconciliation_report(value: &mut Value) {
    match value {
        Value::String(text) => *text = safe_reconciliation_text(text),
        Value::Array(values) => {
            for value in values {
                redact_reconciliation_report(value);
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                redact_reconciliation_report(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// A display form for command selectors and other reconciliation prose.
pub(super) fn safe_reconciliation_text(text: &str) -> String {
    redact_home_dir(&redact_all(text))
}

/// Why a changed merged head cannot complete its task, or `None` when an
/// accepted reconciliation binds exactly this observation.
pub(crate) fn merged_head_completion_refusal(
    runtime: &OrbitRuntime,
    task: &orbit_types::task::Task,
    accepted: &AcceptedHandoff,
    pull_request: &HandoffPullRequest,
) -> Result<Option<String>, OrbitError> {
    let records = runtime
        .review_store()?
        .review_reconciliations_for_task(&runtime.workspace_id()?, &task.id)?;
    let digest = observe::task_digest(task)?;
    let matching: Vec<&ReviewReconciliation> = records
        .iter()
        .filter(|record| observe::binds(record, &digest, accepted, pull_request))
        .collect();
    if matching.iter().any(|record| record.accepts_completion()) {
        return Ok(None);
    }
    let head = &pull_request.head;
    let lead = format!(
        "pull request #{} merged head {head}, not the candidate {} that run {} handed off; that \
         candidate's validation and review do not carry to a changed head",
        pull_request.number, accepted.handoff.candidate.candidate.commit, accepted.handoff.run_id
    );
    let next = match matching.first() {
        Some(record) => next_step(record, &task.id),
        None => format!(
            "As an operator, reconcile the merged head: `orbit task reconcile-review submit {} \
             --request <key>`, follow it with `orbit task reconcile-review status {}`, and \
             complete once it is accepted",
            task.id, task.id
        ),
    };
    Ok(Some(format!("{lead}. {next}")))
}

/// The exact next step for a reconciliation record.
fn next_step(record: &ReviewReconciliation, task_id: &str) -> String {
    let rid = &record.reconciliation_id;
    match &record.outcome {
        Some(ReconciliationOutcome::Accepted) => {
            "the merged head is reconciled; complete the task from review".to_string()
        }
        Some(ReconciliationOutcome::AcceptedWithDisposition) if record.accepts_completion() => {
            "the merged head is reconciled; complete the task from review".to_string()
        }
        Some(ReconciliationOutcome::AcceptedWithDisposition) => format!(
            "reconciliation {rid} predates verified remediation evidence; inspect the merged \
             head and submit a new request key to establish current evidence"
        ),
        Some(ReconciliationOutcome::AwaitingDisposition { commands }) => format!(
            "reconciliation {rid} is waiting for an operator disposition of baseline failures \
             in {}: once a landed commit remediates each one, run `orbit task reconcile-review \
             accept-baseline {task_id} --reconciliation {rid} --command '<command>' \
             --remediation <commit> --reason '<why>'`",
            commands
                .iter()
                .map(|command| safe_reconciliation_text(command))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(ReconciliationOutcome::Refused { reason, next_step }) => {
            format!("reconciliation {rid} was refused: {reason}. {next_step}")
        }
        None => format!(
            "reconciliation {rid} has not settled; follow it with `orbit task reconcile-review \
             status {task_id} --reconciliation {rid}`, or resubmit its request key \
             `{}` once its run stopped",
            record.request_key
        ),
    }
}
