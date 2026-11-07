//! Typed pilot admission holds and their persisted finding identities.

use orbit_types::task::{
    DecisionAnswer, HostOs, PilotAdmissionVerdict, Task, TaskHistoryEntry, TaskOsRequirement,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::super::finding_verification::UnverifiedFindingHold;

use super::backlog::{criteria_digest, host_operational_material, validation_material};

/// A finding in the latest applied assessment that requires an operator decision.
pub(crate) enum PilotAdmissionHold {
    /// Edited description/criteria need an explicitly requested new assessment.
    StaleAssessment(String),
    Duplicate,
    AlreadyLanded,
    OperatorValidation(OperatorValidationHold),
    HostOperational(HostOperationalHold),
    /// Reported only while the task's `os:` tags miss a required OS or the
    /// pilot required this owner's machine. It holds a host by
    /// [`NativeOsHold::wait_on`], so a capable host may still start the task.
    NativeOs(NativeOsHold),
    /// A review-filed bug without a CONFIRMED verification verdict, while the
    /// workspace sets `[review] verify_findings`. Ranked first: until the
    /// finding is confirmed, no assessment of it is acted on.
    UnverifiedFinding(UnverifiedFindingHold),
}

/// Operator-side work that no managed lane can perform, scoped to the material
/// the pilot assessed. Lifecycle approval does not satisfy this finding.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostOperationalHold {
    pub(super) material: String,
    evidence: String,
}

impl HostOperationalHold {
    pub(crate) fn new(task: &Task, evidence: String) -> Self {
        Self {
            material: host_operational_material(task),
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
            operation_id: None,
            decision: None,
        }
    }
}

impl PilotAdmissionHold {
    /// Whether evidenced operator clear/override actions can resolve this hold.
    pub(crate) fn is_resolvable(&self) -> bool {
        matches!(
            self,
            Self::OperatorValidation(_)
                | Self::NativeOs(_)
                | Self::HostOperational(_)
                | Self::Duplicate
                | Self::AlreadyLanded
        )
    }

    /// Answers a decision request for this hold may offer.
    ///
    /// Duplicate and already-landed findings resolve only through a verdict
    /// that overrides the finding. `evaluated` does not, so it is not offered:
    /// an operator who chose it would spend the request and leave the hold.
    pub(crate) fn decision_request_answers(&self) -> Vec<DecisionAnswer> {
        match self {
            Self::Duplicate | Self::AlreadyLanded => {
                PilotAdmissionVerdict::finding_override_decision_answers()
            }
            _ => PilotAdmissionVerdict::decision_answers(),
        }
    }

    /// Whether `verdict` records a resolution for this hold.
    ///
    /// A duplicate or already-landed finding accepts only an overriding
    /// verdict. Every other hold keeps the answers it already accepted;
    /// evidence rules for `evaluated` still run when that comment is applied.
    pub(crate) fn accepts_decision_answer(&self, verdict: PilotAdmissionVerdict) -> bool {
        match self {
            Self::Duplicate | Self::AlreadyLanded => verdict.overrides_findings(),
            _ => true,
        }
    }

    pub(super) fn operator_handoff(
        &self,
        operation_id: &str,
    ) -> Option<(TaskHistoryEntry, &'static str)> {
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
    pub(super) material: String,
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
            operation_id: None,
            decision: None,
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
    pub(super) criteria: String,
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

    /// Requirement order and repetitions do not change the assessed finding.
    pub(super) fn same_finding(&self, other: &Self) -> bool {
        let requirements = |hold: &Self| {
            let mut requirements = hold
                .requirements
                .iter()
                .map(|requirement| (requirement.criterion, requirement.os))
                .collect::<Vec<_>>();
            requirements.sort_unstable();
            requirements.dedup();
            requirements
        };
        self.criteria == other.criteria
            && self.machine == other.machine
            && requirements(self) == requirements(other)
    }

    /// Whether anything is left for admission to judge under `tags`.
    pub(super) fn outstanding(&self, tags: &TaskOsRequirement) -> bool {
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
                "Machine requirement: criterion {criteria} needs evidence only machine {name} can produce, so {caller} cannot satisfy it. Run it on {name}, re-scope the criterion, or record an operator decision with evidence using `task-pilot-admission: clear` or `approve-anyway` as the comment's first line. The decision remains valid across newer assessments with the same criteria digest and native-host requirements; changed criteria or requirements supersede it."
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
            "Native OS requirement: {needs}, and the task's tags lack {add}, so a host running {host} cannot satisfy it. Add {add} with `orbit.task.update` so admission routes it to a capable host, re-scope the criterion, or record an operator decision with evidence using `task-pilot-admission: clear` or `approve-anyway` as the comment's first line. The decision remains valid across newer assessments with the same criteria digest and native-host requirements; changed criteria or requirements supersede it."
        ))
    }
}
