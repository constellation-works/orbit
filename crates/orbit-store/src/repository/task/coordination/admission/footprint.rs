//! Footprint guards: the claim overlap checks that ordinary writes and admission both apply.

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;

use super::super::TaskCommitBoundary;
use super::canonical_footprint;
use crate::contracts::*;

impl TaskCommitBoundary {
    pub(crate) fn guard_ordinary_footprint(
        &self,
        status: TaskStatus,
        files: &[String],
    ) -> Result<(), OrbitError> {
        if status != TaskStatus::InProgress {
            return Ok(());
        }
        let claims = self.execution_claims()?;
        if claims.is_empty() {
            return Ok(());
        }
        let checkout = self
            .registry
            .find_workspace_checkout(&self.workspace_id)?
            .ok_or_else(|| OrbitError::Store("claimed workspace checkout is unavailable".into()))?;
        if files.is_empty() {
            return Ok(());
        } // legacy no-context chore semantics
        let canonical = canonical_footprint(files, &checkout.repo_root)?;
        let lock_modes = self.store.footprint_lock_modes();
        if let Some(conflicting_claim) = claims.iter().find(|claim| {
            claim.phase.protects_footprint()
                && lock_modes.footprints_block(&canonical, &claim.footprint)
        }) {
            return Err(OrbitError::InvalidInput(format!(
                "task footprint overlaps an execution claim (task {}, run {})",
                conflicting_claim.task_id, conflicting_claim.run_context.run_id
            )));
        }
        Ok(())
    }

    pub(crate) fn refuse_unscoped_claim_write(&self, task_id: &str) -> Result<(), OrbitError> {
        if let Some(claim) = self.protecting_claim_for_task(task_id)? {
            return Err(OrbitError::InvalidInput(format!(
                "active execution claim requires a claim-scoped mutation (task {}, run {})",
                claim.task_id, claim.run_context.run_id
            )));
        }
        Ok(())
    }

    pub(crate) fn frozen_claim_conflicts(
        &self,
        files: &[String],
    ) -> Result<Vec<TaskLockConflict>, OrbitError> {
        let lock_modes = self.store.footprint_lock_modes();
        Ok(self
            .execution_claims()?
            .iter()
            .filter(|claim| claim.phase.protects_footprint())
            .flat_map(|claim| {
                files
                    .iter()
                    .filter(|file| {
                        lock_modes.footprints_block(std::slice::from_ref(*file), &claim.footprint)
                    })
                    .map(|file| TaskLockConflict {
                        file: file.clone(),
                        held_by: TaskLockHolder::Task,
                        held_by_id: claim.task_id.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect())
    }
}
