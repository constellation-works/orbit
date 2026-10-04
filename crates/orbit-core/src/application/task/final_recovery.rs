//! The deterministic applier for `final_recovery` decisions.
//!
//! The final-recovery agent only proposes. Every lifecycle write a decision
//! implies happens here, under the task write lock, after re-checking the
//! task against the revision observed when the run failed. The engine hook
//! and the out-of-pipeline backstop both apply decisions through this one
//! module, so a decision means the same thing whichever path produced it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskComment, TaskStatus};
use orbit_types::workflow::FinalRecoveryDecision;
use serde_json::Value;

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use super::lifecycle::ensure_completion_run_stopped;
use crate::OrbitRuntime;

/// History event a final-recovery requeue records; the requeue bound counts it.
pub const FINAL_RECOVERY_REQUEUED_EVENT: &str = "final_recovery_requeued";
const COMPLETED_EVENT: &str = "final_recovery_completed";
const REJECTED_EVENT: &str = "final_recovery_rejected";
const ARCHIVED_EVENT: &str = "final_recovery_archived";
const ESCALATED_EVENT: &str = "final_recovery_escalated";

/// The task as it stood when the failure was recorded.
///
/// Any later write — an operator's transition, comment, or edit — changes
/// this, and the applier then refuses the decision rather than overwrite a
/// human's call with an agent's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalRecoveryTaskRevision {
    /// Status at the failure.
    pub status: TaskStatus,
    /// Last write at the failure.
    pub updated_at: DateTime<Utc>,
}

impl FinalRecoveryTaskRevision {
    /// The revision `task` is at now.
    pub fn of(task: &Task) -> Self {
        Self {
            status: task.status,
            updated_at: task.updated_at,
        }
    }
}

/// How far a verified `complete_no_diff` may take the task: the failed run's
/// own completion authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalRecoveryCompletion {
    /// The run delivered to review; an operator approves.
    Review,
    /// The run was authorized to complete the task.
    Done,
}

/// How many final-recovery requeues one task may take inside a window before
/// a further `requeue` escalates instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalRecoveryRequeueBound {
    /// Requeues allowed inside `window`.
    pub max_requeues: usize,
    /// Trailing window the requeues are counted over.
    pub window: Duration,
}

impl Default for FinalRecoveryRequeueBound {
    fn default() -> Self {
        Self {
            max_requeues: 2,
            window: Duration::hours(24),
        }
    }
}

/// Everything the applier needs besides the decision itself.
#[derive(Debug, Clone)]
pub struct FinalRecoveryRequest {
    /// The task the failed run carried.
    pub task_id: String,
    /// The failed run; named in every comment the applier writes.
    pub run_id: String,
    /// The task revision observed when the run failed.
    pub observed: FinalRecoveryTaskRevision,
    /// Checkout `evidence_commit` is resolved in.
    pub repo_root: PathBuf,
    /// Base branch ref a `complete_no_diff` commit must be reachable from.
    pub base_ref: String,
    /// The run's completion authority.
    pub completion: FinalRecoveryCompletion,
    /// Requeue bound; [`FinalRecoveryRequeueBound::default`] is 2 per 24 h.
    pub requeue_bound: FinalRecoveryRequeueBound,
}

/// What the applier did with a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalRecoveryOutcome {
    /// `resume` is the workflow engine's to apply; nothing was written.
    Resume {
        /// Step the agent asked to resume from.
        step_id: String,
    },
    /// The task moved to `status` on the verified covering commit.
    Completed {
        /// `review` or `done`, per the run's completion authority.
        status: TaskStatus,
        /// Full SHA of the verified covering commit.
        evidence_commit: String,
    },
    /// The task was rejected.
    Rejected,
    /// The task was archived.
    Archived,
    /// The task went back to `backlog`.
    Requeued,
    /// The task was blocked for a human. `reason` says why when the applier
    /// escalated a decision it could not apply as proposed.
    Escalated {
        /// The applier's own reason, when it overrode the proposal.
        reason: Option<String>,
    },
    /// Nothing changed but the audit comment.
    Refused {
        /// Why the decision was not applied.
        reason: String,
    },
}

