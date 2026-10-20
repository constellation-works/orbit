//! Read-only explanation of the current block, independent of recovery eligibility.

use orbit_types::task::{RecordedFinalRecovery, TaskDecision, TaskHistoryEntry, TaskStatus};
use serde_json::{Value, json};

use crate::application::codebase::{BlockCause, classify_block};
use crate::application::task::TaskRow;

use super::super::helpers::SYSTEM_ACTOR_LABEL;

pub(super) fn is_escalation(entry: &TaskHistoryEntry) -> bool {
    entry.event == "final_recovery_escalated"
        || (entry.by == SYSTEM_ACTOR_LABEL
            && matches!(
                entry.decision,
                Some(TaskDecision::FinalRecovery {
                    outcome: RecordedFinalRecovery::Escalated { .. },
                    ..
                })
            ))
}

/// An escalation can refine an existing block without a status transition.
/// Only look within the current episode, so reblocking never revives old advice.
pub(super) fn current_block(history: &[TaskHistoryEntry]) -> Option<&TaskHistoryEntry> {
    let start = history
        .iter()
        .rposition(|entry| entry.to_status == Some(TaskStatus::Blocked))?;
    history[start..]
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Blocked) || is_escalation(entry))
}

pub(super) fn blocking_run_id(entry: &TaskHistoryEntry) -> Option<&str> {
    if let Some(TaskDecision::FinalRecovery { run_id, .. }) = &entry.decision {
        return Some(run_id);
    }
    let rest = entry.note.as_deref()?.split_once("run_id=")?.1;
    let id = rest.split([',', ' ', ';', ')']).next()?;
    (!id.is_empty()).then_some(id)
}

/// Additive show-only projection. No history or diagnosis parsing belongs in
/// the browser, and no read here changes the backstop's eligible episodes.
pub(in crate::application::task) fn block_json(row: &TaskRow) -> Option<Value> {
    if row.task.status != TaskStatus::Blocked {
        return None;
    }
    let entry = current_block(&row.history);
    let run_id = entry.and_then(blocking_run_id);
    let escalation = entry.is_some_and(is_escalation);
    // The applier stamps the decision and its full comment together. Matching
    // both timestamp and run avoids a newer, unrelated comment replacing it.
    let comment = entry.filter(|_| escalation).and_then(|entry| {
        row.comments.iter().enumerate().rev().find(|(_, comment)| {
            comment.by == SYSTEM_ACTOR_LABEL
                && comment.at == entry.at
                && comment.message.lines().next().is_some_and(|header| {
                    header.starts_with("final_recovery ")
                        && header
                            .split_whitespace()
                            .any(|field| field == "outcome=escalated")
                        && run_id.is_some_and(|id| {
                            header
                                .split_whitespace()
                                .any(|field| field.strip_prefix("run_id=") == Some(id))
                        })
                })
        })
    });
    let detail = comment.and_then(|(_, comment)| {
        comment
            .message
            .split_once("\ndiagnosis: ")
            .map(|(_, detail)| detail)
    });
    let (diagnosis, next_action) = detail
        .map(|detail| {
            detail
                .split_once("\nhuman_action: ")
                .map_or((detail, None), |(reason, action)| (reason, Some(action)))
        })
        .unwrap_or(("", None));
    let note = entry
        .and_then(|entry| entry.note.as_deref())
        .unwrap_or_default();
    let reason = if !diagnosis.is_empty() {
        diagnosis
    } else if escalation {
        note.split_once(" escalated: ")
            .map_or("", |(_, reason)| reason)
    } else {
        note.split_once(", error=")
            .map_or(note, |(_, reason)| reason)
    };
    let reason = reason.trim();
    let cause = entry.map(|entry| classify_block(entry, &[]));
    let cause = match cause {
        Some(BlockCause::Infra(_)) => "infra",
        Some(BlockCause::Run) if escalation => "escalation",
        Some(BlockCause::Run) => "run",
        Some(BlockCause::BaseConflict(_)) => "base_conflict",
        Some(BlockCause::Manual(_)) => "manual",
        Some(BlockCause::NotFailure) => "hold",
        Some(BlockCause::Unclassified) | None => "unknown",
    };
    Some(json!({
        "at": entry.map(|entry| entry.at),
        "cause": cause,
        "reason": (!reason.is_empty() && reason != "-").then_some(reason),
        "run_id": run_id,
        "comment_index": comment.map(|(index, _)| index),
        "next_action": next_action.map(str::trim).filter(|action| !action.is_empty()),
    }))
}
