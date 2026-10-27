//! The branch and worktree path of a task's run, for the task detail panel.
//! Read only from what the run recorded: its own `worktree` step on the
//! machine that executed it, or, for a run another machine executed, what
//! that machine reported beside its claim settlement. Nothing is derived
//! from naming rules, so an unrecorded value is reported as unrecorded.

use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimInspection, ClaimRun};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskRow, job_run_is_locally_navigable};

/// The branch and worktree path a run's `worktree` step recorded.
#[derive(Debug, Default)]
pub(crate) struct RecordedRunWorktree {
    pub(crate) branch: Option<String>,
    pub(crate) worktree_path: Option<String>,
}

impl RecordedRunWorktree {
    fn of_state(state: &PipelineState) -> Self {
        let field = |name: &str| {
            state
                .pipeline
                .get("worktree")
                .and_then(|output| output.get(name))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            branch: field("head_ref"),
            worktree_path: field("workspace_path"),
        }
    }

    fn complete(&self) -> bool {
        self.branch.is_some() && self.worktree_path.is_some()
    }
}

/// The run's branch and path from the claim bound to it: what its follower
/// reported at settlement, else the branch of the candidate the claim kept
/// from that same run.
fn claimed_run_worktree(claim: &ClaimInspection, run_id: &str) -> RecordedRunWorktree {
    let settlement = claim.settlement.as_ref();
    let kept_branch = claim
        .preserved_candidate
        .as_ref()
        .map(|preserved| &preserved.candidate)
        .filter(|candidate| candidate.source_run_id.as_deref() == Some(run_id))
        .map(|candidate| candidate.branch.trim().to_string())
        .filter(|branch| !branch.is_empty());
    RecordedRunWorktree {
        branch: settlement
            .and_then(|record| record.branch.clone())
            .or(kept_branch),
        worktree_path: settlement.and_then(|record| record.worktree_path.clone()),
    }
}

impl OrbitRuntime {
    /// The branch and worktree path `run_id`'s `worktree` step recorded in
    /// this machine's job store; empty when the run or the step is absent.
    pub(crate) fn recorded_run_worktree(&self, run_id: &str) -> RecordedRunWorktree {
        self.read_run_state(run_id)
            .ok()
            .flatten()
            .map(|state| RecordedRunWorktree::of_state(&state))
            .unwrap_or_default()
    }

    /// `job_run_worktree` for one task row: `branch` and `worktree_path` of
    /// the task's linked run on the machine that executed it, each `null`
    /// when it was not recorded, with `unrecorded` saying why. `None` when
    /// the task has no linked run.
    ///
    /// A run this machine executed is read from its own `worktree` step. A
    /// run another machine executed is known here only through the claim
    /// bound to that machine and run: the values its follower reported when
    /// it settled. A run with the same id in this machine's store is a
    /// different run and is never read for it.
    pub fn job_run_worktree(&self, row: &TaskRow) -> Result<Option<Value>, OrbitError> {
        let task = &row.task;
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        let (recorded, missing) =
            if job_run_is_locally_navigable(self.automation_machine_identity(), task) {
                (
                    self.recorded_run_worktree(run_id),
                    "The run did not record its worktree on this machine.",
                )
            } else {
                self.foreign_run_worktree(&task.id, run_id, task.job_run_machine.as_ref())
            };
        Ok(Some(json!({
            "branch": recorded.branch,
            "worktree_path": recorded.worktree_path,
            "unrecorded": (!recorded.complete()).then_some(missing),
        })))
    }

    fn foreign_run_worktree(
        &self,
        task_id: &str,
        run_id: &str,
        location: Option<&orbit_types::task::ExecutionLocation>,
    ) -> (RecordedRunWorktree, &'static str) {
        const NO_CLAIM: &str = "This machine holds no claim record for the run, so its \
                                execution machine's values are unknown here.";
        let Some(location) = location else {
            return (RecordedRunWorktree::default(), NO_CLAIM);
        };
        let bound = ClaimRun {
            machine_id: location.machine_id.clone(),
            run_id: run_id.to_string(),
        };
        // A display read: an unreadable claim journal reports the values as
        // unknown rather than failing the whole detail projection.
        let claim = match self.stores().tasks().resolve_execution_claims() {
            Ok(claims) => claims.into_iter().find(|claim| {
                claim.claim.task_id == task_id && claim.bound_run.as_ref() == Some(&bound)
            }),
            Err(error) => {
                tracing::warn!(task_id, run_id, %error, "claim journal unreadable for run worktree");
                None
            }
        };
        let Some(claim) = claim else {
            return (RecordedRunWorktree::default(), NO_CLAIM);
        };
        let missing = if claim.settlement.is_some() {
            "The execution machine did not report this when the run settled."
        } else {
            "The execution machine reports this when the run settles."
        };
        (claimed_run_worktree(&claim, run_id), missing)
    }
}
