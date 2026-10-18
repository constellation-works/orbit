//! Mid-run steering's delivery: operator comments written after a run's
//! prompt snapshot reach that run's next task-context prompt as one bounded
//! steering block (`docs/design/mid-run-steering/2_design.md` §2).
//!
//! A run's first task-context prompt for a task is its snapshot: the host
//! records the newest comment it carried as the run's watermark. Every later
//! prompt the run builds for the task — a recovery attempt, a repair round,
//! final recovery, a re-dispatch — is a safe point. There the host re-reads
//! the comment log, and operator comments the run has not yet delivered form
//! the block, oldest first. Building that prompt is the delivery: the host
//! records a [`STEERING_DELIVERED_EVENT`] on the task and advances the
//! watermark past what the block carried in full. Agent output never reaches
//! either record.

use chrono::SecondsFormat;
use orbit_common::OrbitError;
use orbit_common::text::{ceil_char_boundary, floor_char_boundary};
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry, refuses_implementer_writes};
use orbit_types::workflow::{CommentLogMark, PipelineState, SteeringWatermark};
use serde_json::{Value, json};

use super::SYSTEM_ACTOR_LABEL;
use super::helpers::is_automation_actor;
use crate::OrbitRuntime;

/// Task history event recording that a prompt carried a steering block. Its
/// note names the run, the step, the comments it carried in full
/// (`delivered`) and those it could only cut down (`truncated`).
pub(crate) const STEERING_DELIVERED_EVENT: &str = "steering_delivered";

/// Largest steering block text, in bytes. A long comment thread must not
/// evict the task itself from the prompt; past this the block keeps its head
/// and tail around a marker, as recovery diagnostics do [ORB-12467].
const MAX_STEERING_BLOCK_BYTES: usize = 32 * 1024;

const STEERING_LABEL: &str = "Operator guidance received after this run was dispatched, oldest \
     first. It amends the task below with the same standing as its description; weigh it \
     before you continue.";

/// One pending operator comment, rendered for the block.
struct PendingComment {
    mark: CommentLogMark,
    text: String,
}

impl OrbitRuntime {
    /// Record `run_id`'s steering watermark for the task-context prompt
    /// `step_id` builds for `task` from `comments`, and return the steering
    /// block that prompt carries ahead of the task, if any.
    ///
    /// Without a watermark this is the run's snapshot: the newest comment is
    /// recorded and no block is built. With one, operator comments past it
    /// that no earlier block carried in full are delivered. A safe point with
    /// none builds the prompt as the snapshot does. A task that refuses
    /// implementer writes is not steered, since its step is stopping, and
    /// neither is a claimed leaf; their comments stay undelivered.
    pub(crate) fn steer_task_prompt(
        &self,
        run_id: &str,
        step_id: &str,
        task: &Task,
        comments: &[TaskComment],
    ) -> Result<Option<Value>, OrbitError> {
        let watermark = self
            .stores()
            .jobs()
            .read_run_state(run_id)?
            .and_then(|mut state| state.steering_watermarks.remove(&task.id));
        let pending = watermark
            .as_ref()
            .map(|watermark| pending_operator_comments(watermark, comments))
            .unwrap_or_default();
        let Some(watermark) = watermark.filter(|_| !pending.is_empty()) else {
            let newest = newest_mark(comments);
            self.update_steering_watermark(run_id, |state| {
                state.record_steering_snapshot(&task.id, step_id, newest);
            })?;
            return Ok(None);
        };
        // A stopping step gets no steering, and a claimed leaf's task record
        // is the owner's, so delivery there waits for owner-routed steering.
        // Either way the comments stay undelivered.
        if refuses_implementer_writes(task.status) || self.worker_invocation().is_some() {
            return Ok(None);
        }

        let (text, carried) = bounded_steering_text(&task.id, &pending);
        let (delivered, truncated): (Vec<_>, Vec<_>) = pending
            .iter()
            .zip(&carried)
            .partition(|(_, carried)| **carried);
        let delivered: Vec<CommentLogMark> = delivered.into_iter().map(|(c, _)| c.mark).collect();
        let truncated: Vec<CommentLogMark> = truncated.into_iter().map(|(c, _)| c.mark).collect();
        // The watermark is a prefix of the log: it stops below the oldest
        // comment the block only cut down.
        let through = match truncated.first() {
            Some(oldest) => oldest
                .position
                .checked_sub(1)
                .and_then(|position| comment_mark(comments, usize::try_from(position).ok()?)),
            None => newest_mark(comments),
        };

        // The event goes first: if the watermark write then fails, the prompt
        // is never sent and the next safe point delivers again, rather than a
        // recorded watermark hiding a comment no prompt carried.
        let note = json!({
            "run_id": run_id,
            "step_id": step_id,
            "delivered": delivered,
            "truncated": truncated,
        });
        self.append_system_history(
            &task.id,
            vec![TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: SYSTEM_ACTOR_LABEL.to_string(),
                event: STEERING_DELIVERED_EVENT.to_string(),
                note: Some(note.to_string()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
        )?;
        self.update_steering_watermark(run_id, |state| {
            state.record_steering_delivery(&task.id, step_id, through, &delivered);
        })?;

        Ok(Some(json!({
            "label": STEERING_LABEL,
            "run_id": run_id,
            "snapshot": watermark.comment,
            "comments": text,
        })))
    }

    fn update_steering_watermark(
        &self,
        run_id: &str,
        mut apply: impl FnMut(&mut PipelineState),
    ) -> Result<(), OrbitError> {
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                apply(state);
                Ok(())
            })
            .map(|_| ())
    }
}

