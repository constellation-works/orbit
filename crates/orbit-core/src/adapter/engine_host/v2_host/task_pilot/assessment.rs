//! Validation of an agent's task-pilot assessment: selectors and recommendations.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::fs::selector::{
    anchor_path, canonical_selector, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_engine::DispatchError;
use orbit_types::task::{ExecutionLocation, HostOs, TaskComplexity};
use serde_json::{Value, json};

use crate::application::task::{MachineRequirement, NativeOsRequirement};

use super::VALIDATION_TOOL_WARNINGS;
use super::input::{action_failed, required_string, required_string_array, string_array_value};
use super::source::{GitPathKind, SourceSnapshot};

pub(super) struct ValidatedSelectors {
    pub(super) values: Vec<String>,
    pub(super) normalizations: Vec<Value>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_after_selectors(
    action: &str,
    task_id: &str,
    disposition: &str,
    assessment: &Value,
    selectors: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
    authorized_creation: &[String],
) -> Result<ValidatedSelectors, DispatchError> {
    if selectors.is_empty() {
        if !matches!(disposition, "verified_no_diff" | "host_operational") {
            return Err(action_failed(
                action,
                format!(
                    "task {task_id} may keep empty context_files only with verified_no_diff or host_operational disposition"
                ),
            ));
        }
        required_string(assessment, "evidence", action)?;
        return Ok(ValidatedSelectors {
            values: Vec::new(),
            normalizations: Vec::new(),
        });
    }
    if disposition != "selectors" {
        return Err(action_failed(
            action,
            format!(
                "task {task_id} has non-empty context_files_after but disposition is {disposition}"
            ),
        ));
    }

    let mut seen = BTreeSet::new();
    let mut values = Vec::with_capacity(selectors.len());
    let mut normalizations = Vec::new();
    for selector in selectors {
        let trimmed = selector.trim();
        let has_kind = matches!(
            trimmed.split_once(':').map(|(kind, _)| kind),
            Some("file" | "dir" | "symbol")
        );
        let candidate = if has_kind {
            trimmed.to_string()
        } else {
            if trimmed.contains(':') {
                return Err(action_failed(
                    action,
                    format!(
                        "task {task_id} bare selector {selector:?} is ambiguous because it contains ':'"
                    ),
                ));
            }
            let source = source.ok_or_else(|| {
                action_failed(
                    action,
                    format!(
                        "task {task_id} selector {selector:?} must use file:, dir:, or symbol: when no pinned source is available"
                    ),
                )
            })?;
            let path_candidate =
                canonical_selector(&format!("file:{trimmed}")).map_err(|error| {
                    action_failed(
                        action,
                        format!("task {task_id} bare selector {selector:?} is invalid: {error}"),
                    )
                })?;
            let anchor = anchor_path(&path_candidate).map_err(|error| {
                action_failed(
                    action,
                    format!("task {task_id} bare selector {selector:?} is invalid: {error}"),
                )
            })?;
            let kind = source.path_kind(action, workspace_root, &anchor)?;
            let normalized = match kind {
                GitPathKind::Blob => path_candidate,
                GitPathKind::Tree => canonical_selector(&format!("dir:{trimmed}"))
                    .map_err(|error| action_failed(action, error.to_string()))?,
                GitPathKind::Missing => {
                    return Err(action_failed(
                        action,
                        format!(
                            "task {task_id} bare selector {selector:?} does not resolve at pinned source revision {}",
                            source.source_revision
                        ),
                    ));
                }
                GitPathKind::Other => {
                    return Err(action_failed(
                        action,
                        format!(
                            "task {task_id} bare selector {selector:?} has an unsupported or ambiguous kind at pinned source revision {}",
                            source.source_revision
                        ),
                    ));
                }
            };
            normalizations.push(json!({
                "original": selector,
                "normalized": normalized,
            }));
            normalized
        };
        // A dirty primary may replace an anchor with a symlink or a different
        // kind. Only syntax comes from this parser; pinned Git objects own
        // containment and target validation when source identity is present.
        let canonical = if source.is_some() {
            canonical_selector(&candidate)
        } else {
            canonical_selector_in_workspace(&candidate, workspace_root)
        }
        .map_err(|error| {
            action_failed(
                action,
                format!("task {task_id} selector {selector:?} is invalid: {error}"),
            )
        })?;
        if has_kind && canonical != trimmed {
            return Err(action_failed(
                action,
                format!(
                    "task {task_id} selector {selector:?} is not canonical; expected {canonical:?}"
                ),
            ));
        }
        // Only an exact selector the task's durable creation grant names may
        // be missing; it still has to be absent, never a target of another
        // kind or a symlink.
        let authorized = has_kind && authorized_creation.contains(&canonical);
        if let Some(source) = source {
            validate_selector_at_source(
                action,
                task_id,
                &canonical,
                workspace_root,
                source,
                authorized,
            )?;
        } else if !exists_in_workspace(&canonical, workspace_root) {
            if !authorized {
                return Err(action_failed(
                    action,
                    format!(
                        "task {task_id} selector {selector:?} does not resolve to an existing in-workspace target{}",
                        reauthorization_hint(&canonical)
                    ),
                ));
            }
        } else {
            validate_selector_target_kind(action, task_id, &canonical, workspace_root)?;
        }
        if !seen.insert(canonical) {
            return Err(action_failed(
                action,
                format!("task {task_id} repeats selector {selector:?}"),
            ));
        }
        values.push(candidate);
    }
    Ok(ValidatedSelectors {
        values,
        normalizations,
    })
}

/// Whether a validated assessment leaves the task ready for the state
/// automation to promote it on its own. Actionable selectors are necessary but
/// not sufficient: any finding that names other work, or an action outside
/// what this repository owns — a duplicate, a repair that already landed, or
/// an operator-reserved release action — keeps that decision with a human
/// [ORB-11517]. A validation criterion the implementation lane cannot satisfy
/// does the same, because promoting it admits work whose acceptance check is
/// already known to be unreachable [ORB-11980].
pub(in super::super) fn member_ready(assessment: &Value) -> bool {
    // Unlike the agent's own findings below, this field is injected by the
    // deterministic apply boundary, so an assessment that predates the
    // injection reads as "no finding" rather than as "not ready".
    let validation_tools_feasible = assessment
        .get(VALIDATION_TOOL_WARNINGS)
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);

