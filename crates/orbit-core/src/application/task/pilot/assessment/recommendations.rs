//! Assessment readiness, recommendation validation and evidence gaps.
use super::super::input::{required_string, required_string_array, string_array_value};
use super::super::{CONTRACT_CONFLICTS, FINDING_INVALID, VALIDATION_TOOL_WARNINGS};
use orbit_engine::DispatchError;
use orbit_types::task::TaskComplexity;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Whether a validated assessment leaves the task ready for the state
/// automation to promote it on its own. Actionable selectors are necessary but
/// not sufficient: any finding that names other work, or an action outside
/// what this repository owns — a duplicate, a repair that already landed, or
/// an operator-reserved release action — keeps that decision with a human
/// [ORB-11517]. A validation criterion the implementation lane cannot satisfy
/// does the same, because promoting it admits work whose acceptance check is
/// already known to be unreachable [ORB-11980].
pub(in super::super::super) fn member_ready(assessment: &Value) -> bool {
    // Unlike the agent's own findings below, this field is injected by the
    // deterministic apply boundary, so an assessment that predates the
    // injection reads as "no finding" rather than as "not ready".
    let validation_tools_feasible = assessment
        .get(VALIDATION_TOOL_WARNINGS)
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);

    // A missing or null `contract_conflicts` on a historical receipt reads
    // as empty; any present entry holds, with no advisory opt-out.
    let no_contract_conflicts = match assessment.get(CONTRACT_CONFLICTS) {
        None | Some(Value::Null) => true,
        Some(Value::Array(findings)) => findings.is_empty(),
        Some(_) => false,
    };

    let no_start_prerequisites = match assessment.get("start_prerequisites") {
        None | Some(Value::Null) => true,
        Some(Value::Array(findings)) => findings.is_empty(),
        Some(_) => false,
    };

    validation_tools_feasible
        && no_start_prerequisites
        && no_contract_conflicts
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
                .is_some_and(|findings| {
                    findings.iter().all(|finding| {
                        !super::super::promotion::finding_holds_promotion(field, finding)
                    })
                })
        })
        && [
            "duplicate_of",
            "already_landed",
            "release_action_required",
            FINDING_INVALID,
        ]
        .iter()
        .all(|field| assessment.get(field).is_none_or(Value::is_null))
}

