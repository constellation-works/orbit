use chrono::{DateTime, Utc};
use orbit_common::{OrbitError, governance::attention::cursor};
use orbit_types::{
    task::{DecisionRequest, Task, TaskComment, TaskHistoryEntry, TaskStatus},
    tool::{
        AttentionHost, AttentionItem, AttentionKind, AttentionNext, AttentionSeverity,
        AttentionWorkspace,
    },
    workflow::{REVIEW_GATE_ARTIFACT, ReviewAttemptState, ReviewCertificate},
};
use serde_json::{Value, json};

use super::item;
use crate::{
    OrbitRuntime,
    application::task::{
        TriageCandidacy,
        pilot::{self, PilotAdmissionHold},
    },
};

pub(in super::super) struct Subject<'a> {
    pub(in super::super) runtime: &'a OrbitRuntime,
    pub(in super::super) task: &'a Task,
    pub(in super::super) workspace: &'a AttentionWorkspace,
    pub(in super::super) host: &'a AttentionHost,
    pub(in super::super) now: DateTime<Utc>,
}
impl Subject<'_> {
    pub(in super::super) fn item(
        &self,
        kind: AttentionKind,
        since: DateTime<Utc>,
        reason: &str,
        detail: Value,
        next: AttentionNext,
    ) -> AttentionItem {
        self.item_for(kind, &json!(self.task.id), since, reason, detail, next)
    }
    /// Like [`Self::item`], with an explicit identity `subject` (a decision
    /// request id) in place of the task id.
    pub(super) fn item_for(
        &self,
        kind: AttentionKind,
        subject: &Value,
        since: DateTime<Utc>,
        reason: &str,
        detail: Value,
        next: AttentionNext,
    ) -> AttentionItem {
        AttentionItem {
            id: cursor::attention_item_id(kind, &self.host.machine_id, &self.workspace.id, subject),
            kind,
            severity: AttentionSeverity::Medium,
            workspace: self.workspace.clone(),
            host: self.host.clone(),
            refs: json!({"task_id": self.task.id}),
            task: Some(item::attention_task(self.task)),
            since: Some(since),
            age_seconds: Some((self.now - since).num_seconds().max(0) as u64),
            expires_at: None,
            reason: item::attention_reason(reason),
            detail,
            next,
            fingerprint: String::new(),
        }
    }
    /// A next action naming a standalone tool (not a family operation).
    /// `tool` must be a callable tool name; family operations go through
    /// [`Self::family_tool_next`] so the stored `tool` is the family a
    /// caller invokes.
    pub(in super::super) fn tool_next(&self, tool: &str, summary: &str) -> AttentionNext {
        let input = json!({"id": self.task.id, "workspace": self.workspace.id});
        AttentionNext {
            summary: summary.into(),
            tool: Some(tool.into()),
            input: Some(input),
            command: format!(
                "orbit --workspace '{}' {} {}",
                self.workspace.id,
                tool.strip_prefix("orbit.")
                    .unwrap_or(tool)
                    .replace('.', " "),
                self.task.id
            ),
            needs: Vec::new(),
            also: Vec::new(),
        }
    }
    /// A next action naming one action of a tool family: `tool` is the
    /// callable family, `input` carries its `action`, and `command` keeps
    /// the CLI verb form (`cli_verb`, such as `task requeue`). Arguments
    /// needing operator judgment stay out of `input`, are named in
    /// `needs`, and appear in `command` through `extra`.
    fn family_tool_next(
        &self,
        family: &str,
        action: &str,
        cli_verb: &str,
        summary: &str,
        needs: &[&str],
        extra: &str,
    ) -> AttentionNext {
        AttentionNext {
            summary: summary.into(),
            tool: Some(family.into()),
            input: Some(
                json!({"action": action, "id": self.task.id, "workspace": self.workspace.id}),
            ),
            command: format!(
                "orbit --workspace '{}' {} {}{extra}",
                self.workspace.id, cli_verb, self.task.id
            ),
            needs: needs.iter().map(|need| (*need).to_string()).collect(),
            also: Vec::new(),
        }
    }
    pub(super) fn show_next(&self) -> AttentionNext {
        self.tool_next("orbit.task.show", "Inspect the task")
    }
}

fn hold_description(hold: &PilotAdmissionHold) -> (&'static str, Option<String>) {
    match hold {
        PilotAdmissionHold::StaleAssessment(reason) => {
            ("pilot_assessment_stale", Some(reason.clone()))
        }
        PilotAdmissionHold::Duplicate => ("pilot_duplicate", None),
        PilotAdmissionHold::AlreadyLanded => ("pilot_already_landed", None),
        PilotAdmissionHold::OperatorValidation(hold) => {
            ("operator_validation", Some(hold.detail()))
        }
        PilotAdmissionHold::HostOperational(hold) => ("host_operational", Some(hold.detail())),
        PilotAdmissionHold::NativeOs(_) => ("native_os", None),
    }
}
pub(super) fn decision_answers(
    subject: &Subject<'_>,
    request: &DecisionRequest,
) -> Vec<AttentionNext> {
    let mut answers = request.answers.iter().collect::<Vec<_>>();
    answers.sort_by_key(|answer| request.recommended.as_deref() != Some(answer.id.as_str()));
    answers
        .into_iter()
        .map(|answer| AttentionNext {
            summary: answer.label.clone(),
            tool: None,
            input: None,
            command: format!(
                "orbit --workspace '{}' decision answer {} --answer '{}'",
                subject.workspace.id,
                request.id,
                answer.id.replace('\'', "'\\''")
            ),
            needs: Vec::new(),
            also: Vec::new(),
        })
        .collect()
}