/// One planned write: the status, its history event and note, and the
/// comment that records the decision.
struct Plan {
    status: TaskStatus,
    event: &'static str,
    note: String,
    comment: String,
    execution_summary: Option<String>,
    outcome: FinalRecoveryOutcome,
}

impl OrbitRuntime {
    /// Apply a `final_recovery` activity result to its task.
    ///
    /// `output` is the activity's raw result; a missing or malformed one
    /// escalates. `resume` returns without writing. Every other decision is
    /// re-checked under the task write lock and recorded as one task comment
    /// naming the run, whether it was applied, overridden or refused:
    ///
    /// - a task that is already terminal, or that changed after
    ///   `request.observed`, is refused;
    /// - `complete_no_diff` completes only when `evidence_commit` is reachable
    ///   from `request.base_ref`, and otherwise escalates;
    /// - `requeue` past `request.requeue_bound` escalates;
    /// - `escalate` blocks the task with the diagnosis and the human action.
    pub fn apply_final_recovery(
        &self,
        request: &FinalRecoveryRequest,
        output: Option<&Value>,
    ) -> Result<FinalRecoveryOutcome, OrbitError> {
        let decision = FinalRecoveryDecision::from_output(output);
        if let FinalRecoveryDecision::Resume { step_id, .. } = &decision {
            return Ok(FinalRecoveryOutcome::Resume {
                step_id: step_id.trim().to_string(),
            });
        }
        self.ensure_coordination_task_write_permitted()?;
        let mut applied = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&request.task_id, &mut || {
                let task = self.get_task(&request.task_id)?;
                let plan = match refusal(&task, request) {
                    Some(reason) => Plan::refused(&task, request, &decision, reason),
                    None => self.plan(&task, request, &decision)?,
                };
                if plan.status == TaskStatus::Done {
                    self.ensure_resolves_are_workspace_local(&task)?;
                    ensure_completion_run_stopped(self, &task, None, Some(&request.run_id))?;
                }
                self.write_plan(&task, &plan)?;
                applied = Some(plan.outcome);
                Ok(())
            })?;
        let outcome = applied.ok_or_else(|| {
            OrbitError::Execution(
                "final recovery applier did not run under the task lock".to_string(),
            )
        })?;
        if matches!(
            outcome,
            FinalRecoveryOutcome::Completed {
                status: TaskStatus::Done,
                ..
            }
        ) {
            self.record_resolves_side_effects(&self.get_task(&request.task_id)?)?;
        }
        Ok(outcome)
    }

    fn plan(
        &self,
        task: &Task,
        request: &FinalRecoveryRequest,
        decision: &FinalRecoveryDecision,
    ) -> Result<Plan, OrbitError> {
        let header = comment_header(request, decision);
        Ok(match decision {
            FinalRecoveryDecision::Resume { step_id, .. } => {
                return Err(OrbitError::Execution(format!(
                    "final recovery resume from `{step_id}` is applied by the workflow engine"
                )));
            }
            FinalRecoveryDecision::CompleteNoDiff {
                evidence_commit,
                rationale,
            } => match verify_on_base(&request.repo_root, &request.base_ref, evidence_commit) {
                Ok(commit) => {
                    let status = match request.completion {
                        FinalRecoveryCompletion::Review => TaskStatus::Review,
                        FinalRecoveryCompletion::Done => TaskStatus::Done,
                    };
                    let note = format!(
                        "final recovery (run_id={}): covering commit {commit} is reachable from \
                         '{}'",
                        request.run_id, request.base_ref
                    );
                    Plan {
                        status,
                        event: COMPLETED_EVENT,
                        execution_summary: task.execution_summary.trim().is_empty().then(|| {
                            format!(
                                "Outcome: success\nAlready delivered on '{}' by {commit} \
                                 (verified by final recovery, run_id={}).\n{}",
                                request.base_ref,
                                request.run_id,
                                rationale.trim()
                            )
                        }),
                        comment: format!(
                            "{header} outcome=completed status={status}\n\
                             evidence_commit: {commit}\nbase_ref: {}\nrationale: {}",
                            request.base_ref,
                            rationale.trim()
                        ),
                        note,
                        outcome: FinalRecoveryOutcome::Completed {
                            status,
                            evidence_commit: commit,
                        },
                    }
                }
                Err(reason) => {
                    let reason = format!("complete_no_diff refused: {reason}");
                    Plan::escalated(
                        request,
                        &header,
                        &reason,
                        &format!(
                            "evidence_commit: {}\nrationale: {}\nhuman_action: find the commit \
                             that delivers this task on '{}', or move the task by hand",
                            evidence_commit.trim(),
                            rationale.trim(),
                            request.base_ref
                        ),
                        Some(reason.clone()),
                    )
                }
            },
            FinalRecoveryDecision::Reject { reason, evidence } => Plan {
                status: TaskStatus::Rejected,
                event: REJECTED_EVENT,
                note: format!(
                    "final recovery (run_id={}): {}",
                    request.run_id,
                    reason.trim()
                ),
                comment: format!(
                    "{header} outcome=rejected\nreason: {}\nevidence: {}",
                    reason.trim(),
                    evidence.trim()
                ),
                execution_summary: None,
                outcome: FinalRecoveryOutcome::Rejected,
            },
            FinalRecoveryDecision::Archive { reason } => Plan {
                status: TaskStatus::Archived,
                event: ARCHIVED_EVENT,
                note: format!(
                    "final recovery (run_id={}): {}",
                    request.run_id,
                    reason.trim()
                ),
                comment: format!("{header} outcome=archived\nreason: {}", reason.trim()),
                execution_summary: None,
                outcome: FinalRecoveryOutcome::Archived,
            },
            FinalRecoveryDecision::Requeue { reason } => {
                let bound = request.requeue_bound;
                let recent = self.recent_requeues(&task.id, bound.window)?;
                if recent >= bound.max_requeues {
                    let limit = format!(
                        "requeue refused: final recovery already requeued this task {recent} \
                         time(s) in the last {} hour(s) (limit {})",
                        bound.window.num_hours(),
                        bound.max_requeues
                    );
                    Plan::escalated(
                        request,
                        &header,
                        &limit,
                        &format!(
                            "requeue_reason: {}\nhuman_action: the failure keeps recurring; \
                             fix its cause before moving the task back to backlog",
                            reason.trim()
                        ),
                        Some(limit.clone()),
                    )
                } else {
                    Plan {
                        status: TaskStatus::Backlog,
                        event: FINAL_RECOVERY_REQUEUED_EVENT,
                        note: format!(
                            "final recovery (run_id={}): {}",
                            request.run_id,
                            reason.trim()
                        ),
                        comment: format!(
                            "{header} outcome=requeued requeue={} of {} per {} hour(s)\n\
                             reason: {}",
                            recent + 1,
                            bound.max_requeues,
                            bound.window.num_hours(),
                            reason.trim()
                        ),
                        execution_summary: None,
                        outcome: FinalRecoveryOutcome::Requeued,
                    }
                }
            }
            FinalRecoveryDecision::Escalate {
                diagnosis,
                human_action,
            } => Plan::escalated(
                request,
                &header,
                diagnosis.trim(),
                &format!("human_action: {}", human_action.trim()),
                None,
            ),
        })
    }

    /// Final-recovery requeues recorded in the trailing `window`.
    fn recent_requeues(&self, task_id: &str, window: Duration) -> Result<usize, OrbitError> {
        let since = Utc::now() - window;
        Ok(self
            .get_task_history(task_id)?
            .iter()
            .filter(|entry| entry.event == FINAL_RECOVERY_REQUEUED_EVENT && entry.at >= since)
            .count())
    }

    fn write_plan(&self, task: &Task, plan: &Plan) -> Result<(), OrbitError> {
        let comment = TaskComment {
            at: Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            message: plan.comment.clone(),
        };
        let transition = plan.status != task.status;
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                &task.id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    status: transition.then_some(plan.status),
                    status_event: transition.then(|| plan.event.to_string()),
                    status_note: transition.then(|| plan.note.clone()),
                    execution_summary: plan.execution_summary.clone(),
                    append_comments: vec![comment.clone()],
                    expected_status: Some(vec![task.status]),
                    ..Default::default()
                },
            )?;
            Ok((
                updated,
                OrbitEvent::TaskUpdated {
                    id: task.id.clone(),
                },
            ))
        })?;
        Ok(())
    }
}

