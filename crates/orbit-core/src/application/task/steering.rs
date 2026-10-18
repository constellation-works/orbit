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
//!
//! The promotion gate reads the same records (§3): before `commit`, an
//! operator comment past the run's watermark that no block carried in full
//! is outstanding. Each refusal spends one steering round, recorded on the
//! run before its agent step is dispatched again.
//!
//! A claimed leaf's task record is its owner's, so its watermark is too: the
//! leaf reads the comment log, its claim's watermark and the baseline its
//! predecessor claim left from the owner, and records each snapshot and
//! delivery there through its claim, where settlement keeps them. A comment
//! only the follower's replica holds is never read, and an owner the leaf
//! cannot read fails the prompt or the gate rather than certifying a stale
//! log. Only its steering rounds, the run's own budget, stay in the leaf's
//! run state.

use std::collections::BTreeSet;

use chrono::SecondsFormat;
use orbit_common::OrbitError;
use orbit_common::text::{ceil_char_boundary, floor_char_boundary};
use orbit_engine::{SteeringGateReading, SteeringRoundAdmission};
use orbit_store::contracts::{ClaimSteeringReading, ClaimSteeringRecord};
use orbit_types::task::{
    Task, TaskComment, TaskHistoryEntry, TaskStatus, refuses_implementer_writes,
};
use orbit_types::workflow::{
    CommentLogMark, PipelineState, STEERING_DELIVERED_EVENT, SteeringRound, SteeringWatermark,
};
use serde_json::{Value, json};

use super::SYSTEM_ACTOR_LABEL;
use super::helpers::is_automation_actor;
use crate::OrbitRuntime;

/// Largest steering block text, in bytes. A long comment thread must not
/// evict the task itself from the prompt; past this the block keeps its head
/// and tail around a marker, as recovery diagnostics do [ORB-12467].
const MAX_STEERING_BLOCK_BYTES: usize = 32 * 1024;