pub(super) fn derive_subject_kind(
    subject: &Subject<'_>,
    kind: AttentionKind,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
    request: Option<&DecisionRequest>,
    tasks: &[Task],
) -> Result<Option<AttentionItem>, OrbitError> {
    let task = subject.task;
    let runtime = subject.runtime;
    let (item, state, high) = match kind {
        AttentionKind::PilotWarning => {
            let Some(mut held) =
                pilot::held_pilot_warnings(task.status, comments, &task.dependencies(), &|id| {
                    runtime.warning_prerequisite_is_done(id)
                })
            else {
                return Ok(None);
            };
            held.findings
                .sort_by(|a, b| (&a.kind, &a.text).cmp(&(&b.kind, &b.text)));
            let since = comments
                .iter()
                .rev()
                .find(|comment| {
                    comment
                        .receipt()
                        .is_some_and(|receipt| receipt.operation_id == held.operation_id)
                })
                .map_or(task.updated_at, |comment| comment.at);
            let mut next = subject.family_tool_next(
                "orbit.task.recover",
                "repilot",
                "task repilot",
                pilot::PILOT_HOLD_CLEARANCE_SUMMARY,
                &[],
                "",
            );
            next.also.push(subject.show_next());
            // Keep the display bounded without truncating fingerprint material.
            let findings = held.findings.iter().take(32).map(|finding| json!({"kind": finding.kind, "text": item::attention_reason(&finding.text)})).collect::<Vec<_>>();
            let item = subject.item(
                kind,
                since,
                &held.findings[0].text,
                json!({"findings": findings, "findings_truncated": held.findings.len() > findings.len()}),
                next,
            );
            let state = json!({"operation_id": held.operation_id, "findings": held.findings});
            let high = held
                .findings
                .iter()
                .any(|finding| finding.kind == "contract_conflicts");
            (item, state, high)
        }
        AttentionKind::PilotHold => {
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                return Ok(None);
            }
            let Some(hold) =
                runtime.pilot_admission_hold_in(task, comments, &|| Ok(history.to_vec()))?
            else {
                return Ok(None);
            };
            let (classification, reason) = hold_description(&hold);
            let receipt = comments
                .iter()
                .rev()
                .find(|comment| comment.by == "task-pilot" && comment.receipt().is_some());
            let operation_id = receipt.and_then(|comment| {
                comment
                    .receipt()
                    .map(|receipt| receipt.operation_id.to_string())
            });
            let entry = history.iter().rev().find(|entry| {
                operation_id.as_deref().is_some_and(|operation| {
                    entry.operation_id.as_deref() == Some(operation)
                        || entry
                            .note
                            .as_deref()
                            .and_then(|note| serde_json::from_str::<Value>(note).ok())
                            .is_some_and(|note| note["operation_id"].as_str() == Some(operation))
                })
            });
            let since = entry
                .map(|entry| entry.at)
                .or_else(|| receipt.map(|comment| comment.at))
                .or_else(|| request.map(|request| request.raised_at))
                .unwrap_or(task.updated_at);
            let assessment_reason = receipt
                .and_then(|comment| comment.receipt())
                .and_then(|receipt| serde_json::from_str::<Value>(receipt.body).ok())
                .and_then(|audit| {
                    audit["assessment"]["assessment_rationale"]
                        .as_str()
                        .map(str::to_owned)
                });
            let source = reason
                .or(assessment_reason)
                .or_else(|| entry.and_then(|entry| entry.note.clone()))
                .or_else(|| receipt.map(|comment| comment.message.clone()))
                .unwrap_or_else(|| task.title.clone());
            let mut next = if matches!(hold, PilotAdmissionHold::StaleAssessment(_)) {
                subject.family_tool_next(
                    "orbit.task.recover",
                    "repilot",
                    "task repilot",
                    pilot::PILOT_HOLD_CLEARANCE_SUMMARY,
                    &[],
                    "",
                )
            } else {
                subject.show_next()
            };
            next.summary = pilot::PILOT_HOLD_CLEARANCE_SUMMARY.into();
            let state = json!({"hold": classification, "operation_id": operation_id.or_else(|| entry.and_then(|entry| entry.operation_id.clone())), "decision_id": null});
            (
                subject.item(kind, since, &source, json!({"hold": classification}), next),
                state,
                tasks
                    .iter()
                    .any(|dependent| dependent.dependencies().contains(&task.id)),
            )
        }
        AttentionKind::Blocked => {
            if task.status != TaskStatus::Blocked {
                return Ok(None);
            }
            let artifacts = runtime.get_task_artifact_manifest(&task.id)?;
            let TriageCandidacy::Qualifies(episode) = crate::application::task::triage_candidacy(
                task,
                history,
                comments,
                &artifacts,
                subject.now,
            ) else {
                return Ok(None);
            };
            let mut next = subject.family_tool_next(
                "orbit.task.recover",
                "requeue",
                "task requeue",
                "Requeue after resolving the block",
                &["reason"],
                " --reason '<reason>'",
            );
            next.also.push(subject.show_next());
            // Put the leaf error before run metadata so the bounded reason
            // shows what the operator needs to resolve.
            let prerequisite = orbit_store::contracts::ClaimFailedNote::parse(&episode.block_note)
                .filter(|note| {
                    note.failure_class.is_some_and(
                        orbit_types::workflow::ClaimFailureClass::requires_operator_decision,
                    )
                });
            let source = prerequisite
                .as_ref()
                .map(|note| note.reason.as_str())
                .or(episode.escalation.as_deref())
                .unwrap_or(&episode.block_note);
            let mut item = subject.item(
                kind,
                episode.blocked_at,
                source,
                json!({"basis": episode.basis}),
                next,
            );
            if let Some(run) = &episode.run_id {
                item.refs["run_id"] = json!(run);
            }
            let escalation_at = comments
                .iter()
                .rev()
                .find(|comment| Some(&comment.message) == episode.escalation.as_ref())
                .map(|comment| comment.at);
            (
                item,
                json!({"blocked_at": episode.blocked_at, "basis": episode.basis, "escalation_at": escalation_at, "decision_id": null}),
                true,
            )
        }
        AttentionKind::ReviewWaiting => {
            if task.status != TaskStatus::Review {
                return Ok(None);
            }
            if let Some(hold) = runtime.current_review_hold(task, history)? {
                let state = json!({
                    "waiting_on": hold.kind.as_str(),
                    "run_id": hold.run_id,
                    "code": hold.code,
                    "rule": hold.rule,
                    "matched": hold.matched,
                });
                (
                    item::review_hold_attention_item(subject, kind, &hold),
                    state,
                    false,
                )
            } else {
                let Some(artifact) = runtime.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)?
                else {
                    // No certificate yet: the review has not settled (not run, still
                    // running, or in owner landing). A status or approval request alone
                    // cannot prove settlement, so this is not waiting and not a read failure.
                    return Ok(None);
                };
                let certificate: ReviewCertificate = serde_json::from_slice(&artifact.content)
                    .map_err(|error| {
                        OrbitError::Execution(format!("decode review certificate: {error}"))
                    })?;
                let Some(ledger) = runtime
                    .review_store()?
                    .review_ledger(&runtime.workspace_id()?, &certificate.lineage_key)?
                else {
                    return Err(OrbitError::Execution(
                        "review certificate has no lineage ledger; settlement cannot be verified"
                            .into(),
                    ));
                };
                if ledger.attempts.iter().any(|attempt| {
                    matches!(attempt.state, ReviewAttemptState::Open)
                        || attempt.reviewer_running.is_some()
                }) {
                    return Ok(None);
                }
                let Some(latest) = ledger.latest_attempt() else {
                    return Ok(None);
                };
                if latest.attempt_id != certificate.attempt_id
                    || !matches!(latest.state, ReviewAttemptState::Settled { .. })
                {
                    return Ok(None);
                }
                let waiting_on = if !certificate.verdict.passed()
                    && ledger.remaining_at(subject.now).seconds == 0
                {
                    "review_budget"
                } else if certificate.verdict.passed() {
                    "approval"
                } else {
                    return Ok(None);
                };
                let next = if waiting_on == "approval" {
                    AttentionNext {
                        summary: "Approve the settled review".into(),
                        tool: None,
                        input: None,
                        command: format!(
                            "orbit --workspace '{}' task update {} --approve",
                            subject.workspace.id, task.id
                        ),
                        needs: vec![],
                        also: vec![subject.show_next()],
                    }
                } else {
                    AttentionNext {
                        summary: "Reset the exhausted review budget".into(),
                        tool: Some("orbit.task.recover".into()),
                        input: Some(
                            json!({"action": "review_reset", "id": task.id, "workspace": subject.workspace.id}),
                        ),
                        command: format!(
                            "orbit --workspace '{}' task review-reset {} --lineage '<lineage>' --reason '<reason>'",
                            subject.workspace.id, task.id
                        ),
                        needs: vec!["lineage_key".into(), "reason".into()],
                        also: vec![subject.show_next()],
                    }
                };
                let reason = certificate
                    .escalation
                    .as_deref()
                    .unwrap_or_else(|| certificate.verdict.as_str());
                (subject.item(kind, certificate.issued_at, reason, json!({"waiting_on": waiting_on, "lineage": certificate.lineage_key, "review_id": certificate.attempt_id}), next), json!({"waiting_on": waiting_on, "review_id": certificate.attempt_id}), false)
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(item::finalize_attention_item(
        item, state, request, subject, high,
    )))
}
