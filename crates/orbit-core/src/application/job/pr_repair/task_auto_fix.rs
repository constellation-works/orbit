//! One task's out-of-run auto-fix, started by an operator.
//!
//! The dashboard's Fix PR action repairs one `review` task's conflicting or
//! red Orbit pull request without a drain. It runs the drain pass's own steps
//! for that task (`adoption`): the newest own delivery leaf, the same verdict,
//! resume plan and start, the same attempt ledger and budget, the same
//! deferred footprint locks and the same `pr_auto_fix_out_of_run` history
//! record, which names no drain. The resumed leaf keeps its own
//! `completion`, so this action never merges a pull request whose leaf lacked
//! `--complete`; a leaf that held it is resumed only for a caller that holds
//! completion authority too, as a drain must.
//!
//! Every reason the drain leaves a task alone, plus the single-task ones
//! (a write-free or replica checkout, host resource pressure, no delivery of
//! the task's own), is a typed refusal here, never a silent no-op.

use std::fmt;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::adoption::{PassReads, RepairAuthority, RepairObservation, Verdict};
use crate::OrbitRuntime;

/// Why one task's out-of-run auto-fix was refused, or is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAutoFixRefusal {
    reason: &'static str,
    detail: String,
}

impl TaskAutoFixRefusal {
    fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    /// The reason token, as the drain's adoption pass reports it (for
    /// example `live_run`, `operator_hold`, `budget_exhausted`, `no_blocker`).
    pub fn reason(&self) -> &'static str {
        self.reason
    }

    /// Stable machine-readable error code: `task_auto_fix_<reason>`.
    pub fn code(&self) -> String {
        format!("task_auto_fix_{}", self.reason)
    }

    /// The human-readable explanation.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TaskAutoFixRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

/// A failed single-task auto-fix start.
#[derive(Debug)]
pub enum TaskAutoFixError {
    /// Nothing was started, for the named reason.
    Refused(TaskAutoFixRefusal),
    /// Lookup failed (an unknown task is `NotFound`).
    Runtime(OrbitError),
}

impl fmt::Display for TaskAutoFixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Runtime(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for TaskAutoFixError {}

impl From<OrbitError> for TaskAutoFixError {
    fn from(error: OrbitError) -> Self {
        Self::Runtime(error)
    }
}

/// How far [`OrbitRuntime::task_pr_auto_fix`] goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Decide and plan, start nothing.
    Check,
    Start,
}

impl OrbitRuntime {
    /// Whether [`Self::start_task_pr_auto_fix`] would start a repair of
    /// `task_id`'s pull request now, without starting one. Reads the forge
    /// fresh. Returns `{available, reason, detail, pr_number, blocker,
    /// head_sha}`; `reason` and `detail` are those of the refusal a start
    /// would answer.
    pub fn task_pr_auto_fix_readiness(
        &self,
        task_id: &str,
        completion_done: bool,
    ) -> Result<Value, OrbitError> {
        let mut observation = RepairObservation::default();
        match self.task_pr_auto_fix(
            task_id,
            completion_done,
            None,
            Mode::Check,
            &mut observation,
        ) {
            Ok(planned) => Ok(json!({
                "available": true,
                "reason": Value::Null,
                "detail": Value::Null,
                "pr_number": planned["pr_number"],
                "blocker": planned["blocker"],
                "head_sha": planned["head_sha"],
            })),
            Err(TaskAutoFixError::Refused(refusal)) => Ok(json!({
                "available": false,
                "reason": refusal.reason,
                "detail": refusal.detail,
                "pr_number": observation.pr_number,
                "blocker": observation.blocker,
                "head_sha": Value::Null,
            })),
            Err(TaskAutoFixError::Runtime(error)) => Err(error),
        }
    }

    /// Start an out-of-run auto-fix of `task_id`'s pull request: the repair
    /// an `--auto-fix` drain's adoption pass would start for it, with no
    /// drain. `completion_done` is the caller's completion authority; a leaf
    /// started with `--complete` is resumed only under it. Returns the
    /// started repair as the drain pass reports one.
    pub fn start_task_pr_auto_fix(
        &self,
        task_id: &str,
        completion_done: bool,
        actor: Option<&str>,
    ) -> Result<Value, TaskAutoFixError> {
        let mut observation = RepairObservation::default();
        self.task_pr_auto_fix(
            task_id,
            completion_done,
            actor,
            Mode::Start,
            &mut observation,
        )
    }

    fn task_pr_auto_fix(
        &self,
        task_id: &str,
        completion_done: bool,
        actor: Option<&str>,
        mode: Mode,
        observation: &mut RepairObservation,
    ) -> Result<Value, TaskAutoFixError> {
        let refuse = |reason, detail: String| {
            Err(TaskAutoFixError::Refused(TaskAutoFixRefusal::new(
                reason, detail,
            )))
        };
        if let Some(owner) = self.replica_owner_machine() {
            return refuse(
                "replica_checkout",
                format!("this checkout is a replica of {owner}; repair the pull request there"),
            );
        }
        if self.is_write_free() {
            return refuse(
                "write_free",
                "this dashboard is read-only (write-free mode)".to_string(),
            );
        }
        if self.resource_admission().throttle.is_some() {
            return refuse(
                "resource_throttled",
                "host resource pressure holds new runs; try again when it clears".to_string(),
            );
        }
        // An unknown task is a lookup error, not a refusal.
        self.get_task(task_id)?;
        let Some(run) = self.newest_own_delivery_leaf(task_id)? else {
            return refuse(
                "no_delivery_leaf",
                "the task has no delivery run of its own to hand the pull request back to"
                    .to_string(),
            );
        };
        let authority = RepairAuthority {
            drain_run_id: None,
            actor,
            completion_done,
        };
        let mut reads = PassReads::fresh();
        let verdict =
            match self.repair_verdict(&run, task_id, &authority, &mut reads, observation, None) {
                Verdict::Repair(repair) => self.plan_repair(&run, *repair, &mut reads),
                verdict => verdict,
            };
        match verdict {
            Verdict::Repair(repair) if mode == Mode::Check => Ok(repair.summary(&run)),
            Verdict::Repair(repair) => match self.start_repair(&run, &authority, *repair) {
                Verdict::Started(started) => Ok(started),
                Verdict::Ignore(reason, detail) | Verdict::Skip(reason, detail) => {
                    refuse(reason, detail)
                }
                Verdict::Repair(_) => refuse(
                    "resume_failed",
                    "the repair was decided but not started".to_string(),
                ),
            },
            Verdict::Ignore(reason, detail) | Verdict::Skip(reason, detail) => {
                refuse(reason, detail)
            }
            Verdict::Started(started) => Ok(started),
        }
    }
}