impl Plan {
    fn refused(
        task: &Task,
        request: &FinalRecoveryRequest,
        decision: &FinalRecoveryDecision,
        reason: String,
    ) -> Self {
        Self {
            status: task.status,
            event: ESCALATED_EVENT,
            note: String::new(),
            comment: format!(
                "{} outcome=refused\nreason: {reason}",
                comment_header(request, decision)
            ),
            execution_summary: None,
            outcome: FinalRecoveryOutcome::Refused { reason },
        }
    }

    fn escalated(
        request: &FinalRecoveryRequest,
        header: &str,
        diagnosis: &str,
        detail: &str,
        override_reason: Option<String>,
    ) -> Self {
        Self {
            status: TaskStatus::Blocked,
            event: ESCALATED_EVENT,
            note: format!(
                "final recovery (run_id={}) escalated: {diagnosis}",
                request.run_id
            ),
            comment: format!("{header} outcome=escalated\ndiagnosis: {diagnosis}\n{detail}"),
            execution_summary: None,
            outcome: FinalRecoveryOutcome::Escalated {
                reason: override_reason,
            },
        }
    }
}

/// Why the decision may not be applied to `task` at all, if it may not.
fn refusal(task: &Task, request: &FinalRecoveryRequest) -> Option<String> {
    if matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    ) {
        return Some(format!(
            "task is already {}; final recovery never reopens settled work",
            task.status
        ));
    }
    let current = FinalRecoveryTaskRevision::of(task);
    if current != request.observed {
        return Some(format!(
            "task changed after the failure (was {} at {}, now {} at {}); the later decision \
             stands",
            request.observed.status,
            request.observed.updated_at.to_rfc3339(),
            current.status,
            current.updated_at.to_rfc3339()
        ));
    }
    None
}