/// Resume-lineage ancestors the gate walks for a watermark. If the bound
/// leaves older ancestors unresolved, the gate cannot safely establish a
/// baseline and fails closed.
const MAX_LINEAGE_HOPS: usize = 32;

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
    /// Without a watermark this is the run's snapshot, unless the promotion
    /// gate admitted a steering round: that first prompt delivers from the
    /// inherited baseline instead of covering comments its bounded task
    /// context may omit. With a watermark, operator comments past it
    /// that no earlier block carried in full are delivered. A safe point with
    /// none builds the prompt as the snapshot does. A task that refuses
    /// implementer writes is not steered, since its step is stopping; its
    /// comments stay undelivered. A claimed leaf reads and records through
    /// its owner, so an owner it cannot reach fails the prompt.
    pub(crate) fn steer_task_prompt(
        &self,
        run_id: &str,
        step_id: &str,
        task: &Task,
        comments: &[TaskComment],
    ) -> Result<Option<Value>, OrbitError> {
        let state = self.stores().jobs().read_run_state(run_id)?;
        let mut watermark = self.steering_watermark(&task.id, state.as_ref())?;
        let pending = if let Some(watermark) = &watermark {
            pending_operator_comments(watermark, comments)
        } else if state.as_ref().is_some_and(|state| {
            state
                .steering_rounds
                .get(&task.id)
                .is_some_and(|rounds| !rounds.is_empty())
        }) {
            // A carried candidate or checkpointed resume can first build a
            // prompt in a steering round. Its ordinary task context is
            // bounded, so taking a fresh snapshot here could cover comments
            // that context omitted. Deliver from the gate's inherited
            // baseline instead, including comments since the gate read.
            let outstanding: BTreeSet<_> = self
                .inherited_outstanding(run_id, &task.id, comments)?
                .into_iter()
                .collect();
            let baseline = SteeringWatermark::default();
            let pending = pending_operator_comments(&baseline, comments)
                .into_iter()
                .filter(|comment| outstanding.contains(&comment.mark))
                .collect();
            watermark = Some(baseline);
            pending
        } else {
            Vec::new()
        };
        let Some(watermark) = watermark.filter(|_| !pending.is_empty()) else {
            self.record_steering_snapshot(run_id, &task.id, step_id, newest_mark(comments))?;
            return Ok(None);
        };
        // A stopping step gets no steering, and its comments stay
        // undelivered.
        if refuses_implementer_writes(task.status) {
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
        self.record_steering_delivery(run_id, &task.id, step_id, through, &delivered, &truncated)?;

        Ok(Some(json!({
            "label": STEERING_LABEL,
            "run_id": run_id,
            "snapshot": watermark.comment,
            "comments": text,
        })))
    }

    /// The promotion gate's reading of `task_id` for `run_id`, from the task,
    /// its comment log and the run's state as they are now.
    ///
    /// Off, nothing is read. A claimed leaf reads the task, its comment log
    /// and its watermark from the owner, never its replica. Any record it
    /// cannot read is an error, so the gate fails closed. A comment written
    /// after this read is the next gate's or run's steering.
    pub(crate) fn steering_gate_reading(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<SteeringGateReading, OrbitError> {
        if !self.mid_run_steering().enabled {
            return Ok(SteeringGateReading::Disabled);
        }
        let reading = self.read_steering_gate(run_id, task_id);
        if self.worker_invocation().is_none() {
            return reading;
        }
        reading.map_err(|error| {
            OrbitError::Execution(format!(
                "a claimed leaf reads task {task_id}'s comment log and steering watermark from \
                 its owner, which could not answer ({error}); its replica is never certified"
            ))
        })
    }

    fn read_steering_gate(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<SteeringGateReading, OrbitError> {
        let task = self.get_task(task_id)?;
        let comments = self.get_task_comments(task_id)?;
        let state = self
            .stores()
            .jobs()
            .read_run_state(run_id)?
            .ok_or_else(|| {
                OrbitError::Execution(format!("run {run_id} has no pipeline state to gate"))
            })?;
        let outstanding = match self.steering_watermark(task_id, Some(&state))? {
            Some(watermark) => pending_operator_comments(&watermark, &comments)
                .into_iter()
                .map(|comment| comment.mark)
                .collect(),
            None => self.inherited_outstanding(run_id, task_id, &comments)?,
        };
        Ok(gate_verdict(task.status, outstanding))
    }

    /// Outstanding steering for a run that built no prompt for the task, such
    /// as a resume whose implement step was already checkpointed. Its
    /// candidate came from the nearest run of its resume lineage that did, so
    /// that run's watermark is the baseline. Past the lineage, the watermark
    /// the task's most recent claim kept through its settlement is, since the
    /// run carries that claim's candidate. Without either, every operator
    /// comment written after the lineage started is outstanding.
    fn inherited_outstanding(
        &self,
        run_id: &str,
        task_id: &str,
        comments: &[TaskComment],
    ) -> Result<Vec<CommentLogMark>, OrbitError> {
        let jobs = self.stores().jobs();
        let record = |run_id: &str| {
            jobs.get_job_run(run_id)?.ok_or_else(|| {
                OrbitError::Execution(format!("run {run_id} has no run record to gate"))
            })
        };
        let pending = |watermark: &SteeringWatermark| {
            pending_operator_comments(watermark, comments)
                .into_iter()
                .map(|comment| comment.mark)
                .collect()
        };
        // A claimed leaf's prompts record on its owner, never in this
        // machine's run state.
        let claimed = self.worker_invocation().is_some();
        let mut run = record(run_id)?;
        for _ in 0..MAX_LINEAGE_HOPS {
            let Some(source) = run.retry_source_run_id.clone() else {
                break;
            };
            if !claimed
                && let Some(watermark) = jobs
                    .read_run_state(&source)?
                    .and_then(|mut state| state.steering_watermarks.remove(task_id))
            {
                return Ok(pending(&watermark));
            }
            run = record(&source)?;
        }
        if run.retry_source_run_id.is_some() {
            return Err(OrbitError::Execution(format!(
                "run {run_id}'s steering baseline for task {task_id} remains unresolved after \
                 {MAX_LINEAGE_HOPS} resume-lineage ancestors; refusing to certify a newer \
                 dispatch timestamp that could omit undelivered comments"
            )));
        }
        let claim_baseline = if claimed {
            self.owner_claim_steering(task_id)?.inherited
        } else {
            self.stores().tasks().task_claim_steering(task_id)?
        };
        if let Some(watermark) = claim_baseline {
            return Ok(pending(&watermark));
        }
        let started = run.started_at.unwrap_or(run.created_at);
        Ok(comments
            .iter()
            .enumerate()
            .filter(|(_, comment)| comment.at > started && !is_automation_actor(&comment.by))
            .filter_map(|(position, _)| comment_mark(comments, position))
            .collect())
    }

    /// `task_id`'s steering watermark for the run whose state is `state`: the
    /// run's own, or for a claimed leaf the one its owner holds for its
    /// claim.
    fn steering_watermark(
        &self,
        task_id: &str,
        state: Option<&PipelineState>,
    ) -> Result<Option<SteeringWatermark>, OrbitError> {
        if self.worker_invocation().is_some() {
            return Ok(self.owner_claim_steering(task_id)?.claim);
        }
        Ok(state.and_then(|state| state.steering_watermarks.get(task_id).cloned()))
    }

    /// A claimed leaf's steering records, read from its owner.
    fn owner_claim_steering(&self, task_id: &str) -> Result<ClaimSteeringReading, OrbitError> {
        self.read_owner_request(json!({"id": task_id, "_worker_read": "steering"}))
    }

    /// Record that `step_id` built a prompt for `task_id` with no steering
    /// block, whose newest comment is `newest`.
    fn record_steering_snapshot(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
        newest: Option<CommentLogMark>,
    ) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return self.record_claim_steering(
                task_id,
                ClaimSteeringRecord::Snapshot {
                    step_id: step_id.to_string(),
                    newest,
                    built_at: chrono::Utc::now(),
                },
            );
        }
        self.update_steering_watermark(run_id, |state| {
            state.record_steering_snapshot(task_id, step_id, newest);
        })
    }

    /// Record that `step_id` built a prompt for `task_id` whose steering
    /// block carried `delivered` in full and cut `truncated` down, and after
    /// which every operator comment up to `through` reached a prompt in full.
    fn record_steering_delivery(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
        through: Option<CommentLogMark>,
        delivered: &[CommentLogMark],
        truncated: &[CommentLogMark],
    ) -> Result<(), OrbitError> {
        // The owner records a claimed leaf's event and watermark in one
        // commit.
        if self.worker_invocation().is_some() {
            return self.record_claim_steering(
                task_id,
                ClaimSteeringRecord::Delivery {
                    step_id: step_id.to_string(),
                    through,
                    delivered: delivered.to_vec(),
                    truncated: truncated.to_vec(),
                    built_at: chrono::Utc::now(),
                },
            );
        }
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
            task_id,
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
            state.record_steering_delivery(task_id, step_id, through, delivered);
        })
    }

    /// Record a claimed leaf's steering on its owner, through its claim.
    fn record_claim_steering(
        &self,
        task_id: &str,
        record: ClaimSteeringRecord,
    ) -> Result<(), OrbitError> {
        self.route_worker_host_tool(
            "orbit.task.update",
            json!({"id": task_id, "_worker_steering": record}),
        )
        .map(|_| ())
    }

    /// Admit one more steering round of `run_id` for `task_id`, recording it
    /// in the run's state before answering. Refused once the run spent
    /// `workflow.mid_run_steering.max_rounds`, or when its last round was
    /// admitted for exactly `outstanding` and so delivered none of it.
    pub(crate) fn admit_steering_round(
        &self,
        run_id: &str,
        task_id: &str,
        outstanding: &[CommentLogMark],
    ) -> Result<SteeringRoundAdmission, OrbitError> {
        let max_rounds = self.mid_run_steering().max_rounds;
        let mut admission = None;
        self.update_steering_watermark(run_id, |state| {
            let rounds = state
                .steering_rounds
                .entry(task_id.to_string())
                .or_default();
            let spent = u8::try_from(rounds.len()).unwrap_or(u8::MAX);
            admission = Some(
                if rounds
                    .last()
                    .is_some_and(|round| round.outstanding == outstanding)
                {
                    SteeringRoundAdmission::Unobservable {
                        rounds: spent,
                        max_rounds,
                    }
                } else if spent >= max_rounds {
                    SteeringRoundAdmission::Exhausted {
                        rounds: spent,
                        max_rounds,
                    }
                } else {
                    rounds.push(SteeringRound {
                        admitted_at: chrono::Utc::now(),
                        outstanding: outstanding.to_vec(),
                    });
                    state.updated_at = chrono::Utc::now();
                    SteeringRoundAdmission::Admitted {
                        round: spent.saturating_add(1),
                        max_rounds,
                    }
                },
            );
        })?;
        admission.ok_or_else(|| {
            OrbitError::Execution(format!(
                "run {run_id} has no pipeline state to record a steering round"
            ))
        })
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

/// The gate's verdict on `outstanding` for a task in `status`: terminal
/// handling wins over steering.
fn gate_verdict(status: TaskStatus, outstanding: Vec<CommentLogMark>) -> SteeringGateReading {
    if refuses_implementer_writes(status) {
        SteeringGateReading::Terminal {
            status,
            outstanding,
        }
    } else if outstanding.is_empty() {
        SteeringGateReading::Clear
    } else {
        SteeringGateReading::Outstanding(outstanding)
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
