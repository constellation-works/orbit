//! Backlog holds from applied pilot findings, including assessments made
//! without lifecycle promotion. Decisions are scoped to the latest assessment.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::governed_tool;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{
    HostOs, Task, TaskArtifact, TaskComment, TaskHistoryEntry, TaskOsRequirement, TaskStatus,
};
use orbit_types::tool::McpCapability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::positive_validation_tools;

use super::helpers::is_automation_actor;
use crate::OrbitRuntime;

/// The trusted history events an operator decision on a hold records.
const OPERATOR_VALIDATION_RESOLVED: &str = "operator_validation_resolved";
const HOST_OPERATIONAL_RESOLVED: &str = "host_operational_resolved";
const NATIVE_OS_RESOLVED: &str = "native_os_requirement_resolved";

/// A finding in the latest applied assessment that requires an operator decision.
pub(crate) enum PilotAdmissionHold {
    Duplicate,
    AlreadyLanded,
    OperatorValidation(OperatorValidationHold),
    HostOperational(HostOperationalHold),
    /// Reported only while the task's `os:` tags miss a required OS or the
    /// pilot required this owner's machine. It holds a host by
    /// [`NativeOsHold::wait_on`], so a capable host may still start the task.
    NativeOs(NativeOsHold),
}

/// Operator-side work that no managed lane can perform, scoped to the material
/// the pilot assessed. Lifecycle approval does not satisfy this finding.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostOperationalHold {
    material: String,
    evidence: String,
}

impl HostOperationalHold {
    pub(crate) fn new(task: &Task, evidence: String) -> Self {
        Self {
            material: validation_material(task),
            evidence,
        }
    }

    pub(crate) fn detail(&self) -> String {
        format!(
            "Operator handoff: the latest task-pilot disposition is `host_operational`. Pilot evidence: {}. Re-scope the work, or record an operator decision with evidence using `task-pilot-admission: evaluated`, `clear`, or `approve-anyway` as the comment's first line. A newer assessment supersedes the decision. This decision does not grant tools.",
            self.evidence
        )
    }

    pub(crate) fn history(&self, operation_id: &str) -> TaskHistoryEntry {
        TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: "task-pilot".into(),
            event: "host_operational_held".into(),
            note: Some(
                json!({
                    "operation_id": operation_id, "disposition": "host_operational", "hold": self,
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
        }
    }
}

impl PilotAdmissionHold {
    fn operator_handoff(&self, operation_id: &str) -> Option<(TaskHistoryEntry, &'static str)> {
        match self {
            Self::OperatorValidation(hold) => {
                Some((hold.history(operation_id), "operator-validation-handoff"))
            }
            Self::HostOperational(hold) => {
                Some((hold.history(operation_id), "host-operational-handoff"))
            }
            _ => None,
        }
    }
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

/// A criterion whose evidence only a host of one native OS can produce.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeOsRequirement {
    /// 1-based, like an [`OperatorValidationRequirement`].
    pub(crate) criterion: usize,
    pub(crate) os: HostOs,
}

/// Criteria whose evidence only the machine that owns the task store can
/// produce: its live store, services or data [ORB-15278].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MachineRequirement {
    /// 1-based, like a [`NativeOsRequirement`].
    pub(crate) criteria: Vec<usize>,
    pub(crate) machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine_name: Option<String>,
}

/// The pilot's typed native-host finding, persisted with its atomic audit.
/// It stays current while the acceptance criteria it assessed are unchanged,
/// so re-scoping a criterion releases it; adding the matching `os:` tag
/// satisfies an OS requirement without a decision. A machine requirement
/// has no tag: only that machine, a re-scope or a decision satisfies it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeOsHold {
    criteria: String,
    pub(crate) requirements: Vec<NativeOsRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine: Option<MachineRequirement>,
}