/// Operator comments `watermark` neither covers nor saw delivered, oldest
/// first. Comments Orbit's own automation wrote are never steering.
fn pending_operator_comments(
    watermark: &SteeringWatermark,
    comments: &[TaskComment],
) -> Vec<PendingComment> {
    comments
        .iter()
        .enumerate()
        .filter_map(|(position, comment)| {
            let mark = comment_mark(comments, position)?;
            (!watermark.covers(mark.position) && !is_automation_actor(&comment.by)).then(|| {
                PendingComment {
                    mark,
                    text: format!(
                        "[comment {} by {} at {}]\n{}",
                        mark.position,
                        comment.by,
                        comment.at.to_rfc3339_opts(SecondsFormat::Secs, true),
                        comment.message
                    ),
                }
            })
        })
        .collect()
}

fn newest_mark(comments: &[TaskComment]) -> Option<CommentLogMark> {
    comment_mark(comments, comments.len().checked_sub(1)?)
}

fn comment_mark(comments: &[TaskComment], position: usize) -> Option<CommentLogMark> {
    let comment = comments.get(position)?;
    Some(CommentLogMark {
        position: u64::try_from(position).ok()?,
        at: comment.at,
    })
}

/// The block text for `pending`, and for each comment whether the text
/// carries it in full. Past [`MAX_STEERING_BLOCK_BYTES`] the middle is cut
/// and a marker names the full size and where to read every comment.
fn bounded_steering_text(task_id: &str, pending: &[PendingComment]) -> (String, Vec<bool>) {
    let mut text = String::new();
    let mut spans = Vec::with_capacity(pending.len());
    for comment in pending {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let start = text.len();
        text.push_str(&comment.text);
        spans.push((start, text.len()));
    }
    if text.len() <= MAX_STEERING_BLOCK_BYTES {
        return (text, vec![true; pending.len()]);
    }
    let marker = format!(
        "\n… [truncated: these comments are {} B; the middle is omitted. Read every comment in \
         full with `orbit task show {task_id}`] …\n",
        text.len()
    );
    let budget = MAX_STEERING_BLOCK_BYTES.saturating_sub(marker.len());
    let head_budget = budget * 3 / 4;
    let head_end = floor_char_boundary(&text, head_budget);
    let tail_start = ceil_char_boundary(&text, text.len() - (budget - head_budget));
    let carried = spans
        .iter()
        .map(|(start, end)| *end <= head_end || *start >= tail_start)
        .collect();
    (
        format!("{}{marker}{}", &text[..head_end], &text[tail_start..]),
        carried,
    )
}
