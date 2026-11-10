//! Hold assessments whose source evidence leaves their lock footprint unresolved.
use super::super::promotion::{FOOTPRINT_UNRESOLVED, text_declares_unresolved_footprint};
use orbit_types::task::TaskComplexity;
use serde_json::{Value, json};

/// Hold a task whose pilot could not establish its lock footprint, so the hold
/// is independent of its tier and selectors [ORB-22153]. An `unassessed`
/// recommendation that kept an assessed tier gets one `utility_warnings`
/// finding naming the unread evidence. An assessed recommendation whose
/// pilot declared the same unread source as a holding `utility_warnings`
/// finding gets the marker stamped on that finding, so the hold is not
/// released merely because the unread source names a done or dependency
/// task — whether that task was already done or a dependency at apply time
/// or finishes only later [ORB-22178]. Any footprint finding already in the
/// assessment is dropped first, so a pilot cannot supply the marker and a
/// re-check cannot hold the task twice.
pub(super) fn hold_unresolved_footprint(
    assessment: &mut Value,
    recommended: TaskComplexity,
    applied: TaskComplexity,
) {
    let gaps_list = assessment
        .get("evidence_gaps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let gaps = gaps_list.join("; ");
    let kept = recommended == TaskComplexity::Unassessed && applied.is_assessed();
    let Some(warnings) = assessment
        .get_mut("utility_warnings")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    warnings.retain(|finding| {
        finding.get("hold").and_then(Value::as_str) != Some(FOOTPRINT_UNRESOLVED)
    });
    if recommended.is_assessed() {
        // Stamp every pilot-declared unread-source finding, not only one
        // whose named task is already a dependency or done at apply time:
        // a predecessor that finishes later must not release the hold while
        // the evidence remains unread [ORB-22178].
        for finding in warnings.iter_mut() {
            // `finding.text` only. Promotion passes `warning_text` instead.
            if text_declares_unresolved_footprint(
                finding.get("holds_promotion").and_then(Value::as_bool) == Some(true),
                finding.get("text").and_then(Value::as_str),
                &gaps_list,
            ) && let Some(object) = finding.as_object_mut()
            {
                object.insert("hold".to_string(), json!(FOOTPRINT_UNRESOLVED));
            }
        }
    }
    if kept {
        warnings.push(json!({
            "text": format!(
                "Footprint unresolved: the pilot could not establish the task's lock footprint, \
                 so it returned unassessed. The task keeps its assessed {applied} tier and pool. \
                 Unresolved: {gaps}. Make the named evidence readable or re-scope the task, then \
                 request a new pilot assessment. A consulted reference document is not a \
                 verified footprint."
            ),
            "holds_promotion": true,
            "hold": FOOTPRINT_UNRESOLVED,
        }));
    }
}
