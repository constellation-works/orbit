//! Native OS, machine and UI requirements from pilot assessments.
use super::super::source::SourceSnapshot;
use crate::application::task::{MachineRequirement, NativeOsRequirement};
use orbit_engine::DispatchError;
use orbit_types::task::{ExecutionLocation, HostOs};
use serde_json::Value;
use std::path::Path;

/// OS and native-facility tokens a criterion must name before `required_os`
/// may cite it. Matched as ASCII case-insensitive tokens, so a portable
/// word such as "machine" is not read as "mac".
const NATIVE_FACILITY_TERMS: &[&str] = &[
    "mac",
    "macos",
    "darwin",
    "seatbelt",
    "sandbox-exec",
    "linux",
    "bwrap",
    "bubblewrap",
    "windows",
    "launchd",
    "systemd",
    "keychain",
    "cgroup",
];

/// The pilot's typed native-OS finding: each `required_os` entry names a
/// 1-based criterion of the task's acceptance criteria, the OS whose native
/// host alone can produce its evidence, and that evidence. Absent or empty
/// is no finding. Admission holds only this typed field, never warning prose.
///
/// A criterion that names none of [`NATIVE_FACILITY_TERMS`] cannot justify
/// the finding unless its evidence cites a `#[cfg(target_os = "<os>")]` gate
/// that the pinned source confirms, on an item the criterion names, with
/// `os` equal to that gate. Any other citation is rejected so the repair
/// pilot leaves `required_os` empty instead of holding a portable task on a
/// native host.
pub(in super::super) fn required_os(
    action: &str,
    task_id: &str,
    assessment: &Value,
    criteria: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
) -> Result<Vec<NativeOsRequirement>, DispatchError> {
    let Some(entries) = assessment
        .get("required_os")
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let invalid = |detail: &str| {
        DispatchError::action_failed(action, format!("task {task_id} required_os {detail}"))
    };
    let mut requirements = Vec::new();
    for entry in entries
        .as_array()
        .ok_or_else(|| invalid("must be an array or null"))?
    {
        let criterion = entry
            .get("criterion")
            .and_then(Value::as_u64)
            .and_then(|criterion| usize::try_from(criterion).ok())
            .filter(|criterion| (1..=criteria.len()).contains(criterion))
            .ok_or_else(|| {
                invalid(&format!(
                    "criterion must be a 1-based index of the task's {} acceptance criteria",
                    criteria.len()
                ))
            })?;
        let os = entry
            .get("os")
            .and_then(Value::as_str)
            .and_then(HostOs::parse)
            .ok_or_else(|| invalid("os must be linux, macos, or windows"))?;
        let evidence = entry
            .get("evidence")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|evidence| !evidence.is_empty())
            .ok_or_else(|| invalid("evidence must be a non-empty string"))?;
        // `criterion` is a checked 1-based index of `criteria`.
        if !criterion_names_native_facility(&criteria[criterion - 1]) {
            match super::super::cfg_gate::admit_cfg_gate(&super::super::cfg_gate::CfgGateCheck {
                action,
                task_id,
                criterion_index: criterion,
                criterion: &criteria[criterion - 1],
                evidence,
                os,
                workspace_root,
                source,
            })? {
                super::super::cfg_gate::CfgGateDecision::NotCited => {
                    return Err(invalid(&format!(
                        "criterion {criterion} names no OS or native facility ({}); leave required_os \
                         empty for portable criteria",
                        NATIVE_FACILITY_TERMS.join(", ")
                    )));
                }
                super::super::cfg_gate::CfgGateDecision::Admit => {}
            }
        }
        let requirement = NativeOsRequirement { criterion, os };
        if !requirements.contains(&requirement) {
            requirements.push(requirement);
        }
    }
    Ok(requirements)
}

/// Whether `text` names an OS or native facility as a whole token.
fn criterion_names_native_facility(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    NATIVE_FACILITY_TERMS
        .iter()
        .any(|term| contains_native_facility_term(&text, term))
}

