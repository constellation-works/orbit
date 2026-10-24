//! The escalation triage lane: the pilot-warning holds it reads, the one
//! decision it records on each, and the deterministic support action of
//! `escalation_triage_pipeline`.
//!
//! A `proposed` task whose latest applied task-pilot assessment holds
//! promotion on warning findings ([`held_pilot_warnings`], the predicate
//! every promotion authority shares) waits until someone notices it. The
//! seeded `pilot_warning_triage` routine (`kind: pilot_warnings_held`)
//! admits each such assessment once; `prepare_escalation_triage` draws a
//! hard-tier crew for it, and the `triage_escalation` agent records one
//! disposition through [`OrbitRuntime::record_pilot_warning_triage`]:
//!
//! - **propose** (decision): one comment quoting the holding findings, with
//!   two or three options, their consequences, ready-to-apply replacement
//!   text and a recommendation.
//! - **report** (human-only): one comment naming the human action.
//!
//! Each appends one [`PILOT_WARNING_TRIAGED_EVENT`] marker naming the
//! assessment's operation id, which excludes that assessment from the lane
//! and is what member settlement reads. The lane is propose-only: it never
//! approves, promotes, clears a hold or edits any other task field. A
//! material edit re-pilots the task; a new assessment that still holds is a
//! new member.

mod prepare;

pub(crate) use prepare::prepare;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::DrainHoldFinding;
use serde_json::{Value, json};

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::blocked_recovery::{proposal_text, quote, triage_text, validated_proposal};
use super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use super::pilot::{HeldPilotWarnings, held_pilot_warnings};
use super::{TaskListFilter, TriageOption};
use crate::OrbitRuntime;

/// The job the `pilot_warnings_held` trigger submits.
pub const ESCALATION_TRIAGE_JOB: &str = "escalation_triage_pipeline";

/// History event that marks one pilot assessment's warning hold triaged.
pub const PILOT_WARNING_TRIAGED_EVENT: &str = "pilot_warning_triaged";

/// Whether a task's pilot warning hold awaits triage, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PilotWarningCandidacy {
    /// The assessment holds and awaits triage.
    Qualifies(HeldPilotWarnings),
    /// The task is not `proposed`, or its latest assessment holds nothing on
    /// warnings.
    NotHeld,
    /// An operator recorded a task-pilot admission decision on the assessment.
    OperatorDecided {
        /// Who decided.
        by: String,
    },
    /// The lane already recorded a decision on the assessment.
    Triaged {
        /// When it did.
        at: DateTime<Utc>,
    },
}

impl PilotWarningCandidacy {
    /// Stable token for the candidacy.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Qualifies(_) => "qualifies",
            Self::NotHeld => "not_held",
            Self::OperatorDecided { .. } => "operator_decided",
            Self::Triaged { .. } => "triaged",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Qualifies(held) => format!(
                "assessment {} holds on {} warning finding(s)",
                held.operation_id,
                held.findings.len()
            ),
            Self::NotHeld => {
                "the task is not proposed, or its latest task-pilot assessment holds nothing on \
                 warnings"
                    .to_string()
            }
            Self::OperatorDecided { by } => {
                format!("{by} recorded a task-pilot admission decision on the latest assessment")
            }
            Self::Triaged { at } => format!(
                "the latest assessment was already triaged at {}",
                at.to_rfc3339()
            ),
        }
    }
}

