//! Backlog holds from applied pilot findings, including assessments made
//! without lifecycle promotion. Decisions are scoped to the latest assessment.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::governed_tool;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{Task, TaskArtifact, TaskComment, TaskHistoryEntry, TaskStatus};
use orbit_types::tool::McpCapability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::positive_validation_tools;

use super::helpers::is_automation_actor;
use crate::OrbitRuntime;

/// A finding in the latest applied assessment that requires an operator decision.
pub(crate) enum PilotAdmissionHold {
    Duplicate,
    AlreadyLanded,
    OperatorValidation(OperatorValidationHold),
}

/// A typed requirement that the implementing Agent capability cannot satisfy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorValidationRequirement {
    pub(crate) criterion: usize,
    pub(crate) tool: String,
}

/// An assessment-scoped hold, persisted with the pilot's atomic audit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorValidationHold {
    material: String,
    pub(crate) requirements: Vec<OperatorValidationRequirement>,
}

impl OperatorValidationHold {
    pub(crate) fn new(task: &Task, requirements: Vec<OperatorValidationRequirement>) -> Self {
        Self {
            material: validation_material(task),
            requirements,
        }
    }

    pub(crate) fn detail(&self) -> String {
        let requirements = self
            .requirements
            .iter()
            .map(|requirement| {
                format!(
                    "criterion {} requires `{}`",
                    requirement.criterion, requirement.tool
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "Operator validation handoff: {requirements}. Re-scope the criterion, or record an operator evaluation or override with evidence using `task-pilot-admission: evaluated`, `clear`, or `approve-anyway` as the comment's first line. A newer assessment supersedes the decision."
        )
    }

    pub(crate) fn history(&self, operation_id: &str) -> TaskHistoryEntry {
        TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: "task-pilot".into(),
            event: "operator_validation_held".into(),
            note: Some(json!({"operation_id": operation_id, "hold": self}).to_string()),
            from_status: None,
            to_status: None,
        }
    }
}

/// Status, priority, attribution, comments and execution evidence are not
/// assessed validation material; editing them must not release a hold.
fn validation_material(task: &Task) -> String {
    sha256_hex(
        json!({
            "title": task.title, "description": task.description,
            "acceptance_criteria": task.acceptance_criteria, "plan": task.plan,
            "context_files": task.context_files, "complexity": task.complexity,
            "tags": task.tags, "required_tools": task.required_tools,
            "relations": task.relations,
        })
        .to_string()
        .as_bytes(),
    )
}

fn decision_header(message: &str) -> bool {
    matches!(
        message.lines().next().unwrap_or_default().trim(),
        "task-pilot-admission: approve-anyway"
            | "task-pilot-admission: clear"
            | "task-pilot-admission: evaluated"
    )
}

fn decision_has_evidence(message: &str) -> bool {
    decision_header(message) && message.lines().skip(1).any(|line| !line.trim().is_empty())
}

impl OrbitRuntime {
    pub(crate) fn pilot_admission_hold(
        &self,
        task_id: &str,
    ) -> Result<Option<PilotAdmissionHold>, OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        let task = self.get_task(task_id)?;
        let material = validation_material(&task);
        let mut legacy_decision = false;
        // Atomic pilot application persists the full assessment after its replay
        // receipt in a task-pilot-authored comment. Read that durable format, not
        // the human-readable history summary or an uncommitted agent result.
        for comment in comments.iter().rev() {
            let mut lines = comment.message.lines();
            let header = lines.next().unwrap_or_default().trim();
            if !comment.by.trim().is_empty()
                && !is_automation_actor(&comment.by)
                && matches!(
                    header,
                    "task-pilot-admission: approve-anyway" | "task-pilot-admission: clear"
                )
            {
                // Preserve the duplicate/landed comment contract. Operator
                // validation additionally requires its trusted resolution event.
                legacy_decision = true;
            }
            if comment.by != "task-pilot" || !header.starts_with("operation_id=") {
                continue;
            }
            let (_, audit) = comment.message.split_once('\n').ok_or_else(|| {
                OrbitError::Execution("pilot receipt is missing its assessment".into())
            })?;
            let audit: Value = serde_json::from_str(audit).map_err(|error| {
                OrbitError::Execution(format!("decode pilot assessment: {error}"))
            })?;
            let assessment = audit.get("assessment").ok_or_else(|| {
                OrbitError::Execution("pilot receipt is missing its assessment".into())
            })?;
            let operation_id = header.strip_prefix("operation_id=").unwrap_or_default();
            let operator_decision = self.get_task_history(task_id)?.iter().any(|entry| {
                if entry.event != "operator_validation_resolved" || is_automation_actor(&entry.by) {
                    return false;
                }
                let Some(record) = entry
                    .note
                    .as_deref()
                    .and_then(|note| serde_json::from_str::<Value>(note).ok())
                else {
                    return false;
                };
                record["operation_id"] == operation_id
                    && record["hold"]["material"] == material
                    && matches!(
                        record["decision"].as_str(),
                        Some(
                            "task-pilot-admission: clear"
                                | "task-pilot-admission: approve-anyway"
                                | "task-pilot-admission: evaluated"
                        )
                    )
                    && record["evidence"]
                        .as_str()
                        .is_some_and(decision_has_evidence)
            });
            if let Some(hold) = audit
                .get("operator_validation_hold")
                .filter(|hold| !hold.is_null())
            {
                let hold: OperatorValidationHold =
                    serde_json::from_value(hold.clone()).map_err(|error| {
                        OrbitError::Execution(format!("decode operator validation hold: {error}"))
                    })?;
                if hold.material == material && !hold.requirements.is_empty() && !operator_decision
                {
                    return Ok(Some(PilotAdmissionHold::OperatorValidation(hold)));
                }
            } else if !operator_decision {
                // Older pilot audits have no material snapshot. They remain
                // current only while no document edit follows their receipt.
                let edited = self.get_task_history(task_id)?.iter().any(|event| {
                    event.at > comment.at && matches!(event.event.as_str(), "updated" | "renamed")
                });
                if !edited {
                    let tools = assessment["validation_tool_warnings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter_map(|warning| {
                            let rest = warning.strip_prefix("acceptance criterion requires `")?;
                            let (tool, rest) = rest.split_once('`')?;
                            (rest.starts_with(", a governed operation reserved for the ")
                                && governed_tool(tool).is_some_and(|operation| {
                                    !operation.allowed.contains(&McpCapability::Agent)
                                }))
                            .then(|| tool.to_string())
                        })
                        .collect::<Vec<_>>();
                    let requirements = positive_validation_tools(&task, &tools)
                        .into_iter()
                        .map(|(criterion, tool)| OperatorValidationRequirement {
                            criterion,
                            tool: tool.into(),
                        })
                        .collect::<Vec<_>>();
                    if !requirements.is_empty() {
                        return Ok(Some(PilotAdmissionHold::OperatorValidation(
                            OperatorValidationHold::new(&task, requirements),
                        )));
                    }
                }
            }
            if legacy_decision {
                return Ok(None);
            }
            for (field, hold) in [
                ("already_landed", PilotAdmissionHold::AlreadyLanded),
                ("duplicate_of", PilotAdmissionHold::Duplicate),
            ] {
                if assessment.get(field).is_some_and(|value| !value.is_null()) {
                    return Ok(Some(hold));
                }
            }
            // A clear latest assessment supersedes any older finding. Unrelated
            // comments and metadata edits do not release an existing hold.
            return Ok(None);
        }
        Ok(None)
    }

    /// Backfill the typed record for a still-current legacy pilot receipt.
    /// New pilot audits already committed this evidence atomically. This is
    /// called only by admission, never by read-only readiness projections.
    pub(crate) fn record_operator_validation_hold(
        &self,
        task_id: &str,
        hold: &OperatorValidationHold,
    ) -> Result<(), OrbitError> {
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let Some(PilotAdmissionHold::OperatorValidation(current)) =
                    self.pilot_admission_hold(task_id)?
                else {
                    return Ok(());
                };
                if current.material != hold.material {
                    return Ok(());
                }
                let comments = self.get_task_comments(task_id)?;
                let operation_id = comments
                    .iter()
                    .rev()
                    .filter(|comment| comment.by == "task-pilot")
                    .find_map(|comment| {
                        comment
                            .message
                            .lines()
                            .next()?
                            .strip_prefix("operation_id=")
                    })
                    .ok_or_else(|| {
                        OrbitError::Execution(
                            "operator validation hold has no pilot receipt".into(),
                        )
                    })?;
                let mut event = hold.history(operation_id);
                if self
                    .get_task_history(task_id)?
                    .iter()
                    .any(|entry| entry.event == event.event && entry.note == event.note)
                {
                    return Ok(());
                }
                event.by = "system".into();
                self.stores().task_records().update(
                    task_id,
                    super::TaskRecordUpdateParams {
                        actor: "system".into(),
                        expected_status: Some(vec![TaskStatus::Backlog]),
                        append_comments: vec![TaskComment {
                            at: event.at,
                            by: event.by.clone(),
                            message: format!(
                                "operator-validation-handoff\n{}",
                                event.note.as_deref().unwrap_or_default()
                            ),
                        }],
                        append_history: vec![event],
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
    }

    pub(crate) fn record_backlog_operator_validation_holds(&self) -> Result<(), OrbitError> {
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)?
        {
            if let Some(PilotAdmissionHold::OperatorValidation(hold)) =
                self.pilot_admission_hold(&task.id)?
            {
                self.record_operator_validation_hold(&task.id, &hold)?;
            }
        }
        Ok(())
    }

    /// Only the trusted human update path calls this. Agent-authored comment
    /// text cannot mint a resolution event or satisfy an operator handoff.
    pub(super) fn operator_validation_resolution(
        &self,
        task: &Task,
        params: &super::TaskUpdateParams,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(PilotAdmissionHold::OperatorValidation(hold)) =
            self.pilot_admission_hold(&task.id)?
        else {
            return Ok(None);
        };
        let rescoped = params
            .acceptance_criteria
            .as_ref()
            .is_some_and(|criteria| criteria != &task.acceptance_criteria);
        let decision = params
            .comment
            .as_deref()
            .filter(|message| decision_header(message));
        if !rescoped && decision.is_none() {
            return Ok(None);
        }
        if let Some(message) = decision
            && !decision_has_evidence(message)
        {
            return Err(OrbitError::InvalidInput(
                "operator validation decision requires evidence after its first line".into(),
            ));
        }
        if let Some(message) = decision
            && message.lines().next().unwrap_or_default().trim()
                == "task-pilot-admission: evaluated"
        {
            let evidence = message
                .split_once('\n')
                .map_or("", |(_, evidence)| evidence);
            let references_artifact = |artifacts: &[TaskArtifact]| {
                artifacts.iter().any(|artifact| {
                    !artifact.path.trim().is_empty()
                        && !artifact.content.is_empty()
                        && evidence.contains(&artifact.path)
                })
            };
            if !references_artifact(&params.upsert_artifacts)
                && !references_artifact(&self.get_task_artifacts(&task.id)?)
            {
                return Err(OrbitError::InvalidInput(
                    "evaluated operator validation decisions must reference a non-empty attached evaluation artifact".into(),
                ));
            }
        }
        let comments = self.get_task_comments(&task.id)?;
        let operation_id = comments
            .iter()
            .rev()
            .filter(|comment| comment.by == "task-pilot")
            .find_map(|comment| {
                comment
                    .message
                    .lines()
                    .next()?
                    .strip_prefix("operation_id=")
            })
            .ok_or_else(|| {
                OrbitError::Execution("operator validation decision has no pilot receipt".into())
            })?;
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor.into(),
            event: "operator_validation_resolved".into(),
            note: Some(
                json!({
                    "operation_id": operation_id,
                    "decision": if rescoped { "rescope" } else {
                        decision.unwrap_or_default().lines().next().unwrap_or_default().trim()
                    }, "hold": hold, "evidence": params.comment,
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
        }))
    }
}
