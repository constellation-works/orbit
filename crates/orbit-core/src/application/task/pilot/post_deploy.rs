//! The pilot's typed post-deploy finding: criteria only the deployed build on
//! a named host can evidence.
//!
//! Unlike `required_machine`, the finding routes nothing and holds no
//! admission: running on that host would still measure the deployed build,
//! not the candidate. It only lets an implementer hand the criterion off as
//! owed after deploy, so the owner can tell a typed criterion from one a
//! worker would rather not meet.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskComment};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::{MachineRequirement, NativeOsRequirement};

use super::admission::criteria_digest;

/// The audit field the finding is persisted under.
pub(super) const POST_DEPLOY_AUDIT_FIELD: &str = "post_deploy";

/// One criterion only the deployed build on `host` can evidence, and the
/// command that measures it there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostDeployRequirement {
    /// 1-based, like a [`NativeOsRequirement`].
    pub(crate) criterion: usize,
    pub(crate) host: String,
    pub(crate) command: String,
}

/// The finding as the pilot audit persists it. It stays current while the
/// acceptance criteria it assessed are unchanged.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostDeployFinding {
    criteria: String,
    pub(crate) requirements: Vec<PostDeployRequirement>,
}

impl PostDeployFinding {
    pub(super) fn new(task: &Task, requirements: Vec<PostDeployRequirement>) -> Self {
        Self {
            criteria: criteria_digest(task),
            requirements,
        }
    }
}

/// Current requirements from the latest pilot audit. Admission and handoff
/// must use the same criteria digest when interpreting this finding.
pub(super) fn requirements_from_audit(
    task: &Task,
    audit: &Value,
) -> Result<Vec<PostDeployRequirement>, OrbitError> {
    let Some(finding) = audit
        .get(POST_DEPLOY_AUDIT_FIELD)
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let finding: PostDeployFinding = serde_json::from_value(finding.clone())
        .map_err(|error| OrbitError::Execution(format!("decode post-deploy finding: {error}")))?;
    Ok(if finding.criteria == criteria_digest(task) {
        finding.requirements
    } else {
        Vec::new()
    })
}

/// Validate the assessment's `post_deploy` entries: each names a 1-based
/// criterion, the host and the measurement command, and why only the
/// deployed build there can evidence it. A criterion the assessment also
/// routes to a native OS or to this owner's machine is refused: routing
/// would only reach the deployed build, never the candidate.
pub(super) fn post_deploy(
    action: &str,
    task_id: &str,
    assessment: &Value,
    criteria: usize,
    native_os: &[NativeOsRequirement],
    machine: Option<&MachineRequirement>,
) -> Result<Vec<PostDeployRequirement>, DispatchError> {
    let Some(entries) = assessment
        .get(POST_DEPLOY_AUDIT_FIELD)
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let invalid = |detail: &str| {
        DispatchError::action_failed(action, format!("task {task_id} post_deploy {detail}"))
    };
    let text = |entry: &Value, field: &str| {
        entry
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| invalid(&format!("{field} must be a non-empty string")))
    };
    let mut requirements: Vec<PostDeployRequirement> = Vec::new();
    for entry in entries
        .as_array()
        .ok_or_else(|| invalid("must be an array or null"))?
    {
        let criterion = entry
            .get("criterion")
            .and_then(Value::as_u64)
            .and_then(|criterion| usize::try_from(criterion).ok())
            .filter(|criterion| (1..=criteria).contains(criterion))
            .ok_or_else(|| {
                invalid(&format!(
                    "criterion must be a 1-based index of the task's {criteria} acceptance criteria"
                ))
            })?;
        let host = text(entry, "host")?;
        let command = text(entry, "command")?;
        text(entry, "evidence")?;
        let routed = native_os
            .iter()
            .any(|requirement| requirement.criterion == criterion)
            || machine.is_some_and(|machine| machine.criteria.contains(&criterion));
        if routed {
            return Err(invalid(&format!(
                "criterion {criterion} is also in required_os or required_machine; a criterion \
                 only the deployed build can evidence needs no routing, so type it in one place"
            )));
        }
        if requirements
            .iter()
            .any(|requirement| requirement.criterion == criterion)
        {
            return Err(invalid(&format!("names criterion {criterion} twice")));
        }
        requirements.push(PostDeployRequirement {
            criterion,
            host,
            command,
        });
    }
    Ok(requirements)
}

impl OrbitRuntime {
    /// The post-deploy requirements of the task's latest applied pilot
    /// assessment, while the acceptance criteria it assessed are unchanged.
    /// A newer assessment without the finding, or a re-scoped criterion,
    /// leaves none.
    pub(crate) fn pilot_post_deploy(
        &self,
        task: &Task,
        comments: &[TaskComment],
    ) -> Result<Vec<PostDeployRequirement>, OrbitError> {
        let Some((_, receipt)) = super::pilot_receipts(comments).next() else {
            return Ok(Vec::new());
        };
        let audit: Value = serde_json::from_str(receipt.body)
            .map_err(|error| OrbitError::Execution(format!("decode pilot assessment: {error}")))?;
        requirements_from_audit(task, &audit)
    }
}