/// Classify `task`'s latest pilot assessment for triage. It qualifies while
/// [`held_pilot_warnings`] reports holding findings, no trusted operator
/// decision answers that assessment, and no marker records it triaged.
pub(crate) fn pilot_warning_candidacy(
    task: &Task,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
    is_done: &dyn Fn(&str) -> bool,
) -> PilotWarningCandidacy {
    let Some(held) = held_pilot_warnings(task.status, comments, &task.dependencies(), is_done)
    else {
        return PilotWarningCandidacy::NotHeld;
    };
    if let Some(at) = triaged_at(history, &held.operation_id) {
        return PilotWarningCandidacy::Triaged { at };
    }
    // Only a person decides admission: an agent-authored decision, like an
    // agent's `task-pilot-admission:` comment, is never one.
    if let Some(entry) = history.iter().find(|entry| {
        !entry.by.trim().is_empty()
            && !is_automation_actor(&entry.by)
            && matches!(
                &entry.decision,
                Some(TaskDecision::PilotAdmission { operation_id, .. })
                    if *operation_id == held.operation_id
            )
    }) {
        return PilotWarningCandidacy::OperatorDecided {
            by: entry.by.clone(),
        };
    }
    PilotWarningCandidacy::Qualifies(held)
}

/// When the lane's marker for the assessment `operation_id` was recorded.
pub(crate) fn triaged_at(
    history: &[TaskHistoryEntry],
    operation_id: &str,
) -> Option<DateTime<Utc>> {
    history
        .iter()
        .find(|entry| {
            entry.event == PILOT_WARNING_TRIAGED_EVENT
                && entry.by == SYSTEM_ACTOR_LABEL
                && entry
                    .note
                    .as_deref()
                    .and_then(|note| serde_json::from_str::<Value>(note).ok())
                    .is_some_and(|note| note["operation_id"] == operation_id)
        })
        .map(|entry| entry.at)
}

/// What the lane asks to record on one held assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PilotWarningDecision {
    /// Decision: the operator chooses between options.
    Propose {
        /// The options, two or three.
        options: Vec<TriageOption>,
        /// 1-based index of the recommended option.
        recommended: usize,
    },
    /// Human-only: name the action only a human can take.
    Report {
        /// The specific human action.
        human_action: String,
    },
}

/// A triage decision for one task's held pilot assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotWarningTriageRequest {
    /// The held task.
    pub task_id: String,
    /// The decision.
    pub decision: PilotWarningDecision,
    /// Who asked, named in the comment.
    pub requested_by: String,
}

impl OrbitRuntime {
    /// `task`'s pilot-warning candidacy, from its current comments and history.
    pub(crate) fn pilot_warning_candidacy_of(
        &self,
        task: &Task,
    ) -> Result<PilotWarningCandidacy, OrbitError> {
        let comments = self.get_task_comments(&task.id)?;
        let history = self.get_task_history(&task.id)?;
        Ok(pilot_warning_candidacy(
            task,
            &comments,
            &history,
            &|id: &str| self.task_is_done(id),
        ))
    }

    fn task_is_done(&self, id: &str) -> bool {
        matches!(self.get_task(id), Ok(found) if found.status == TaskStatus::Done)
    }

    /// The lane's candidate list as JSON: every `proposed` task whose latest
    /// assessment holds on warnings, with the holding findings quoted, and
    /// the held ones it excludes with why.
    pub fn pilot_warning_candidates_json(&self) -> Result<Value, OrbitError> {
        let mut envelopes = self
            .task_candidates(
                &TaskListFilter {
                    statuses: Some(vec![TaskStatus::Proposed]),
                    ..Default::default()
                },
                usize::MAX,
            )?
            .items;
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        let mut qualifying = Vec::new();
        let mut excluded = Vec::new();
        for envelope in envelopes {
            let task = self.get_task(&envelope.id)?;
            match self.pilot_warning_candidacy_of(&task)? {
                PilotWarningCandidacy::Qualifies(held) => qualifying.push(json!({
                    "task_id": task.id,
                    "title": task.title,
                    "operation_id": held.operation_id,
                    "pilot_run_id": held.pilot_run_id,
                    "findings": held.findings,
                })),
                PilotWarningCandidacy::NotHeld => {}
                other => excluded.push(json!({
                    "task_id": task.id,
                    "why": other.as_str(),
                    "detail": other.detail(),
                })),
            }
        }
        Ok(json!({
            "subject": "pilot_warning",
            "qualifying": qualifying,
            "excluded": excluded,
        }))
    }