    validation_tools_feasible
        && matches!(
            assessment
                .get("recommended_complexity")
                .and_then(Value::as_str),
            Some("low" | "medium" | "hard" | "xhard")
        )
        && assessment["disposition"] == "selectors"
        && [
            "blocked_by",
            "adr_conflicts",
            "utility_warnings",
            "surface_warnings",
        ]
        .iter()
        .all(|field| {
            assessment
                .get(field)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
        && ["duplicate_of", "already_landed", "release_action_required"]
            .iter()
            .all(|field| assessment.get(field).is_none_or(Value::is_null))
}

/// Coerce a formatting slip in `evidence_gaps` into the array the contract
/// asks for: a missing or null field becomes `[]`, and a scalar string becomes
/// a one-element array (an empty string becomes `[]`). Returns the note that
/// records the rewrite, or `None` when the field was already an array or is
/// not a shape worth guessing at (validation then rejects it as before).
pub(super) fn normalize_evidence_gaps(assessment: &mut Value) -> Option<Value> {
    let fields = assessment.as_object_mut()?;
    let (coerced, was) = match fields.get("evidence_gaps") {
        None | Some(Value::Null) => (json!([]), "missing"),
        Some(Value::String(gap)) if gap.trim().is_empty() => (json!([]), "empty string"),
        Some(Value::String(gap)) => (json!([gap]), "string"),
        Some(_) => return None,
    };
    fields.insert("evidence_gaps".to_string(), coerced);
    Some(json!({
        "field": "evidence_gaps",
        "note": format!("{was} evidence_gaps coerced to an array"),
    }))
}

pub(super) fn validate_recommendations(
    action: &str,
    task_id: &str,
    assessment: &Value,
) -> Result<TaskComplexity, DispatchError> {
    required_string(assessment, "recommended_crew", action)?;
    let complexity = required_string(assessment, "recommended_complexity", action)?;
    let complexity = complexity.parse::<TaskComplexity>().map_err(|_| {
        action_failed(
            action,
            format!(
                "task {task_id} recommended_complexity must be low, medium, hard, xhard, or \
                 unassessed"
            ),
        )
    })?;
    required_string(assessment, "assessment_rationale", action)?;
    required_string(assessment, "validation_approach", action)?;
    let confidence = required_string(assessment, "confidence", action)?;
    if !matches!(confidence, "high" | "medium" | "low") {
        return Err(action_failed(
            action,
            format!("task {task_id} confidence must be high, medium, or low"),
        ));
    }
    let evidence_gaps = required_string_array(assessment, "evidence_gaps", action)?;
    required_string_array(assessment, "reassessment_triggers", action)?;
    if complexity == TaskComplexity::Unassessed && evidence_gaps.is_empty() {
        return Err(action_failed(
            action,
            format!("task {task_id} unassessed complexity requires actionable evidence_gaps"),
        ));
    }
    if complexity.is_assessed() && !evidence_gaps.is_empty() && confidence == "high" {
        return Err(action_failed(
            action,
            format!("task {task_id} with high confidence must not have evidence_gaps"),
        ));
    }
    for field in [
        "blocked_by",
        "adr_conflicts",
        "utility_warnings",
        "surface_warnings",
    ] {
        string_array_value(
            assessment.get(field).ok_or_else(|| {
                action_failed(action, format!("task {task_id} is missing {field}"))
            })?,
            field,
            action,
        )?;
    }
    for field in ["duplicate_of", "already_landed"] {
        if assessment.get(field).is_none() {
            return Err(action_failed(
                action,
                format!("task {task_id} is missing {field} recommendation"),
            ));
        }
    }
    validate_optional_finding(
        action,
        task_id,
        assessment,
        "duplicate_of",
        &["task_id", "evidence"],
    )?;
    validate_optional_finding(action, task_id, assessment, "already_landed", &["evidence"])?;
    validate_optional_finding(
        action,
        task_id,
        assessment,
        "release_action_required",
        &["action", "evidence"],
    )?;
    Ok(complexity)
}

/// The pilot's typed native-OS finding: each `required_os` entry names a
/// 1-based criterion of the task's `criteria` acceptance criteria, the OS
/// whose native host alone can produce its evidence, and that evidence.
/// Absent or empty is no finding. Admission holds only this typed field,
/// never warning prose.
pub(super) fn required_os(
    action: &str,
    task_id: &str,
    assessment: &Value,
    criteria: usize,
) -> Result<Vec<NativeOsRequirement>, DispatchError> {
    let Some(entries) = assessment
        .get("required_os")
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let invalid =
        |detail: &str| action_failed(action, format!("task {task_id} required_os {detail}"));
    let mut requirements = Vec::new();
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
        let os = entry
            .get("os")
            .and_then(Value::as_str)
            .and_then(HostOs::parse)
            .ok_or_else(|| invalid("os must be linux, macos, or windows"))?;
        entry
            .get("evidence")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|evidence| !evidence.is_empty())
            .ok_or_else(|| invalid("evidence must be a non-empty string"))?;
        let requirement = NativeOsRequirement { criterion, os };
        if !requirements.contains(&requirement) {
            requirements.push(requirement);
        }
    }
    Ok(requirements)
}

