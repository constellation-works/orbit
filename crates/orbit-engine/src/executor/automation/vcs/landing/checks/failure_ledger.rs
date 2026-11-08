//! [model ranking §5.1] Each failed required check on a candidate head,
//! recorded once with its cause.
//!
//! The landing's own classification ([`CheckFailureClass`]) decides what the
//! landing does; this module derives the cause the model ranking charges
//! ([`CiFailureClass`]) from the same evidence and records it as a
//! `ci_failure_recorded` task-history event:
//!
//! - **infrastructure**: the runner, the network or a declared
//!   `workflow.ci_infrastructure_signatures` entry failed the job (landing
//!   class transient), or the base fails the same check (inherited);
//! - **code**: the failure cites a file the head changed (its log, or the
//!   failure annotations of a check outside a workflow), or the same check
//!   failed again on a rerun of the unchanged head;
//! - **flaky**: the same check later passed on the same head, with no new
//!   commit;
//! - **unknown**: none of these was established yet.
//!
//! A later observation upgrades an `unknown` record by appending a
//! `ci_failure_reclassified` event naming the old class, the new class and
//! the evidence, so every change is audited in the task's history. Recording
//! is best effort: a refused write never changes what the landing does.

use std::collections::BTreeMap;

use orbit_types::workflow::{
    CI_FAILURE_RECLASSIFIED_EVENT, CI_FAILURE_RECORDED_EVENT, CI_FAILURE_SCHEMA_VERSION,
    CI_RERUN_RESERVED_EVENT, CiFailureClass, CiFailureClassChange, CiFailureLedgerEntry,
    CiFailureReclassification, CiFailureRecord, CiRerunPlan, ci_failure_ledger,
};
use serde_json::Value;

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::operations::CiCommitJobEvidence;
use super::super::super::pr::{CheckState, PullRequestStatus};
use super::recovery::{CheckFailureClass, FailedJob};

/// The recorded failures of one landing's task, read once per attempt and
/// kept current with what this attempt records.
#[derive(Default)]
pub(in crate::executor::automation::vcs) struct FailureLedger {
    entries: Option<Vec<CiFailureLedgerEntry>>,
}

impl FailureLedger {
    fn entries<H: RuntimeHost + ?Sized>(
        &mut self,
        host: &H,
        task: &str,
    ) -> &mut Vec<CiFailureLedgerEntry> {
        self.entries
            .get_or_insert_with(|| match host.get_task_history(task) {
                Ok(history) => ci_failure_ledger(&history),
                Err(error) => {
                    tracing::warn!(task, %error, "CI failure ledger: task history unreadable");
                    Vec::new()
                }
            })
    }

