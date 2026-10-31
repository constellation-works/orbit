//! Operator submission of one explicit escalation triage run.
//!
//! The dashboard's "Triage" action and `orbit run job
//! escalation_triage_pipeline --input 'task_ids=["<id>"]'` (with
//! `--input 'subject="block"'` for a blocked task) are the same submission:
//! this module only adds the eligibility readout an operator surface shows
//! and the refusals it wants *before* a run exists, each typed so the
//! surface can name the reason. Eligibility is the pipeline's own
//! candidacy — [`pilot_warning_candidacy`] for a held pilot assessment and
//! [`triage_candidacy`] for a block episode — plus a live triage run
//! already holding the task and a replica checkout. The pipeline's prepare
//! step repeats the candidacy and holder checks against current state, so
//! a race between the preflight and the run still settles safely there.
//! Triage only proposes: the submission itself never approves, promotes,
//! clears a hold or edits the task.

use std::fmt;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRunState, JobRunTrigger};
use serde_json::{Value, json};

use super::PipelineInvokeResult;
use crate::OrbitRuntime;
use crate::application::contracts::ESCALATION_TRIAGE_JOB;
use crate::application::task::{
    PilotWarningCandidacy, TaskAutomationPort, TaskRow, TriageCandidacy, pilot_warning_candidacy,
    triage_candidacy,
};

/// Bound on the live triage runs scanned for one task; non-terminal runs are
/// always among the newest rows.
const TRIAGE_SCAN_LIMIT: usize = 50;

/// Why an explicit escalation triage submission was refused before any run
/// was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationTriageRefusal {
    /// The task is neither `proposed` nor `blocked`, so neither triage
    /// subject reads it.
    UnsupportedStatus {
        /// The refused task.
        task_id: String,
        /// Its current status.
        status: TaskStatus,
    },
    /// The task's assessment or block episode does not currently qualify;
    /// `reason` is the candidacy's stable token and `detail` its words.
    NotEligible {
        /// The refused task.
        task_id: String,
        /// The submission subject the task's status selects.
        subject: &'static str,
        /// Stable candidacy token (for example `triaged`).
        reason: String,
        /// Human-readable candidacy detail.
        detail: String,
    },
    /// A live escalation triage run already holds the task.
    AlreadyTriaging {
        /// The held task.
        task_id: String,
        /// The live run triaging it.
        run_id: String,
    },
    /// This checkout is a replica (follower); the owner machine triages.
    ReplicaCheckout {
        /// The owner machine this checkout replicates.
        owner: String,
    },
}

impl EscalationTriageRefusal {
    /// Stable machine-readable token for the refusal.
    pub fn code(&self) -> String {
        match self {
            Self::UnsupportedStatus { .. } => "task_triage_unsupported_status".to_string(),
            Self::NotEligible { reason, .. } => format!("task_triage_{reason}"),
            Self::AlreadyTriaging { .. } => "task_triage_already_triaging".to_string(),
            Self::ReplicaCheckout { .. } => "task_triage_replica_checkout".to_string(),
        }
    }

    /// The candidacy token behind a `NotEligible` refusal, when there is one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::NotEligible { reason, .. } => Some(reason),
            Self::AlreadyTriaging { .. } => Some("already_triaging"),
            Self::ReplicaCheckout { .. } => Some("replica_checkout"),
            Self::UnsupportedStatus { .. } => Some("unsupported_status"),
        }
    }
}

impl fmt::Display for EscalationTriageRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedStatus { task_id, status } => write!(
                f,
                "escalation triage refuses {task_id} (status {status}); only a proposed task \
                 held by pilot warnings or a blocked task may be triaged"
            ),
            Self::NotEligible {
                task_id, detail, ..
            } => write!(
                f,
                "escalation triage refuses {task_id}: {detail}; nothing was submitted"
            ),
            Self::AlreadyTriaging { task_id, run_id } => write!(
                f,
                "task {task_id} is already being triaged by run {run_id}; wait for it to \
                 finish or cancel it"
            ),
            Self::ReplicaCheckout { owner } => write!(
                f,
                "this checkout is a replica of owner machine {owner}; triage on the owner"
            ),
        }
    }
}

/// A failed explicit escalation triage submission.
#[derive(Debug)]
pub enum EscalationTriageSubmitError {
    /// The preflight refused the request; nothing was submitted.
    Refused(EscalationTriageRefusal),
    /// Lookup or submission failed (an unknown task is `NotFound`).
    Runtime(OrbitError),
}

