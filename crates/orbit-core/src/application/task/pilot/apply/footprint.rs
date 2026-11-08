//! Hold assessments whose source evidence leaves their lock footprint unresolved.
use super::super::promotion::FOOTPRINT_UNRESOLVED;
use orbit_types::task::{TaskComplexity, is_valid_orb_task_id};
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
            if pilot_declares_unresolved_footprint(finding, &gaps_list)
                && let Some(object) = finding.as_object_mut()
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

/// Whole task ids named in `text`, under any valid task-id prefix.
fn named_task_ids(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_')))
        .filter(|token| is_valid_orb_task_id(token))
        .map(str::to_string)
        .collect()
}

/// Whether an assessed pilot's holding utility finding declares an unread
/// footprint source: it explicitly holds promotion and, with `evidence_gaps`
/// present, either describes the source as unread or names a task the gaps
/// also name as the unread source. The word "footprint" alone is not
/// evidence: "ORB-1 must land before adjusting the footprint" declares
/// nothing unread.
fn pilot_declares_unresolved_footprint(finding: &Value, gaps: &[String]) -> bool {
    if finding.get("holds_promotion").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let Some(text) = finding.get("text").and_then(Value::as_str) else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    if gaps.is_empty() {
        return false;
    }
    if lower.contains("unread") {
        return true;
    }
    // A task id the gaps also name ties the finding to the gap's source.
    let gaps_text = gaps.join(" ");
    named_task_ids(text)
        .iter()
        .any(|id| gaps_text.contains(id.as_str()))
}
