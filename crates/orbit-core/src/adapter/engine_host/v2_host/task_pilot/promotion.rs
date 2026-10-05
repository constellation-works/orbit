//! The pilot findings that withhold a `proposed` → `backlog` promotion,
//! whichever authority asked for it.
//!
//! The CI sweep and an `--approve-proposed` drain decide differently about
//! what qualifies a task, but both refuse the same evidence: a duplicate, an
//! already-landed repair, or any conflict or warning finding. Both also
//! honour the filer's `no-auto-approve` opt-out. Reading and validating those
//! findings lives here once so the two modes cannot drift.

use orbit_engine::DispatchError;
use orbit_types::task::NO_AUTO_APPROVE_TAG;
use serde_json::{Value, json};

use super::VALIDATION_TOOL_WARNINGS;
use super::input::action_failed;

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

/// Whether the task's filer opted it out of automatic approval. The tag is
/// also the hold reason, so the report names what to remove.
pub(in crate::adapter::engine_host::v2_host) fn auto_approval_opted_out(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_AUTO_APPROVE_TAG)
}

pub(in crate::adapter::engine_host::v2_host) struct PromotionFindings<'a> {
    pub(in crate::adapter::engine_host::v2_host) duplicate_of: &'a Value,
    pub(in crate::adapter::engine_host::v2_host) already_landed: &'a Value,
    /// One `{field, value}` entry per warning finding.
    pub(in crate::adapter::engine_host::v2_host) warnings: Vec<Value>,
}

/// Read the withholding findings from one validated assessment. A duplicate
/// or already-landed finding without concrete evidence is malformed output,
/// not a clean result.
pub(in crate::adapter::engine_host::v2_host) fn promotion_findings<'a>(
    action: &str,
    task_id: &str,
    assessment: &'a Value,
) -> Result<PromotionFindings<'a>, DispatchError> {
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
    let warnings = WARNING_FIELDS
        .iter()
        .flat_map(|field| {
            assessment
                .get(*field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |value| json!({ "field": field, "value": value }))
        })
        .collect();
    Ok(PromotionFindings {
        duplicate_of,
        already_landed,
        warnings,
    })
}

pub(in crate::adapter::engine_host::v2_host) fn recommendation_has_evidence(value: &Value) -> bool {
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
