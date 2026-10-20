//! The pilot findings that withhold a `proposed` → `backlog` promotion,
//! whichever authority asked for it.
//!
//! The CI sweep and an `--approve-proposed` drain decide differently about
//! what qualifies a task, but both refuse the same evidence: a duplicate, an
//! already-landed repair, or any conflict or holding warning finding. Both also
//! honour the filer's `no-auto-approve` opt-out. Reading and validating those
//! findings lives here once so the two modes cannot drift.

use orbit_engine::DispatchError;
use orbit_types::task::NO_AUTO_APPROVE_TAG;
use orbit_types::workflow::DrainHoldFinding;
use serde_json::{Value, json};

use super::VALIDATION_TOOL_WARNINGS;

/// Warning-shaped findings. `validation_tool_warnings` is the deterministic
/// boundary's own finding rather than the pilot's, but it withholds promotion
/// for the same reason the others do: the task would be admitted with an
/// acceptance check the implementation lane cannot run [ORB-11980].
const WARNING_FIELDS: [&str; 5] = [
    "blocked_by",
    "adr_conflicts",
    "utility_warnings",
    "surface_warnings",
    VALIDATION_TOOL_WARNINGS,
];

/// Only surface and utility findings may explicitly be advisory. Legacy
/// strings and malformed findings hold by default; prerequisite, conflict and
/// deterministic validation findings cannot opt out through this flag.
pub(super) fn finding_holds_promotion(field: &str, finding: &Value) -> bool {
    !(matches!(field, "surface_warnings" | "utility_warnings")
        && finding.get("holds_promotion").and_then(Value::as_bool) == Some(false)
        && finding
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty()))
}

/// Plain or structured finding text, shared with warning normalization.
pub(super) fn warning_text(finding: &Value) -> Option<&str> {
    finding
        .as_str()
        .or_else(|| finding.get("text").and_then(Value::as_str))
}

/// Whether the task's filer opted it out of automatic approval. The tag is
/// also the hold reason, so the report names what to remove.
pub(crate) fn auto_approval_opted_out(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_AUTO_APPROVE_TAG)
}

pub(crate) struct PromotionFindings<'a> {
    pub(crate) duplicate_of: &'a Value,
    pub(crate) already_landed: &'a Value,
    /// One `{field, value}` entry per warning finding.
    pub(crate) warnings: Vec<Value>,
}

/// Read the withholding findings from one validated assessment. A duplicate
/// or already-landed finding without concrete evidence is malformed output,
/// not a clean result.
pub(crate) fn promotion_findings<'a>(
    action: &str,
    task_id: &str,
    assessment: &'a Value,
) -> Result<PromotionFindings<'a>, DispatchError> {
    let duplicate_of = assessment.get("duplicate_of").ok_or_else(|| {
        DispatchError::action_failed(action, format!("task {task_id} is missing duplicate_of"))
    })?;
    let already_landed = assessment.get("already_landed").ok_or_else(|| {
        DispatchError::action_failed(action, format!("task {task_id} is missing already_landed"))
    })?;
    for (field, value) in [
        ("duplicate_of", duplicate_of),
        ("already_landed", already_landed),
    ] {
        if !value.is_null() && !recommendation_has_evidence(value) {
            return Err(DispatchError::action_failed(
                action,
                format!("task {task_id} {field} finding must include concrete evidence"),
            ));
        }
    }
    let warnings = WARNING_FIELDS
        .iter()
        .flat_map(|field| {
            assessment
                .get(*field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(move |value| finding_holds_promotion(field, value))
                .map(move |value| json!({ "field": field, "value": value }))
        })
        .collect();
    Ok(PromotionFindings {
        duplicate_of,
        already_landed,
        warnings,
    })
}

/// The warning findings an approval hold can show: one entry per warning-field
/// item, kind equal to the field and text equal to its first non-empty line.
pub(crate) fn warning_hold_findings(assessment: &Value) -> Vec<DrainHoldFinding> {
    WARNING_FIELDS
        .iter()
        .flat_map(|field| {
            assessment
                .get(*field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(move |value| finding_holds_promotion(field, value))
                .filter_map(|value| {
                    finding_text(value).map(|text| DrainHoldFinding {
                        kind: (*field).to_string(),
                        text,
                    })
                })
        })
        .collect()
}

/// First non-empty line of a plain or structured warning item. Unrecognized
/// shapes use compact JSON so even a malformed historical finding is named.
fn finding_text(value: &Value) -> Option<String> {
    let raw = warning_text(value)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string());
    let text = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_string();
    (!text.is_empty()).then_some(text)
}

pub(crate) fn recommendation_has_evidence(value: &Value) -> bool {
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