    /// Record each failed job on `head` the ledger does not hold yet, and
    /// upgrade an `unknown` one the evidence now classes.
    pub(super) fn record<H: RuntimeHost + ?Sized>(
        &mut self,
        host: &H,
        context: &HandoffLandingContext,
        head: &str,
        pr: u64,
        jobs: &[FailedJob],
        code: &BTreeMap<u64, String>,
    ) {
        for job in jobs {
            let (class, evidence) = cause(job, code);
            let entries = self.entries(host, &context.task_id);
            match entries.iter().position(|entry| entry.is(head, &job.label)) {
                Some(index) => {
                    let from = entries[index].class;
                    if from.may_become(class) {
                        reclassify(host, context, entries, index, class, evidence);
                    }
                }
                None => {
                    let record = CiFailureRecord {
                        schema_version: CI_FAILURE_SCHEMA_VERSION,
                        task_id: context.task_id.clone(),
                        head_sha: head.to_string(),
                        pr_number: Some(pr),
                        producing_run_id: Some(context.producing_run_id.clone())
                            .filter(|run| !run.is_empty()),
                        check: job.label.clone(),
                        signature: job.signature.clone(),
                        class,
                        landing_class: job.class.as_str().to_string(),
                        evidence,
                    };
                    if write(host, context, CI_FAILURE_RECORDED_EVENT, &record) {
                        entries.push(CiFailureLedgerEntry {
                            recorded_at: chrono::Utc::now(),
                            class,
                            record,
                            changes: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    /// A check recorded `unknown` on this head that now passes on the same
    /// head is flaky: it passed on a rerun with no new commit.
    pub(in crate::executor::automation::vcs) fn observe_passes<H: RuntimeHost + ?Sized>(
        &mut self,
        host: &H,
        context: &HandoffLandingContext,
        status: &PullRequestStatus,
    ) {
        let Some(head) = status.head_sha.as_deref() else {
            return;
        };
        let passed = status
            .status_checks
            .iter()
            .flatten()
            .filter(|check| check.state == Some(CheckState::Passed))
            .filter_map(|check| {
                let name = check.name.as_deref()?;
                Some(match check.workflow_name.as_deref() {
                    Some(workflow) => format!("{workflow} / {name}"),
                    None => name.to_string(),
                })
            })
            .collect::<Vec<_>>();
        if passed.is_empty() {
            return;
        }
        let entries = self.entries(host, &context.task_id);
        for index in 0..entries.len() {
            let entry = &entries[index];
            if entry.class == CiFailureClass::Unknown
                && entry.record.head_sha == head
                && passed.contains(&entry.record.check)
            {
                let evidence = format!(
                    "check '{}' passed on unchanged head {head} after it failed there",
                    entry.record.check
                );
                reclassify(
                    host,
                    context,
                    entries,
                    index,
                    CiFailureClass::Flaky,
                    evidence,
                );
            }
        }
    }
}

/// The ranking cause of one classified job.
fn cause(job: &FailedJob, code: &BTreeMap<u64, String>) -> (CiFailureClass, String) {
    match job.class {
        CheckFailureClass::Transient => (
            CiFailureClass::Infrastructure,
            format!("infrastructure: {}", job.why),
        ),
        CheckFailureClass::Inherited => (
            CiFailureClass::Infrastructure,
            format!("the base fails the same check: {}", job.why),
        ),
        _ => match code.get(&job.job_id) {
            Some(evidence) => (CiFailureClass::Code, evidence.clone()),
            None => (
                CiFailureClass::Unknown,
                format!(
                    "no cause established yet ({}: {})",
                    job.class.as_str(),
                    job.why
                ),
            ),
        },
    }
}

/// Whether `job` is the rerun of a failed set this head already reserved or
/// sent, failing `check` again: the same head failed the same check twice.
pub(super) fn failed_again_on_rerun(
    history: &[orbit_types::task::TaskHistoryEntry],
    head: &str,
    job: &CiCommitJobEvidence,
    check: &str,
) -> bool {
    let (Some(run), Some(attempt)) = (job.run_id.number(), job.run_attempt.number()) else {
        return false;
    };
    history.iter().any(|entry| {
        let Some(note) = entry.note.as_deref() else {
            return false;
        };
        if entry.event == CI_RERUN_RESERVED_EVENT {
            serde_json::from_str::<CiRerunPlan>(note).is_ok_and(|plan| {
                plan.head_sha == head
                    && plan.run_id == run
                    && plan.run_attempt < attempt
                    && plan.roots.iter().any(|root| root.check == check)
            })
        } else if entry.event == "ci_check_rerun" {
            serde_json::from_str::<Value>(note).is_ok_and(|note| {
                note["head"] == head
                    && note["run_id"].as_u64() == Some(run)
                    && note["jobs"].as_array().is_some_and(|jobs| {
                        jobs.iter().any(|rerun| {
                            rerun["check"] == check
                                && rerun["run_attempt"]
                                    .as_u64()
                                    .is_some_and(|rerun_attempt| rerun_attempt < attempt)
                        })
                    })
            })
        } else {
            false
        }
    })
}

/// Whether this head already has a rerun reserved or sent: a second result
/// exists or is on its way, so no classification rerun is owed.
pub(super) fn head_was_rerun(history: &[orbit_types::task::TaskHistoryEntry], head: &str) -> bool {
    history.iter().any(|entry| {
        let Some(note) = entry.note.as_deref() else {
            return false;
        };
        match entry.event.as_str() {
            CI_RERUN_RESERVED_EVENT => {
                serde_json::from_str::<CiRerunPlan>(note).is_ok_and(|plan| plan.head_sha == head)
            }
            "ci_check_rerun" => {
                serde_json::from_str::<Value>(note).is_ok_and(|note| note["head"] == head)
            }
            _ => false,
        }
    })
}

/// Append the audited class change and apply it to the cached entry.
fn reclassify<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    entries: &mut [CiFailureLedgerEntry],
    index: usize,
    to: CiFailureClass,
    evidence: String,
) {
    let entry = &mut entries[index];
    let change = CiFailureReclassification {
        schema_version: CI_FAILURE_SCHEMA_VERSION,
        head_sha: entry.record.head_sha.clone(),
        check: entry.record.check.clone(),
        from: entry.class,
        to,
        evidence,
    };
    if write(host, context, CI_FAILURE_RECLASSIFIED_EVENT, &change) {
        entry.class = to;
        entry.changes.push(CiFailureClassChange {
            at: chrono::Utc::now(),
            from: change.from,
            to,
            evidence: change.evidence,
        });
    }
}

/// Append one ledger event to the landing's task; a refusal is logged and
/// leaves the landing's route unchanged.
fn write<H: RuntimeHost + ?Sized, T: serde::Serialize>(
    host: &H,
    context: &HandoffLandingContext,
    event: &str,
    value: &T,
) -> bool {
    let written = serde_json::to_string(value)
        .map_err(|error| orbit_common::OrbitError::Execution(error.to_string()))
        .and_then(|note| host.record_handoff_landing_history(&context.handoff_id, event, &note));
    match written {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(task = %context.task_id, event, %error, "CI failure ledger: the record was not written");
            false
        }
    }
}
