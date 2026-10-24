//! The pilot findings that withhold a `proposed` → `backlog` promotion,
//! whichever authority asked for it.
//!
//! The CI sweep and an `--approve-proposed` drain decide differently about
//! what qualifies a task, but both refuse the same evidence: a duplicate, an
//! already-landed repair, or any conflict or holding warning finding. Both also
//! honour the filer's `no-auto-approve` opt-out. Reading and validating those
//! findings lives here once so the two modes cannot drift.

use orbit_engine::DispatchError;
use orbit_types::task::{NO_AUTO_APPROVE_TAG, TaskComment, TaskStatus, is_valid_orb_task_id};
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

fn task_id_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_')
}

/// Whole task IDs named in one `blocked_by` finding's text.
fn blocked_by_task_ids(finding: &Value) -> Vec<String> {
    let Some(text) = warning_text(finding) else {
        return Vec::new();
    };
    text.split(|character| !task_id_character(character))
        .filter(|id| id.starts_with("ORB-") && is_valid_orb_task_id(id))
        .map(str::to_string)
        .collect()
}

/// Whether one `blocked_by` finding withholds promotion. A prerequisite
/// already enforced as a dependency edge (or already `done` in this
/// workspace) holds nothing: the dependency graph gates admission instead.
/// Prose naming no task, a missing or other-workspace task, or an unfinished
/// task that is not a recorded dependency still holds, because no edge
/// enforces it.
pub(super) fn blocked_by_holds_promotion(
    finding: &Value,
    dependencies: &[String],
    is_done: &dyn Fn(&str) -> bool,
) -> bool {
    let ids = blocked_by_task_ids(finding);
    ids.is_empty()
        || ids
            .iter()
            .any(|id| !dependencies.iter().any(|dep| dep == id) && !is_done(id))
}

/// Whether `field`'s `finding` withholds promotion, with `blocked_by`
/// decided against the task's current `dependencies` and done lookup.
fn holds_promotion(
    field: &str,
    finding: &Value,
    dependencies: &[String],
    is_done: &dyn Fn(&str) -> bool,
) -> bool {
    if field == "blocked_by" {
        blocked_by_holds_promotion(finding, dependencies, is_done)
    } else {
        finding_holds_promotion(field, finding)
    }
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
    dependencies: &[String],
    is_done: &dyn Fn(&str) -> bool,
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
                .filter(move |value| holds_promotion(field, value, dependencies, is_done))
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
/// item, with the original kind and full text.
pub(crate) fn warning_hold_findings(
    assessment: &Value,
    dependencies: &[String],
    is_done: &dyn Fn(&str) -> bool,
) -> Vec<DrainHoldFinding> {
    WARNING_FIELDS
        .iter()
        .flat_map(|field| {
            assessment
                .get(*field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(move |value| holds_promotion(field, value, dependencies, is_done))
                .filter_map(|value| {
                    finding_text(value).map(|text| DrainHoldFinding {
                        kind: (*field).to_string(),
                        text,
                    })
                })
        })
        .collect()
}

/// Full text of a plain or structured warning item. Unrecognized
/// shapes use compact JSON so even a malformed historical finding is named.
fn finding_text(value: &Value) -> Option<String> {
    let raw = warning_text(value)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string());
    let text = raw.trim().to_string();
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

/// Holding warnings from the task's latest durable pilot verification.
/// Ordinary edits and comments preserve the last findings until re-piloted;
/// a clean verification or leaving proposed clears the readout. No live
/// drain or drain-pass snapshot is needed, and advisory findings are excluded.
pub fn task_pilot_warnings(
    status: TaskStatus,
    comments: &[TaskComment],
    dependencies: &[String],
    is_done: &dyn Fn(&str) -> bool,
) -> Vec<DrainHoldFinding> {
    held_pilot_warnings(status, comments, dependencies, is_done)
        .map(|held| held.findings)
        .unwrap_or_default()
}

/// The assessment whose warning findings hold a `proposed` task's promotion:
/// the receipt that persisted it, the pilot run that wrote it, and the
/// findings that hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldPilotWarnings {
    /// Operation id of the task-pilot receipt carrying the assessment.
    pub(crate) operation_id: String,
    /// The task-pilot run that wrote the assessment, when the audit names it.
    pub(crate) pilot_run_id: Option<String>,
    /// Every holding finding, never empty.
    pub(crate) findings: Vec<DrainHoldFinding>,
}

/// [`task_pilot_warnings`] with the assessment it read: `None` unless the
/// task is `proposed` and its latest pilot receipt holds promotion on at
/// least one warning finding.
pub(crate) fn held_pilot_warnings(
    status: TaskStatus,
    comments: &[TaskComment],
    dependencies: &[String],
    is_done: &dyn Fn(&str) -> bool,
) -> Option<HeldPilotWarnings> {
    if status != TaskStatus::Proposed {
        return None;
    }
    let (_, receipt) = super::pilot_receipts(comments).next()?;
    let audit = serde_json::from_str::<Value>(receipt.body).ok()?;
    let findings = warning_hold_findings(audit.get("assessment")?, dependencies, is_done);
    (!findings.is_empty()).then(|| HeldPilotWarnings {
        operation_id: receipt.operation_id.to_string(),
        pilot_run_id: audit
            .get("pilot_run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|run_id| !run_id.is_empty())
            .map(str::to_string),
        findings,
    })
}
