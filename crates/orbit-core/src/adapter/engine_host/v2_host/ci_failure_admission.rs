//! CI-sweep-specific admission policy after task-pilot validation.
//!
//! Filing and pilot inspection are intentionally separate. This module owns
//! the narrow bridge from a validated pilot assessment to a backlog admission
//! decision, including correlation with the current sweep's immutable source
//! evidence and the proposed task's generated identity.

use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

const CI_FAILURE_TAG: &str = "ci-failure-sweep";
const CI_FAILURE_KEY_TAG_PREFIX: &str = "ci-failure:";

pub(super) fn assess(
    action: &str,
    task_id: &str,
    task: &Task,
    assessment: &Value,
    selectors: &[String],
    filing: &Value,
    promotion_authorized: bool,
) -> Result<Value, DispatchError> {
    let filed_task_id = required_string(filing, "task_id", action)?;
    if filed_task_id != task_id {
        return Err(action_failed(
            action,
            format!("CI-sweep filing names task {filed_task_id}, expected {task_id}"),
        ));
    }
    let failure_key = required_string(filing, "failure_key", action)?;
    let tested_commit = required_string(filing, "tested_commit", action)?;
    let workflow = required_string(filing, "workflow", action)?;
    let job = required_string(filing, "job", action)?;
    let step = required_string(filing, "step", action)?;
    let run_urls = required_string_array(filing, "run_urls", action)?;
    if run_urls.is_empty() {
        return Err(action_failed(
            action,
            format!("CI-sweep filing for task {task_id} has no source run URLs"),
        ));
    }
    let failure_tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{failure_key}");
    if !task.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
        || !task.tags.iter().any(|tag| tag == &failure_tag)
    {
        return Err(action_failed(
            action,
            format!("task {task_id} does not match its CI-sweep filing identity"),
        ));
    }
    if task.status != TaskStatus::Proposed {
        return Err(action_failed(
            action,
            format!(
                "CI-sweep task {task_id} changed to {} before admission; refusing stale pilot output",
                task.status
            ),
        ));
    }

    let disposition = required_string(assessment, "disposition", action)?;
    let duplicate_of = assessment
        .get("duplicate_of")
        .ok_or_else(|| action_failed(action, format!("task {task_id} is missing duplicate_of")))?;
    let already_landed = assessment.get("already_landed").ok_or_else(|| {
        action_failed(action, format!("task {task_id} is missing already_landed"))
    })?;
    for (field, value) in [
        ("duplicate_of", duplicate_of),
        ("already_landed", already_landed),
    ] {
        if !value.is_null() && !recommendation_has_evidence(value) {
            return Err(action_failed(
                action,
                format!("task {task_id} {field} finding must include concrete evidence"),
            ));
        }
    }

    // `validation_tool_warnings` is the deterministic boundary's own finding
    // rather than the pilot's, but it withholds admission for the same reason
    // the others do: the repair would be admitted with an acceptance check the
    // implementation lane cannot run [ORB-11980].
    let warning_fields = [
        "blocked_by",
        "adr_conflicts",
        "utility_warnings",
        "surface_warnings",
        super::task_pilot::VALIDATION_TOOL_WARNINGS,
    ];
    let warnings = warning_fields
        .iter()
        .flat_map(|field| {
            assessment
                .get(*field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |value| json!({ "field": field, "value": value }))
        })
        .collect::<Vec<_>>();

    // A release failure may share a cluster with a pull-request run of the
    // same commit. It is release-only for remediation purposes as long as no
    // integration-head run is in that cluster.
    let ref_kinds = filing.get("ref_kinds").and_then(Value::as_array);
    let release_head_failure = ref_kinds.is_some_and(|kinds| {
        kinds.iter().any(|kind| kind.as_str() == Some("release"))
            && !kinds
                .iter()
                .any(|kind| kind.as_str() == Some("integration"))
    });
    let head_branches = filing
        .get("head_branches")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let red_failure = json!({
        "head_branches": head_branches,
        "tested_commit": tested_commit,
        "run_urls": run_urls,
    });
    let release_action = release_action_required(action, task_id, assessment)?;

    // Both release-scoped outcomes are withheld by the same owner: this sweep
    // performs no release operation, so a failure whose correct repair is
    // promotion, a tag, or a publication is reported as that operator action
    // instead of being converted into automatic repository edits.
    let (decision, classification, evidence) = if let Some(finding) = release_action {
        (
            "withhold",
            "release_publication_or_operator_action_needed",
            json!({
                "red_failure": red_failure,
                "pilot_finding": finding,
                "required_action": finding["action"],
                "automatic_action": "none",
                // Carried verbatim: a proposed repair the pilot itself ruled
                // out is evidence of what was refused, not admitted work.
                "withheld_selectors": selectors,
            }),
        )
    } else if !already_landed.is_null() && release_head_failure {
        (
            "withhold",
            "release_promotion_or_hotfix_needed",
            json!({
                "red_release": red_failure,
                "covering_repair": already_landed,
                "required_action": "promote the verified integration repair to the release branch or prepare an authorized hotfix",
                "automatic_action": "none",
            }),
        )
    } else if !already_landed.is_null() {
        ("withhold", "already_landed", already_landed.clone())
    } else if !duplicate_of.is_null() {
        ("withhold", "duplicate", duplicate_of.clone())
    } else if !warnings.is_empty() {
        ("withhold", "warnings", json!(warnings))
    } else if disposition == "verified_no_diff" {
        (
            "withhold",
            "covering_proof_missing",
            json!({
                "pilot_evidence": assessment.get("evidence").cloned().unwrap_or(Value::Null),
                "required_action": "provide concrete covering task and commit evidence or return actionable selectors",
            }),
        )
    } else if disposition != "selectors" || selectors.is_empty() {
        (
            "withhold",
            "no_actionable_selectors",
            assessment.get("evidence").cloned().unwrap_or(Value::Null),
        )
    } else if !promotion_authorized {
        (
            "withhold",
            "promotion_not_authorized",
            json!(
                "pilot results were applied, but this run carried no CI-sweep promotion authority"
            ),
        )
    } else {
        (
            "promote",
            "current_actionable_regression",
            json!(
                "pilot validated non-empty canonical selectors with no duplicate, already-landed, conflict, or warning finding"
            ),
        )
    };

    Ok(json!({
        "task_id": task_id,
        "decision": decision,
        "classification": classification,
        "promotion_authorized": promotion_authorized,
        "failure_key": failure_key,
        "source": {
            "workflow": workflow,
            "job": job,
            "step": step,
            "tested_commit": tested_commit,
            "run_urls": run_urls,
            "ref_kinds": filing.get("ref_kinds").cloned().unwrap_or_else(|| json!([])),
            "head_branches": filing.get("head_branches").cloned().unwrap_or_else(|| json!([])),
        },
        "evidence": evidence,
    }))
}