impl NativeOsHold {
    pub(crate) fn new(
        task: &Task,
        requirements: Vec<NativeOsRequirement>,
        machine: Option<MachineRequirement>,
    ) -> Self {
        Self {
            criteria: criteria_digest(task),
            requirements,
            machine,
        }
    }

    /// Whether anything is left for admission to judge under `tags`.
    fn outstanding(&self, tags: &TaskOsRequirement) -> bool {
        self.machine.is_some() || self.untagged(tags).next().is_some()
    }

    /// Requirements whose OS the task's `os:` tags do not name.
    fn untagged<'a>(
        &'a self,
        tags: &'a TaskOsRequirement,
    ) -> impl Iterator<Item = &'a NativeOsRequirement> {
        self.requirements
            .iter()
            .filter(|requirement| !tags.any_of.contains(&requirement.os))
    }

    /// Why a host running `host` as machine `machine_id` may not start
    /// `task`, or `None` when it may. A host the task's own `os:` tags
    /// exclude is left to that routing, which names its own wait
    /// (`host_os_mismatch`).
    pub(crate) fn wait_on(
        &self,
        task: &Task,
        host: Option<HostOs>,
        machine_id: Option<&str>,
    ) -> Option<String> {
        let tags = TaskOsRequirement::from_tags(&task.tags);
        if !tags.satisfied_by(host) {
            return None;
        }
        if let Some(machine) = &self.machine
            && machine_id != Some(machine.machine_id.as_str())
        {
            let criteria = machine
                .criteria
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let name = machine
                .machine_name
                .as_deref()
                .unwrap_or(&machine.machine_id);
            let caller = machine_id.map_or_else(
                || "a host with no machine identity".to_string(),
                |id| format!("machine {id}"),
            );
            return Some(format!(
                "Machine requirement: criterion {criteria} needs evidence only machine {name} can produce, so {caller} cannot satisfy it. Run it on {name}, re-scope the criterion, or record an operator decision with evidence using `task-pilot-admission: clear` or `approve-anyway` as the comment's first line. A newer assessment supersedes the decision."
            ));
        }
        let unmet = self
            .untagged(&tags)
            .filter(|requirement| host != Some(requirement.os))
            .collect::<Vec<_>>();
        if unmet.is_empty() {
            return None;
        }
        let needs = unmet
            .iter()
            .map(|requirement| {
                format!(
                    "criterion {} needs native {} evidence",
                    requirement.criterion, requirement.os
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let mut add = unmet
            .iter()
            .map(|requirement| format!("`{}`", requirement.os.tag()))
            .collect::<Vec<_>>();
        add.sort();
        add.dedup();
        let add = add.join(" or ");
        let host = host.map_or("an undeclared OS", HostOs::as_str);
        Some(format!(
            "Native OS requirement: {needs}, and the task's tags lack {add}, so a host running {host} cannot satisfy it. Add {add} with `orbit.task.update` so admission routes it to a capable host, re-scope the criterion, or record an operator decision with evidence using `task-pilot-admission: clear` or `approve-anyway` as the comment's first line. A newer assessment supersedes the decision."
        ))
    }
}

/// Typed operator requirements derived directly from positive mentions of registered tools
/// that are governed operations reserved for non-agent capabilities.
pub(crate) fn operator_validation_requirements(
    task: &Task,
    registered: &[String],
) -> Vec<OperatorValidationRequirement> {
    positive_validation_tools(task, registered)
        .into_iter()
        .filter(|(_, tool)| {
            governed_tool(tool)
                .is_some_and(|operation| !operation.allowed.contains(&McpCapability::Agent))
        })
        .map(|(criterion, tool)| OperatorValidationRequirement {
            criterion,
            tool: tool.into(),
        })
        .collect()
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

/// What a [`NativeOsHold`] assessed: editing any acceptance criterion is a
/// re-scope that releases it.
fn criteria_digest(task: &Task) -> String {
    sha256_hex(json!(task.acceptance_criteria).to_string().as_bytes())
}

/// Whether `history` holds a trusted operator resolution `event` of the hold
/// that `operation_id`'s receipt persisted, while `scope` (a field of that
/// hold and its current value) still matches.
fn operator_resolved(
    history: &[TaskHistoryEntry],
    event: &str,
    operation_id: &str,
    (field, current): (&str, &str),
) -> bool {
    history.iter().any(|entry| {
        if entry.event != event || is_automation_actor(&entry.by) {
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
            && record["hold"][field] == current
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
    })
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
        self.pilot_admission_hold_in(&task, &comments, &|| self.get_task_history(task_id))
    }

    /// [`Self::pilot_admission_hold`] over a task and comments the caller has
    /// read, reading history through `history` only when a pilot receipt
    /// needs it. Pull admission re-checks the candidate it is about to claim
    /// this way, from the bundle its exclusive section already read, rather
    /// than reading the store inside that section [ORB-14724].
    pub(crate) fn pilot_admission_hold_in(
        &self,
        task: &Task,
        comments: &[TaskComment],
        history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
    ) -> Result<Option<PilotAdmissionHold>, OrbitError> {
        let material = validation_material(task);
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
            if assessment["disposition"] == "host_operational" {
                // New receipts seal the assessed material. Older receipts have
                // no snapshot: hold conservatively until admission or a trusted
                // operator decision records one. A generic `updated` event
                // cannot distinguish a re-scope from unrelated metadata edits.
                let task_history = history()?;
                let recorded = audit
                    .get("host_operational_hold")
                    .filter(|hold| !hold.is_null())
                    .cloned()
                    .or_else(|| {
                        task_history.iter().rev().find_map(|entry| {
                            let trusted = match entry.event.as_str() {
                                "host_operational_held" => {
                                    matches!(entry.by.as_str(), "system" | "task-pilot")
                                }
                                HOST_OPERATIONAL_RESOLVED => {
                                    !entry.by.trim().is_empty() && !is_automation_actor(&entry.by)
                                }
                                _ => false,
                            };
                            if !trusted {
                                return None;
                            }
                            let record: Value =
                                serde_json::from_str(entry.note.as_deref()?).ok()?;
                            (record["operation_id"] == operation_id).then(|| record["hold"].clone())
                        })
                    });
                let hold = match recorded {
                    Some(hold) => Some(
                        serde_json::from_value::<HostOperationalHold>(hold).map_err(|error| {
                            OrbitError::Execution(format!("decode host operational hold: {error}"))
                        })?,
                    ),
                    None => Some(HostOperationalHold::new(
                        task,
                        assessment["evidence"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )),
                };
                if let Some(hold) = hold
                    && hold.material == material
                    && !operator_resolved(
                        &task_history,
                        HOST_OPERATIONAL_RESOLVED,
                        operation_id,
                        ("material", &material),
                    )
                {
                    return Ok(Some(PilotAdmissionHold::HostOperational(hold)));
                }
            }
            let operator_decision = operator_resolved(
                &history()?,
                OPERATOR_VALIDATION_RESOLVED,
                operation_id,
                ("material", &material),
            );
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
                let edited = history()?.iter().any(|event| {
                    event.at > comment.at && matches!(event.event.as_str(), "updated" | "renamed")
                });
                if !edited {
                    let mut registered = self.allowlist_known_tool_names();
                    registered.sort();
                    let requirements = operator_validation_requirements(task, &registered);
                    if !requirements.is_empty() {
                        return Ok(Some(PilotAdmissionHold::OperatorValidation(
                            OperatorValidationHold::new(task, requirements),
                        )));
                    }
                }
            }
            // Ranked after the other findings: it holds only some hosts, so a
            // duplicate or landed finding must not hide behind it.
            let native_os = match audit.get("native_os_hold").filter(|hold| !hold.is_null()) {
                Some(hold) => {
                    let hold: NativeOsHold =
                        serde_json::from_value(hold.clone()).map_err(|error| {
                            OrbitError::Execution(format!("decode native OS hold: {error}"))
                        })?;
                    let tags = TaskOsRequirement::from_tags(&task.tags);
                    let current = hold.criteria == criteria_digest(task) && hold.outstanding(&tags);
                    (current
                        && !operator_resolved(
                            &history()?,
                            NATIVE_OS_RESOLVED,
                            operation_id,
                            ("criteria", &hold.criteria),
                        ))
                    .then_some(PilotAdmissionHold::NativeOs(hold))
                }
                None => None,
            };
            if legacy_decision {
                return Ok(native_os);
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
            return Ok(native_os);
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
        self.record_pilot_operator_handoff(
            task_id,
            &PilotAdmissionHold::OperatorValidation(hold.clone()),
        )
    }

    pub(crate) fn record_host_operational_hold(
        &self,
        task_id: &str,
        hold: &HostOperationalHold,
    ) -> Result<(), OrbitError> {
        self.record_pilot_operator_handoff(
            task_id,
            &PilotAdmissionHold::HostOperational(hold.clone()),
        )
    }

    fn record_pilot_operator_handoff(
        &self,
        task_id: &str,
        hold: &PilotAdmissionHold,
    ) -> Result<(), OrbitError> {
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let Some(current) = self.pilot_admission_hold(task_id)? else {
                    return Ok(());
                };
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
                        OrbitError::Execution("operator handoff has no pilot receipt".into())
                    })?;
                let Some((mut event, header)) = hold.operator_handoff(operation_id) else {
                    return Ok(());
                };
                let Some((current_event, _)) = current.operator_handoff(operation_id) else {
                    return Ok(());
                };
                if event.event != current_event.event || event.note != current_event.note {
                    return Ok(());
                }
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
                                "{header}\n{}",
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

    pub(crate) fn record_backlog_pilot_operator_handoffs(&self) -> Result<(), OrbitError> {
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)?
        {
            if let Some(
                hold @ (PilotAdmissionHold::OperatorValidation(_)
                | PilotAdmissionHold::HostOperational(_)),
            ) = self.pilot_admission_hold(&task.id)?
            {
                self.record_pilot_operator_handoff(&task.id, &hold)?;
            }
        }
        Ok(())
    }

    /// Only the trusted human update path calls this. Agent-authored comment
    /// text cannot mint a resolution event or satisfy an operator handoff.
    /// Each typed hold records its own resolution event;
    /// one decision resolves the hold admission currently reports.
    pub(super) fn pilot_hold_resolution(
        &self,
        task: &Task,
        params: &super::TaskUpdateParams,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let (event, hold) = match self.pilot_admission_hold(&task.id)? {
            Some(PilotAdmissionHold::OperatorValidation(hold)) => {
                (OPERATOR_VALIDATION_RESOLVED, json!(hold))
            }
            Some(PilotAdmissionHold::NativeOs(hold)) => (NATIVE_OS_RESOLVED, json!(hold)),
            Some(PilotAdmissionHold::HostOperational(hold)) => {
                (HOST_OPERATIONAL_RESOLVED, json!(hold))
            }
            _ => return Ok(None),
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
                "a task-pilot admission decision requires evidence after its first line".into(),
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
                    "evaluated task-pilot admission decisions must reference a non-empty attached evaluation artifact".into(),
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
                OrbitError::Execution("task-pilot admission decision has no pilot receipt".into())
            })?;
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor.into(),
            event: event.into(),
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

impl OrbitRuntime {
    /// Whether a task-pilot assessment has been applied to the task: it
    /// carries a task-pilot receipt comment [ORB-15191]. A backlog task the
    /// pilot assessed and still left without selectors is one it declined to
    /// prepare, so admission stops waiting for it.
    pub(crate) fn task_pilot_assessed(&self, task_id: &str) -> Result<bool, OrbitError> {
        Ok(self.get_task_comments(task_id)?.iter().any(|comment| {
            comment.by == "task-pilot"
                && comment
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .starts_with("operation_id=")
        }))
    }
}