/// The one OS a validated `required_os` finding routes the task to: every
/// requirement names it. Requirements on several OSes name no single host, so
/// they route nothing and stay a hold for an operator to split.
pub(super) fn routing_os(requirements: &[NativeOsRequirement]) -> Option<HostOs> {
    let os = requirements.first()?.os;
    requirements
        .iter()
        .all(|requirement| requirement.os == os)
        .then_some(os)
}

/// The pilot's typed machine finding: each `required_machine` entry names a
/// 1-based criterion whose evidence only `owner`, the machine that owns this
/// workspace's task store, can produce, and that evidence. Absent or empty is
/// no finding. The entry must name the owner by machine id or name: a pilot
/// cannot route work to a machine this owner does not know.
pub(super) fn required_machine(
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
    let invalid =
        |detail: &str| action_failed(action, format!("task {task_id} required_machine {detail}"));
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

fn validate_optional_finding(
    action: &str,
    task_id: &str,
    assessment: &Value,
    field: &str,
    required_fields: &[&str],
) -> Result<(), DispatchError> {
    let Some(value) = assessment.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let object = value.as_object().ok_or_else(|| {
        action_failed(
            action,
            format!("task {task_id} {field} must be an object or null"),
        )
    })?;
    for required in required_fields {
        object
            .get(*required)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                action_failed(
                    action,
                    format!("task {task_id} {field}.{required} must be a non-empty string"),
                )
            })?;
    }
    Ok(())
}

