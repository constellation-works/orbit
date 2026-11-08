//! List and record pilot-warning triage decisions.

use chrono::{SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    DecisionAction, DecisionAnswer, DecisionBinding, DecisionGate, DecisionOption, DecisionRequest,
    Task, TaskComment, TaskHistoryEntry, TaskStatus, decision_option_answers,
};
use orbit_types::workflow::DrainHoldFinding;
use serde_json::{Value, json};

use super::super::TaskListFilter;
use super::super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::super::helpers::SYSTEM_ACTOR_LABEL;
use super::super::pilot::HeldPilotWarnings;
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;

use super::{
    PILOT_WARNING_TRIAGED_EVENT, PilotWarningCandidacy, PilotWarningDecision,
    PilotWarningTriageRequest,
};

impl OrbitRuntime {
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
    /// under the task write lock: one comment, one decision request and one
    /// [`PILOT_WARNING_TRIAGED_EVENT`] marker, nothing else. Refused unless
    /// the held assessment awaits triage: no marker names it and no operator
    /// has decided it.
    pub fn record_pilot_warning_triage(
        &self,
        request: &PilotWarningTriageRequest,
    ) -> Result<Value, OrbitError> {
        let (disposition, outcome, body, summary, answers, recommended, human_action) =
            match &request.decision {
                PilotWarningDecision::Propose {
                    summary,
                    options,
                    recommended,
                } => {
                    let summary = triage_text(summary, "summary")?;
                    let answers = decision_option_answers(options, *recommended)
                        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
                    let listing = answers
                        .iter()
                        .map(|answer| {
                            format!(
                                "\n\nOption {}: {}\nConsequence: {}",
                                answer.id, answer.label, answer.consequence
                            )
                        })
                        .collect::<String>();
                    (
                        "decision",
                        "proposed",
                        format!(
                            "\nsummary: {summary}{listing}\n\nRecommended: option {recommended}\n\
                             Nothing else on this task was changed: answer the decision request to \
                             apply an option in one step."
                        ),
                        summary,
                        answers,
                        Some(recommended.to_string()),
                        None,
                    )
                }
                PilotWarningDecision::Report {
                    human_action,
                    summary,
                } => {
                    let human_action = triage_text(human_action, "reason")?;
                    let summary = match summary {
                        Some(summary) => triage_text(summary, "summary")?,
                        None => human_action.clone(),
                    };
                    (
                        "human_only",
                        "reported",
                        format!(
                            "\nsummary: {summary}\nhuman_action: {human_action}\nNothing else on \
                             this task was changed."
                        ),
                        summary,
                        vec![DecisionAnswer {
                            risk_tier: Default::default(),
                            id: "done".to_string(),
                            label: "Done — approve into backlog".to_string(),
                            consequence: "Confirms the action was taken; the task is approved \
                                          into the backlog."
                                .to_string(),
                            option: Some(DecisionOption {
                                summary: "Done — approve into backlog".to_string(),
                                rewrite: None,
                                action: Some(DecisionAction::Requeue),
                            }),
                        }],
                        Some("done".to_string()),
                        Some(human_action),
                    )
                }
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
            let mut decision = DecisionRequest::new(
                task.id.clone(),
                DecisionGate::ProposedApproval,
                answers.clone(),
                DecisionBinding::for_task(&task, None),
                "pilot_warning_triage".to_string(),
                Utc::now(),
            )
            .with_summary(summary.clone(), recommended.clone())
            .with_category(&self.workspace_id()?);
            if let Some(action) = &human_action {
                decision = decision.with_human_action(action.clone());
            }
            self.write_pilot_warning_triage(
                &task,
                &held,
                disposition,
                outcome,
                &comment,
                decision,
            )?;
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
        decision: DecisionRequest,
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
                    append_history: vec![
                        marker.clone(),
                        decision.history_entry(SYSTEM_ACTOR_LABEL),
                    ],
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

/// `value` trimmed and redacted, refused when that leaves nothing.
fn triage_text(value: &str, field: &str) -> Result<String, OrbitError> {
    let value = orbit_common::security::redaction::redact_all(value.trim());
    if value.is_empty() {
        Err(OrbitError::InvalidInput(format!(
            "pilot warning triage needs a non-empty `{field}`"
        )))
    } else {
        Ok(value)
    }
}

/// `text` as a Markdown quote, line by line.
fn quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Each holding finding as a quote naming its kind.
fn quoted_findings(findings: &[DrainHoldFinding]) -> String {
    findings
        .iter()
        .map(|finding| quote(&format!("[{}] {}", finding.kind, finding.text)))
        .collect::<Vec<_>>()
        .join("\n")
}
