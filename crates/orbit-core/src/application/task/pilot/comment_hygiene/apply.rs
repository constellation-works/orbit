//! Promotion-side checks of a prepared batch: selector validation against the
//! pilot's proposal and the host-owned hold attached to its assessment.

use orbit_engine::DispatchError;
use serde_json::{Value, json};

use super::super::promotion::FOOTPRINT_UNRESOLVED;
use super::{LINE_BUDGET, MAX_BATCH_FILES, OVERSIZED_LINE_THRESHOLD, comment_hygiene_batch_holds};

/// Validate a pilot's proposed disposition and selectors against the host's
/// deterministic `comment_hygiene_batch`.
pub(in crate::application::task::pilot) fn validate_comment_hygiene_selectors(
    action: &str,
    task_id: &str,
    batch: &Value,
    disposition: &str,
    selectors: &[String],
) -> Result<(), DispatchError> {
    let status = batch
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("hold");
    match status {
        "hold" => {
            if !selectors.is_empty() {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} comment-hygiene preparation is held ({}); provisional \
                         selectors must not be published — return empty context_files_after with \
                         disposition \"selectors\", evidence_gaps and a holding utility_warnings \
                         finding",
                        hold_reason_summary(batch)
                    ),
                ));
            }
            if disposition != "selectors" {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} comment-hygiene preparation is held ({}); cannot settle as \
                         {disposition} — return disposition \"selectors\" with empty \
                         context_files_after, evidence_gaps and a holding utility_warnings finding",
                        hold_reason_summary(batch)
                    ),
                ));
            }
            Ok(())
        }
        "empty" => {
            if !selectors.is_empty() || disposition != "verified_no_diff" {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} comment-hygiene preparation found 0 eligible candidates; \
                         return disposition \"verified_no_diff\" with empty context_files_after"
                    ),
                ));
            }
            Ok(())
        }
        "ready" => {
            if disposition != "selectors" {
                return Ok(());
            }
            let Some(ranked) = batch.get("ranked_candidates").and_then(Value::as_array) else {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} prepared comment_hygiene_batch is missing ranked_candidates"
                    ),
                ));
            };
            if selectors.len() > MAX_BATCH_FILES {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} comment-hygiene batch selects {} files, exceeding the cap \
                         of {MAX_BATCH_FILES}",
                        selectors.len()
                    ),
                ));
            }
            let mut last_rank: Option<usize> = None;
            let mut total_lines = 0usize;
            for (position, selector) in selectors.iter().enumerate() {
                let Some((rank, entry)) = ranked.iter().enumerate().find(|(_, item)| {
                    item.get("selector").and_then(Value::as_str) == Some(selector.as_str())
                }) else {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} comment-hygiene selector {selector:?} is not in the \
                             host-verified oldest-first ranked_candidates"
                        ),
                    ));
                };
                if let Some(previous) = last_rank
                    && rank <= previous
                {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} comment-hygiene selector {selector:?} violates the \
                             host-verified oldest-first order"
                        ),
                    ));
                }
                last_rank = Some(rank);
                let lines = entry
                    .get("lines")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0);
                let oversized = lines > OVERSIZED_LINE_THRESHOLD;
                if position == 0 && oversized && selectors.len() > 1 {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} oversized comment-hygiene file {selector:?} ({lines} \
                             lines > {OVERSIZED_LINE_THRESHOLD}) must be selected alone"
                        ),
                    ));
                }
                if position > 0 && oversized {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} oversized comment-hygiene file {selector:?} ({lines} \
                             lines > {OVERSIZED_LINE_THRESHOLD}) cannot join a multi-file batch"
                        ),
                    ));
                }
                total_lines = total_lines.saturating_add(lines);
                if position > 0 && total_lines > LINE_BUDGET {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} comment-hygiene batch reaches {total_lines} lines, \
                             exceeding the {LINE_BUDGET}-line budget"
                        ),
                    ));
                }
            }
            Ok(())
        }
        other => Err(DispatchError::action_failed(
            action,
            format!("task {task_id} prepared comment_hygiene_batch has unknown status {other:?}"),
        )),
    }
}

/// Deterministically attach a host-owned [`FOOTPRINT_UNRESOLVED`] promotion
/// hold when `batch` has `status == "hold"`, so missing required ledger or
/// footprint evidence always withholds promotion regardless of pilot wording.
pub(in crate::application::task::pilot) fn attach_comment_hygiene_hold(
    assessment: &mut Value,
    batch: &Value,
) {
    if !comment_hygiene_batch_holds(batch) {
        return;
    }
    let Some(fields) = assessment.as_object_mut() else {
        return;
    };
    let summary = hold_reason_summary(batch);
    let reasons = batch
        .get("hold_reasons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if fields.get("confidence").and_then(Value::as_str) == Some("high") {
        fields.insert("confidence".to_string(), json!("low"));
    }

    let gaps = fields
        .entry("evidence_gaps".to_string())
        .or_insert_with(|| json!([]));
    if let Some(gap_list) = gaps.as_array_mut() {
        for reason in &reasons {
            let source = reason
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let detail = reason
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("missing required evidence");
            let entry = format!("{source}: {detail}");
            let already_named = gap_list
                .iter()
                .filter_map(Value::as_str)
                .any(|existing| existing.contains(source));
            if !already_named {
                gap_list.push(json!(entry));
            }
        }
        if gap_list.is_empty() {
            gap_list.push(json!(summary.clone()));
        }
    }

    let warnings = fields
        .entry("utility_warnings".to_string())
        .or_insert_with(|| json!([]));
    if let Some(warning_list) = warnings.as_array_mut() {
        warning_list.retain(|finding| {
            finding.get("hold").and_then(Value::as_str) != Some(FOOTPRINT_UNRESOLVED)
        });
        warning_list.push(json!({
            "text": format!(
                "Footprint unresolved: deterministic comment-hygiene batch preparation held \
                 because required evidence is missing or unreadable ({summary}). No provisional \
                 selectors were published. Restore or repair the named evidence and re-pilot."
            ),
            "holds_promotion": true,
            "hold": FOOTPRINT_UNRESOLVED,
        }));
    }
}

fn hold_reason_summary(batch: &Value) -> String {
    let reasons = batch
        .get("hold_reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reason| {
            let code = reason.get("code").and_then(Value::as_str)?;
            let source = reason
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let detail = reason
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(if source.is_empty() {
                format!("{code}: {detail}")
            } else {
                format!("{code} ({source}): {detail}")
            })
        })
        .collect::<Vec<_>>();
    if reasons.is_empty() {
        "required ledger or footprint evidence unavailable".to_string()
    } else {
        reasons.join("; ")
    }
}
