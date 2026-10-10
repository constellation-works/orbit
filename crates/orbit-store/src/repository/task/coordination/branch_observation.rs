//! Task-branch CI observations queued until the owner's claim settles.
//!
//! The sweep must not write a task an execution claim protects. It inserts one
//! row per receipt here. Settlement of that claim, once the phase no longer
//! protects the task, writes the same artifact an ordinary retain would have
//! written and marks the row applied in that commit.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::TaskArtifact;

use crate::contracts::{
    ClaimCommitEffects, ClaimEvidence, DEFERRED_BRANCH_OBSERVATION_KIND, DeferredBranchObservation,
    TaskCoordinationRow,
};
use crate::repository::task::v2::normalize_v2_artifact_path;

use super::TaskCommitBoundary;

impl TaskCommitBoundary {
    /// Insert one deferred observation. A row for the same receipt is success:
    /// retries of the sweep must not duplicate it. An applied row whose
    /// artifact is gone is queued again so the next settlement can rewrite it.
    pub(crate) fn record_deferred_branch_observation(
        &self,
        observation: &DeferredBranchObservation,
    ) -> Result<(), OrbitError> {
        orbit_types::task::validate_orb_task_id(&observation.task_id)?;
        if observation.content.is_empty() {
            return Err(OrbitError::InvalidInput(
                "deferred branch observation requires content".into(),
            ));
        }
        let mut stored = observation.clone();
        stored.schema_version = 1;
        stored.applied = false;
        stored.artifact_path = normalize_v2_artifact_path(&observation.artifact_path)?;
        let digest = sha256_hex(stored.content.as_bytes());
        let row = TaskCoordinationRow {
            kind: DEFERRED_BRANCH_OBSERVATION_KIND.to_string(),
            row_id: format!("{}:{digest}", stored.task_id),
            payload_json: serde_json::to_string(&stored)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
        };
        self.enter_ordinary(|| self.insert_or_revive(&row))
    }

    /// Copy this task's unapplied observations onto `evidence` and mark those
    /// rows applied. Caller holds the admission section and commits `effects`
    /// with `evidence` in the same claim mutation.
    pub(crate) fn attach_deferred_branch_observations(
        &self,
        task_id: &str,
        evidence: &mut ClaimEvidence,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        let rows = self
            .store
            .task_coordination_rows(&self.workspace_id, DEFERRED_BRANCH_OBSERVATION_KIND)?;
        for row in rows {
            if !row_is_for_task(&row.row_id, task_id) {
                continue;
            }
            let observation: DeferredBranchObservation = serde_json::from_str(&row.payload_json)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if observation.applied || observation.task_id != task_id {
                continue;
            }
            let path = normalize_v2_artifact_path(&observation.artifact_path)?;
            let content = observation.content.clone();
            let conflicts = evidence.artifacts.iter().any(|artifact| {
                artifact.path == path && artifact.content.as_slice() != content.as_bytes()
            });
            if conflicts {
                orbit_common::tracing::warn!(
                    task_id,
                    path = %path,
                    "deferred branch observation conflicts with claim evidence; left unapplied"
                );
                continue;
            }
            let present = evidence
                .artifacts
                .iter()
                .any(|artifact| artifact.path == path);
            if !present {
                evidence
                    .artifacts
                    .push(TaskArtifact::from_text(path, content));
            }
            let mut applied = observation;
            applied.applied = true;
            let payload = serde_json::to_string(&applied)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            effects.replacements.push((
                row.clone(),
                TaskCoordinationRow {
                    kind: row.kind,
                    row_id: row.row_id,
                    payload_json: payload,
                },
            ));
        }
        Ok(())
    }

    fn insert_or_revive(&self, row: &TaskCoordinationRow) -> Result<(), OrbitError> {
        match self
            .store
            .insert_task_coordination_row(&self.workspace_id, row)
        {
            Ok(()) => Ok(()),
            Err(error) if unique_constraint(&error) => self.revive_if_artifact_missing(row),
            Err(error) => Err(error),
        }
    }

    fn revive_if_artifact_missing(&self, incoming: &TaskCoordinationRow) -> Result<(), OrbitError> {
        let Some(existing) = self.store.task_coordination_row(
            &self.workspace_id,
            &incoming.kind,
            &incoming.row_id,
        )?
        else {
            return self
                .store
                .insert_task_coordination_row(&self.workspace_id, incoming);
        };
        let observation: DeferredBranchObservation =
            serde_json::from_str(&existing.payload_json)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
        if !observation.applied
            || self.artifact_recorded(&observation.task_id, &observation.artifact_path)?
        {
            return Ok(());
        }
        let mut revived = observation;
        revived.applied = false;
        let payload = serde_json::to_string(&revived)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        if self
            .store
            .replace_task_coordination_payload(&self.workspace_id, &existing, &payload)?
        {
            return Ok(());
        }
        let Some(again) = self.store.task_coordination_row(
            &self.workspace_id,
            &incoming.kind,
            &incoming.row_id,
        )?
        else {
            return Err(OrbitError::Store(
                "deferred branch observation disappeared while requeueing".into(),
            ));
        };
        let observation: DeferredBranchObservation = serde_json::from_str(&again.payload_json)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        if !observation.applied
            || self.artifact_recorded(&observation.task_id, &observation.artifact_path)?
        {
            Ok(())
        } else {
            Err(OrbitError::Store(
                "deferred branch observation could not be requeued".into(),
            ))
        }
    }

    fn artifact_recorded(&self, task_id: &str, path: &str) -> Result<bool, OrbitError> {
        let bundle = match self.bundle_store.read_bundle_lightweight(task_id) {
            Ok(bundle) => bundle,
            Err(OrbitError::NotFound { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let Some(manifest) = bundle.artifact_manifest else {
            return Ok(false);
        };
        let path = normalize_v2_artifact_path(path)?;
        Ok(manifest.files.iter().any(|file| {
            normalize_v2_artifact_path(&file.path).ok().as_deref() == Some(path.as_str())
        }))
    }
}

fn row_is_for_task(row_id: &str, task_id: &str) -> bool {
    row_id
        .strip_prefix(task_id)
        .is_some_and(|rest| rest.starts_with(':'))
}

fn unique_constraint(error: &OrbitError) -> bool {
    error.to_string().contains("UNIQUE constraint failed")
}