impl fmt::Display for EscalationTriageSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Runtime(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for EscalationTriageSubmitError {}

impl From<OrbitError> for EscalationTriageSubmitError {
    fn from(error: OrbitError) -> Self {
        Self::Runtime(error)
    }
}

/// One task's triage eligibility before the live-run and replica checks.
struct BaseEligibility {
    subject: &'static str,
    available: bool,
    reason: String,
    detail: String,
}

impl OrbitRuntime {
    /// `row`'s escalation triage eligibility as the dashboard detail serves
    /// it, or `None` when no Triage action is shown: the task is neither a
    /// `proposed` task whose latest pilot assessment holds on warnings nor
    /// a `blocked` task. A blocked task always carries the readout, so a
    /// disabled action can name its reason.
    pub fn escalation_triage_eligibility_json(
        &self,
        row: &TaskRow,
    ) -> Result<Option<Value>, OrbitError> {
        let Some(base) = self.base_eligibility(row) else {
            return Ok(None);
        };
        // A live-run scan that cannot be read disables the action with its
        // reason rather than failing the detail read; the endpoint still
        // propagates the same failure on submit.
        let live = if base.available {
            self.live_triage_run_for_task(&row.task.id)
        } else {
            Ok(None)
        };
        let (available, reason, detail, run_id) = if !base.available {
            (false, base.reason, base.detail, None)
        } else if let Err(error) = live {
            (
                false,
                "unavailable".to_string(),
                format!("live triage runs could not be read: {error}"),
                None,
            )
        } else if let Ok(Some(run_id)) = live {
            (
                false,
                "already_triaging".to_string(),
                format!("run {run_id} is already triaging this task"),
                Some(run_id),
            )
        } else if let Some(owner) = self.replica_owner_machine() {
            (
                false,
                "replica_checkout".to_string(),
                format!("this checkout is a replica of owner machine {owner}; triage on the owner"),
                None,
            )
        } else {
            (true, base.reason, base.detail, None)
        };
        Ok(Some(json!({
            "available": available,
            "subject": base.subject,
            "reason": reason,
            "detail": detail,
            "run_id": run_id,
        })))
    }

    /// Submit `escalation_triage_pipeline` in explicit mode for exactly
    /// `task_id`, through the shared catalog submission path, with the
    /// subject its status selects. Refuses before submission when the task
    /// does not currently qualify, a live run is triaging it, or this
    /// checkout is a replica.
    pub fn submit_explicit_escalation_triage(
        &self,
        task_id: &str,
        actor: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, EscalationTriageSubmitError> {
        if let Some(owner) = self.replica_owner_machine() {
            return Err(EscalationTriageSubmitError::Refused(
                EscalationTriageRefusal::ReplicaCheckout {
                    owner: owner.to_string(),
                },
            ));
        }
        let row = self.get_task_row(task_id)?;
        let task = &row.task;
        let Some(base) = self.base_eligibility(&row) else {
            return Err(EscalationTriageSubmitError::Refused(match task.status {
                // A proposed task with no held warnings: the readout hides
                // the action, and the endpoint names the candidacy.
                TaskStatus::Proposed => EscalationTriageRefusal::NotEligible {
                    task_id: task.id.clone(),
                    subject: "pilot_warning",
                    reason: "not_held".to_string(),
                    detail: PilotWarningCandidacy::NotHeld.detail(),
                },
                _ => EscalationTriageRefusal::UnsupportedStatus {
                    task_id: task.id.clone(),
                    status: task.status,
                },
            }));
        };
        if !base.available {
            return Err(EscalationTriageSubmitError::Refused(
                EscalationTriageRefusal::NotEligible {
                    task_id: task.id.clone(),
                    subject: base.subject,
                    reason: base.reason,
                    detail: base.detail,
                },
            ));
        }
        if let Some(run_id) = self.live_triage_run_for_task(&task.id)? {
            return Err(EscalationTriageSubmitError::Refused(
                EscalationTriageRefusal::AlreadyTriaging {
                    task_id: task.id.clone(),
                    run_id,
                },
            ));
        }
        let task_id = task.id.clone();
        Ok(self.submit_catalog_job_run(
            ESCALATION_TRIAGE_JOB,
            json!({ "task_ids": [task_id], "subject": base.subject }),
            actor,
            trigger,
        )?)
    }

    /// The candidacy half of eligibility: `None` when no action is shown.
    fn base_eligibility(&self, row: &TaskRow) -> Option<BaseEligibility> {
        let task = &row.task;
        match task.status {
            TaskStatus::Proposed => {
                let candidacy =
                    pilot_warning_candidacy(task, &row.comments, &row.history, &|id: &str| {
                        matches!(
                            self.get_task(id),
                            Ok(found) if found.status == TaskStatus::Done
                        )
                    });
                match candidacy {
                    PilotWarningCandidacy::NotHeld => None,
                    other => Some(BaseEligibility {
                        subject: "pilot_warning",
                        available: matches!(other, PilotWarningCandidacy::Qualifies(_)),
                        reason: other.as_str().to_string(),
                        detail: other.detail(),
                    }),
                }
            }
            TaskStatus::Blocked => {
                let candidacy = triage_candidacy(
                    task,
                    &row.history,
                    &row.comments,
                    &row.artifacts,
                    Utc::now(),
                );
                Some(BaseEligibility {
                    subject: "block",
                    available: matches!(candidacy, TriageCandidacy::Qualifies(_)),
                    reason: candidacy.as_str().to_string(),
                    detail: candidacy.detail(),
                })
            }
            _ => None,
        }
    }

    /// The live escalation triage run holding `task_id`: one that names it
    /// in its input, or whose prepare checkpoint holds it. Reads the job
    /// store directly, like the holders scan, so a detail read neither
    /// reconciles runs nor depends on the job catalog loading.
    fn live_triage_run_for_task(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        let named = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(ESCALATION_TRIAGE_JOB.to_string()),
            task_id: Some(task_id.to_string()),
            states: vec![
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Retrying,
            ],
            limit: Some(TRIAGE_SCAN_LIMIT),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        if let Some(run) = named.into_iter().next() {
            return Ok(Some(run.run_id));
        }
        let holders = self.escalation_triage_holders()?;
        Ok(holders
            .get(task_id)
            .and_then(|runs| runs.iter().next().cloned()))
    }
}