    /// Record the lane's decision on `request.task_id`'s held assessment,
    /// under the task write lock: one comment and one
    /// [`PILOT_WARNING_TRIAGED_EVENT`] marker, nothing else. Refused unless
    /// the held assessment awaits triage: no marker names it and no operator
    /// has decided it.
    pub fn record_pilot_warning_triage(
        &self,
        request: &PilotWarningTriageRequest,
    ) -> Result<Value, OrbitError> {
        let (disposition, outcome, body) = match &request.decision {
            PilotWarningDecision::Propose {
                options,
                recommended,
            } => {
                let options = validated_proposal(options, *recommended)?;
                (
                    "decision",
                    "proposed",
                    format!(
                        "{}\nNothing else on this task was changed: apply the chosen replacement \
                         text with `orbit task update`. The edit re-pilots the task, and an \
                         assessment that still holds is triaged afresh.",
                        proposal_text(&options, *recommended)
                    ),
                )
            }
            PilotWarningDecision::Report { human_action } => (
                "human_only",
                "reported",
                format!(
                    "\nhuman_action: {}\nNothing else on this task was changed.",
                    triage_text(human_action, "reason")?
                ),
            ),
        };
        self.ensure_coordination_task_write_permitted()?;
        let id = request.task_id.as_str();
        let mut answer = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            let held = match self.pilot_warning_candidacy_of(&task)? {
                PilotWarningCandidacy::Qualifies(held) => held,
                other => {
                    return Err(OrbitError::InvalidInput(format!(
                        "{id} has no pilot warning hold awaiting triage: {}; nothing was recorded",
                        other.detail()
                    )));
                }
            };
            let comment = format!(
                "pilot_warning_triage disposition={disposition} outcome={outcome} assessment={} \
                 by={}\nholding findings:\n{}{body}",
                held.operation_id,
                request.requested_by,
                quoted_findings(&held.findings)
            );
            self.write_pilot_warning_triage(&task, &held, disposition, outcome, &comment)?;
            answer = Some(json!({
                "task_id": id,
                "subject": "pilot_warning",
                "operation_id": held.operation_id,
                "disposition": disposition,
                "outcome": outcome,
                "status": task.status,
                "comment": comment,
            }));
            Ok(())
        })?;
        answer.ok_or_else(|| OrbitError::Execution("pilot warning triage recorded nothing".into()))
    }

    fn write_pilot_warning_triage(
        &self,
        task: &Task,
        held: &HeldPilotWarnings,
        disposition: &str,
        outcome: &str,
        comment: &str,
    ) -> Result<(), OrbitError> {
        let now = Utc::now();
        let marker = TaskHistoryEntry {
            at: now,
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: PILOT_WARNING_TRIAGED_EVENT.to_string(),
            note: Some(
                json!({
                    "operation_id": held.operation_id,
                    "pilot_run_id": held.pilot_run_id,
                    "disposition": disposition,
                    "outcome": outcome,
                    "at": now.to_rfc3339_opts(SecondsFormat::Secs, true),
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        };
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                &task.id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    append_comments: vec![TaskComment {
                        at: now,
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        message: comment.to_string(),
                        operation_id: None,
                    }],
                    append_history: vec![marker.clone()],
                    expected_status: Some(vec![TaskStatus::Proposed]),
                    ..Default::default()
                },
            )?;
            Ok((
                updated,
                OrbitEvent::TaskUpdated {
                    id: task.id.clone(),
                },
            ))
        })?;
        Ok(())
    }
}

/// Each holding finding as a quote naming its kind.
fn quoted_findings(findings: &[DrainHoldFinding]) -> String {
    findings
        .iter()
        .map(|finding| quote(&format!("[{}] {}", finding.kind, finding.text)))
        .collect::<Vec<_>>()
        .join("\n")
}