/// Coerce a formatting slip in `evidence_gaps` into the array the contract
/// asks for: a missing or null field becomes `[]`, and a scalar string becomes
/// a one-element array (an empty string becomes `[]`). Returns the note that
/// records the rewrite, or `None` when the field was already an array or is
/// not a shape worth guessing at (validation then rejects it as before).
pub(in super::super) fn normalize_evidence_gaps(assessment: &mut Value) -> Option<Value> {
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

pub(in super::super) fn validate_recommendations(
    action: &str,
    task_id: &str,
    assessment: &Value,
) -> Result<TaskComplexity, DispatchError> {
    required_string(assessment, "recommended_crew", action)?;
    let complexity = required_string(assessment, "recommended_complexity", action)?;
    let complexity = complexity.parse::<TaskComplexity>().map_err(|_| {
        DispatchError::action_failed(
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
        return Err(DispatchError::action_failed(
            action,
            format!("task {task_id} confidence must be high, medium, or low"),
        ));
    }
    let evidence_gaps = required_string_array(assessment, "evidence_gaps", action)?;
    required_string_array(assessment, "reassessment_triggers", action)?;
    if complexity == TaskComplexity::Unassessed && evidence_gaps.is_empty() {
        return Err(DispatchError::action_failed(
            action,
            format!("task {task_id} unassessed complexity requires actionable evidence_gaps"),
        ));
    }
    if complexity.is_assessed() && !evidence_gaps.is_empty() && confidence == "high" {
        return Err(DispatchError::action_failed(
            action,
            format!("task {task_id} with high confidence must not have evidence_gaps"),
        ));
    }
    for field in ["blocked_by", "adr_conflicts"] {
        string_array_value(
            assessment.get(field).ok_or_else(|| {
                DispatchError::action_failed(action, format!("task {task_id} is missing {field}"))
            })?,
            field,
            action,
        )?;
    }
    for field in ["utility_warnings", "surface_warnings"] {
        let findings = assessment
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!("task {task_id} {field} must be an array"),
                )
            })?;
        for finding in findings {
            if finding.is_string() {
                continue;
            }
            required_string(finding, "text", action)?;
            if finding
                .get("holds_promotion")
                .and_then(Value::as_bool)
                .is_none()
            {
                return Err(DispatchError::action_failed(
                    action,
                    format!("task {task_id} {field} finding must have a boolean holds_promotion"),
                ));
            }
        }
    }
    // Historical assessments lack this field. Every present prerequisite is
    // a typed hold, independent of advisory warnings or task IDs in its text.
    if let Some(findings) = assessment
        .get("start_prerequisites")
        .filter(|v| !v.is_null())
    {
        let findings = findings.as_array().ok_or_else(|| {
            DispatchError::action_failed(
                action,
                format!("task {task_id} start_prerequisites must be an array"),
            )
        })?;
        for finding in findings {
            if !finding.is_object() || finding.get("holds_promotion").is_some() {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} start_prerequisites entries must be objects without holds_promotion"
                    ),
                ));
            }
            required_string(finding, "evidence", action)?;
        }
    }
    // `contract_conflicts` is optional for compatibility: absent or null
    // reads as empty. A present entry is typed and always holds, so it
    // carries no `holds_promotion` opt-out.
    if let Some(findings) = assessment
        .get(CONTRACT_CONFLICTS)
        .filter(|value| !value.is_null())
    {
        let findings = findings.as_array().ok_or_else(|| {
            DispatchError::action_failed(
                action,
                format!("task {task_id} {CONTRACT_CONFLICTS} must be an array"),
            )
        })?;
        let mut seen = BTreeSet::new();
        for finding in findings {
            if finding.get("holds_promotion").is_some() {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} {CONTRACT_CONFLICTS} finding has no holds_promotion \
                         opt-out; every entry holds promotion"
                    ),
                ));
            }
            let criterion = finding
                .get("criterion")
                .and_then(Value::as_u64)
                .filter(|criterion| *criterion >= 1)
                .ok_or_else(|| {
                    DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} {CONTRACT_CONFLICTS} criterion must be a positive \
                             1-based index"
                        ),
                    )
                })?;
            if !seen.insert(criterion) {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} {CONTRACT_CONFLICTS} names criterion {criterion} twice"
                    ),
                ));
            }
            finding
                .get("evidence")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|evidence| !evidence.is_empty())
                .ok_or_else(|| {
                    DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} {CONTRACT_CONFLICTS} evidence must be a non-empty \
                             string"
                        ),
                    )
                })?;
        }
    }
    for field in ["duplicate_of", "already_landed"] {
        if assessment.get(field).is_none() {
            return Err(DispatchError::action_failed(
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
    validate_optional_finding(action, task_id, assessment, FINDING_INVALID, &["evidence"])?;
    Ok(complexity)
}

/// Validate that every `contract_conflicts` entry names a criterion of
/// this task. Shape was already checked by `validate_recommendations`;
/// absent or null is no finding.
pub(in super::super) fn contract_conflicts(
    action: &str,
    task_id: &str,
    assessment: &Value,
    criteria: usize,
) -> Result<(), DispatchError> {
    let Some(entries) = assessment
        .get(CONTRACT_CONFLICTS)
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };
    let invalid = |detail: &str| {
        DispatchError::action_failed(
            action,
            format!("task {task_id} {CONTRACT_CONFLICTS} {detail}"),
        )
    };
    for entry in entries
        .as_array()
        .ok_or_else(|| invalid("must be an array or null"))?
    {
        entry
            .get("criterion")
            .and_then(Value::as_u64)
            .and_then(|criterion| usize::try_from(criterion).ok())
            .filter(|criterion| (1..=criteria).contains(criterion))
            .ok_or_else(|| {
                invalid(&format!(
                    "criterion must be a 1-based index of the task's {criteria} acceptance criteria"
                ))
            })?;
    }
    Ok(())
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
        DispatchError::action_failed(
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
                DispatchError::action_failed(
                    action,
                    format!("task {task_id} {field}.{required} must be a non-empty string"),
                )
            })?;
    }
    Ok(())
}