/// Selectors the task's current scope declares that do not exist where the
/// assessment was validated and that no durable creation grant covers, which
/// the assessment dropped. Each finding names the existing reauthorization
/// path; nothing here infers that the target was meant to be created.
pub(super) fn unauthorized_missing_targets(
    action: &str,
    before: &[String],
    after: &[String],
    authorized_creation: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
) -> Result<Vec<String>, DispatchError> {
    let mut findings = Vec::new();
    for selector in before {
        if after.contains(selector) || authorized_creation.contains(selector) {
            continue;
        }
        let Ok(anchor) = anchor_path(selector) else {
            continue;
        };
        let missing = match source {
            Some(source) => {
                source.path_kind(action, workspace_root, &anchor)? == GitPathKind::Missing
            }
            None => !exists_in_workspace(selector, workspace_root),
        };
        if missing {
            findings.push(format!(
                "{selector} was dropped: it does not exist and carries no durable creation authorization{}",
                reauthorization_hint(selector)
            ));
        }
    }
    Ok(findings)
}

/// How an operator records that a missing selector is a target the task will
/// create. Agents' proposals, comments and plans never grant it.
fn reauthorization_hint(selector: &str) -> String {
    format!(
        "; if the task will create it, an operator must re-declare it with orbit.task.update \
         (the full context_files list including {selector}, plus allow_missing_context: true)"
    )
}

fn validate_selector_at_source(
    action: &str,
    task_id: &str,
    selector: &str,
    workspace_root: &Path,
    source: &SourceSnapshot,
    authorized_creation: bool,
) -> Result<(), DispatchError> {
    let anchor = anchor_path(selector).map_err(|error| {
        action_failed(
            action,
            format!("task {task_id} selector {selector:?} has no filesystem anchor: {error}"),
        )
    })?;
    let kind = source.path_kind(action, workspace_root, &anchor)?;
    let expected_dir = selector.starts_with("dir:");
    match (kind, expected_dir) {
        (GitPathKind::Tree, true) | (GitPathKind::Blob, false) => Ok(()),
        (GitPathKind::Missing, _) if authorized_creation => Ok(()),
        (GitPathKind::Missing, _) => Err(action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not resolve to an existing in-workspace target at source revision {} ({}){}",
                source.source_revision,
                source.source_ref,
                reauthorization_hint(selector)
            ),
        )),
        _ => Err(action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not match the target's file/directory kind at source revision {} ({})",
                source.source_revision, source.source_ref
            ),
        )),
    }
}

fn validate_selector_target_kind(
    action: &str,
    task_id: &str,
    selector: &str,
    workspace_root: &Path,
) -> Result<(), DispatchError> {
    let anchor = anchor_path(selector).map_err(|error| {
        action_failed(
            action,
            format!("task {task_id} selector {selector:?} has no filesystem anchor: {error}"),
        )
    })?;
    let resolved = workspace_root.join(anchor);
    let correct_kind = if selector.starts_with("dir:") {
        resolved.is_dir()
    } else {
        resolved.is_file()
    };
    if correct_kind {
        Ok(())
    } else {
        Err(action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not match the target's file/directory kind"
            ),
        ))
    }
}