fn contains_native_facility_term(text: &str, term: &str) -> bool {
    let mut rest = text;
    while let Some(index) = rest.find(term) {
        let end = index + term.len();
        let before = index == 0 || !is_facility_token_byte(rest.as_bytes()[index - 1]);
        let after = end == rest.len() || !is_facility_token_byte(rest.as_bytes()[end]);
        if before && after {
            return true;
        }
        rest = &rest[index + 1..];
    }
    false
}

/// Letters, digits and `_` continue a token. A hyphen separates one, so
/// `sandbox-exec` is its own term and does not match inside a longer word.
fn is_facility_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The one OS a validated `required_os` finding routes the task to: every
/// requirement names it. Requirements on several OSes name no single host, so
/// they route nothing and stay a hold for an operator to split.
pub(in super::super) fn routing_os(requirements: &[NativeOsRequirement]) -> Option<HostOs> {
    let os = requirements.first()?.os;
    requirements
        .iter()
        .all(|requirement| requirement.os == os)
        .then_some(os)
}

/// The pilot's typed UI finding: `user_facing_ui` is true when the task's
/// criteria or context files are UI a user sees. Absent or null is false, so
/// an older pilot result still applies. A true finding adds the `ui` tag;
/// nothing removes it.
pub(in super::super) fn user_facing_ui(
    action: &str,
    task_id: &str,
    assessment: &Value,
) -> Result<bool, DispatchError> {
    match assessment.get("user_facing_ui") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(finding)) => Ok(*finding),
        Some(_) => Err(DispatchError::action_failed(
            action,
            format!("task {task_id} user_facing_ui must be a boolean or null"),
        )),
    }
}

/// The pilot's typed machine finding: each `required_machine` entry names a
/// 1-based criterion whose evidence only `owner`, the machine that owns this
/// workspace's task store, can produce, and that evidence. Absent or empty is
/// no finding. The entry must name the owner by machine id or name: a pilot
/// cannot route work to a machine this owner does not know.
pub(in super::super) fn required_machine(
    action: &str,
    task_id: &str,
    assessment: &Value,
    criteria: usize,
    owner: Option<&ExecutionLocation>,
) -> Result<Option<MachineRequirement>, DispatchError> {
    let Some(entries) = assessment
        .get("required_machine")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let invalid = |detail: &str| {
        DispatchError::action_failed(action, format!("task {task_id} required_machine {detail}"))
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| invalid("must be an array or null"))?;
    if entries.is_empty() {
        return Ok(None);
    }
    let owner = owner.ok_or_else(|| {
        invalid(
            "needs this owner's machine identity, and it has none; no other machine can claim \
             its tasks, so leave required_machine empty",
        )
    })?;
    let mut requirement = MachineRequirement {
        criteria: Vec::new(),
        machine_id: owner.machine_id.clone(),
        machine_name: owner.machine_name.clone(),
    };
    for entry in entries {
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
        let machine = entry
            .get("machine")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        let names_owner = machine.eq_ignore_ascii_case(&owner.machine_id)
            || owner
                .machine_name
                .as_deref()
                .is_some_and(|name| machine.eq_ignore_ascii_case(name.trim()));
        if !names_owner {
            let owner_name = owner.machine_name.as_deref().map_or_else(
                || owner.machine_id.clone(),
                |name| format!("{name} ({})", owner.machine_id),
            );
            return Err(invalid(&format!(
                "machine {machine:?} must name this owner, {owner_name}; report a requirement \
                 on another machine in utility_warnings instead"
            )));
        }
        entry
            .get("evidence")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|evidence| !evidence.is_empty())
            .ok_or_else(|| invalid("evidence must be a non-empty string"))?;
        requirement.criteria.push(criterion);
    }
    requirement.criteria.sort_unstable();
    requirement.criteria.dedup();
    Ok(Some(requirement))
}