/// The pilot's explicit finding that a failure's only correct repair is an
/// operator-reserved release action — publishing or tagging a version this
/// repository already records, for example — rather than a change the
/// repository owns. Admission never infers this from which files a proposed
/// repair would touch: an ordinary packaging or dependency defect that a
/// repository edit does fix stays eligible for promotion [ORB-11517].
fn release_action_required<'a>(
    action: &str,
    task_id: &str,
    assessment: &'a Value,
) -> Result<Option<&'a Value>, DispatchError> {
    let Some(finding) = assessment
        .get("release_action_required")
        .filter(|finding| !finding.is_null())
    else {
        return Ok(None);
    };
    if !recommendation_has_evidence(finding) {
        return Err(action_failed(
            action,
            format!("task {task_id} release_action_required must include concrete evidence"),
        ));
    }
    if required_string(finding, "action", action).is_err() {
        return Err(action_failed(
            action,
            format!(
                "task {task_id} release_action_required must name the required operator action"
            ),
        ));
    }
    Ok(Some(finding))
}

fn recommendation_has_evidence(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Object(fields) => fields
            .get("evidence")
            .is_some_and(recommendation_has_evidence),
        Value::Array(values) => {
            !values.is_empty() && values.iter().all(recommendation_has_evidence)
        }
        _ => false,
    }
}

fn required_string<'a>(
    value: &'a Value,
    field: &str,
    action: &str,
) -> Result<&'a str, DispatchError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| action_failed(action, format!("{field} must be a non-empty string")))
}

fn required_string_array(
    value: &Value,
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    let values = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, format!("{field} must be an array")))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| action_failed(action, format!("{field} must contain strings")))
        })
        .collect()
}

fn action_failed(action: &str, message: impl Into<String>) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.into(),
    }
}
