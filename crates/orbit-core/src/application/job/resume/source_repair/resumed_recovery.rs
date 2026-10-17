//! A generic resume's own final-recovery repair, published by its failure hook.
//!
//! A resumed run reuses the worktree checkpoint of the run that created it, so
//! that original run stays the task's and the checkpoint's owner. When the
//! resumed run's final recovery commits a repair and resumes, the host records
//! the exact before/after HEAD in the resumed run's own state. A commit
//! boundary that consulted only the owner refused that HEAD, and the failure
//! hook then published it. Recovering that publication authenticates both
//! runs: the resumed source's receipt and hook, and the owner's immutable host
//! chain the repair sits on.
use super::observation::authenticated_checkpoint;
use super::*;
use orbit_types::workflow::{FinalRecoveryDecision, FinalRecoveryRepairCommit, SourceRepairOwner};

/// Same bound the generic resume planner applies to `retry_source_run_id`.
const MAX_OWNER_HOPS: usize = 64;

pub(super) struct ResumedRecovery {
    pub(super) owner: JobRun,
    pub(super) owner_state: PipelineState,
    pub(super) owner_definition: JobV2,
    pub(super) repair: FinalRecoveryRepairCommit,
}

impl ResumedRecovery {
    pub(super) fn binding(&self) -> Result<SourceRepairOwner, OrbitError> {
        Ok(SourceRepairOwner {
            run_id: self.owner.run_id.clone(),
            digest: digest(&json!({
                "run": self.owner, "steps": self.owner.steps,
                "checkpoints": self.owner_state, "definition": self.owner_definition,
            }))?,
        })
    }
}

impl OrbitRuntime {
    /// The original checkpoint owner and the source's own repair receipt, when
    /// `source` is a stopped failed resume whose worktree checkpoint names an
    /// earlier run and whose failure hook ran. `None` for every other shape;
    /// once the shape applies, any disagreement refuses rather than falling
    /// back to another provenance path.
    pub(super) fn resumed_recovery(
        &self,
        source: &JobRun,
        task_id: &str,
    ) -> Result<Option<ResumedRecovery>, OrbitError> {
        if source.state != JobRunState::Failed {
            return Ok(None);
        }
        let Some(state) = self.read_run_state(&source.run_id)? else {
            return Ok(None);
        };
        let Some(owner_id) = state
            .pipeline
            .get("worktree")
            .and_then(|worktree| worktree.get("job_run_id"))
            .and_then(Value::as_str)
            .filter(|owner| *owner != source.run_id)
        else {
            return Ok(None);
        };
        if state.failure_activity_checkpoint.is_none() {
            return Ok(None);
        }
        let checkpoint = state
            .final_recovery
            .as_ref()
            .ok_or_else(|| refuse("resumed source has no final-recovery receipt"))?;
        let repair = match (&checkpoint.decision, &checkpoint.repair_commit) {
            (Some(FinalRecoveryDecision::Resume { .. }), Some(repair))
                if checkpoint.task_id == task_id
                    && checkpoint.outcome.as_deref() == Some("resume") =>
            {
                repair.clone()
            }
            _ => {
                return Err(refuse(
                    "resumed final recovery is not an applied resume onto a host-recorded repair for this task",
                ));
            }
        };
        let mut chain = vec![source.run_id.clone()];
        let mut current = source.clone();
        let owner = loop {
            if chain.len() > MAX_OWNER_HOPS {
                return Err(refuse(
                    "resume lineage to the checkpoint owner is unbounded",
                ));
            }
            let parent_id = current
                .retry_source_run_id
                .clone()
                .ok_or_else(|| refuse("source does not resume its checkpoint owner"))?;
            let parent = self
                .get_job_run_backend(&parent_id)?
                .ok_or_else(|| refuse("resume lineage run is missing"))?;
            if parent.job_id != source.job_id || chain.contains(&parent.run_id) {
                return Err(refuse("resume lineage crosses jobs or repeats a run"));
            }
            chain.push(parent.run_id.clone());
            if !matches!(
                parent.state,
                JobRunState::Interrupted
                    | JobRunState::Failed
                    | JobRunState::Timeout
                    | JobRunState::Cancelled
            ) || parent.finished_at.is_none()
                || run_owner_liveness(&parent) != RunOwnerLiveness::Stopped
            {
                return Err(refuse("resume lineage contains an active or unsettled run"));
            }
            if parent.run_id == owner_id {
                break parent;
            }
            current = parent;
        };
        if !chain.contains(&checkpoint.key.run_id) {
            return Err(refuse(
                "final-recovery receipt was not recorded on this resume lineage",
            ));
        }
        let owner_state = self
            .read_run_state(&owner.run_id)?
            .ok_or_else(|| refuse("checkpoint owner state is missing"))?;
        if owner_state.run_id != owner.run_id || owner_state.job_id != owner.job_id {
            return Err(refuse("checkpoint owner state identity disagrees"));
        }
        let owner_definition = self.source_repair_definition(&owner)?;
        Ok(Some(ResumedRecovery {
            owner,
            owner_state,
            owner_definition,
            repair,
        }))
    }

    /// The bound owner, when there is one, is still exactly what inspection saw.
    pub(super) fn source_owner_unchanged(
        &self,
        source: &JobRun,
        binding: &SourceRepairBinding,
    ) -> Result<bool, OrbitError> {
        let observed = self
            .resumed_recovery(source, &binding.task_id)?
            .map(|resumed| resumed.binding())
            .transpose()?;
        Ok(observed == binding.checkpoint_owner)
    }
}

/// Authenticate the resumed source's side of the publication: its reused
/// worktree is the owner's, it never committed or published through its own
/// host steps after the repair, and the hook's candidate is the exact receipt.
/// Returns the pre-repair HEAD, which the owner's host chain must account for.
pub(super) fn authenticated_repair_parent(
    resumed: &ResumedRecovery,
    definition: &JobV2,
    state: &PipelineState,
    worktree: &Value,
    candidate: &str,
    repo: &std::path::Path,
) -> Result<String, OrbitError> {
    let owner_worktree = authenticated_checkpoint(
        &resumed.owner_definition,
        &resumed.owner_state,
        "worktree",
        "worktree_setup",
    )?;
    if owner_worktree != worktree {
        return Err(refuse(
            "resumed worktree checkpoint is not the owner's checkpoint",
        ));
    }
    let commit_index = definition
        .steps
        .iter()
        .position(|step| step.id == "commit")
        .ok_or_else(|| refuse("source definition lacks commit"))? as u32;
    if state
        .step_states
        .iter()
        .any(|(index, step)| *index >= commit_index && *step == JobRunState::Success)
    {
        return Err(refuse(
            "resumed source passed its own commit boundary; recover that delivery instead",
        ));
    }
    let workspace = std::path::Path::new(field(worktree, "workspace_path")?);
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let repair = &resumed.repair;
    if candidate != repair.head_sha
        || repair.head_sha_before == repair.head_sha
        || repair.workspace_path != workspace
        || !run_git(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                &repair.head_sha_before,
                &repair.head_sha,
            ],
        )?
        .success
    {
        return Err(refuse(
            "failure-handoff candidate is not the resumed run's exact host repair receipt",
        ));
    }
    Ok(repair.head_sha_before.clone())
}