fn comment_header(request: &FinalRecoveryRequest, decision: &FinalRecoveryDecision) -> String {
    format!(
        "final_recovery run_id={} decision={}",
        request.run_id,
        decision.kind()
    )
}

/// Resolve `commit` and prove it is reachable from `base_ref`, returning its
/// full SHA. The decision parser already admitted `commit` as hexadecimal, so
/// neither value can be read as a Git option.
fn verify_on_base(repo_root: &Path, base_ref: &str, commit: &str) -> Result<String, String> {
    let base_ref = base_ref.trim();
    if base_ref.is_empty() || base_ref.starts_with('-') {
        return Err(format!("base ref '{base_ref}' is not a usable ref"));
    }
    let resolve = |rev: &str, what: &str| -> Result<String, String> {
        let spec = format!("{rev}^{{commit}}");
        let output = run_git(repo_root, &["rev-parse", "--verify", "--quiet", &spec])
            .map_err(|error| error.to_string())?;
        let sha = output.stdout.trim();
        if output.success && !sha.is_empty() {
            Ok(sha.to_string())
        } else {
            Err(format!("{what} '{rev}' does not resolve to a commit"))
        }
    };
    let commit = resolve(commit.trim(), "evidence commit")?;
    let base = resolve(base_ref, "base ref")?;
    let ancestor = run_git(repo_root, &["merge-base", "--is-ancestor", &commit, &base])
        .map_err(|error| error.to_string())?;
    if ancestor.success {
        Ok(commit)
    } else {
        Err(format!(
            "evidence commit {commit} is not reachable from '{base_ref}' ({base})"
        ))
    }
}
