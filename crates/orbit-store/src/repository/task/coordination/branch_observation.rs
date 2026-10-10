//! Task-branch CI observations retained under the claim commit boundary.
//!
//! The current claim check and queue-or-retain decision run in one exclusive
//! section. A protecting claim queues the receipt for settlement. Otherwise
//! the artifact and applied row commit together, even if settlement finished
//! after the sweep observed the claim but before it submitted the receipt.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::TaskArtifact;

use crate::contracts::{
    BranchObservationOutcome, ClaimCommitEffects, ClaimEvidence, DEFERRED_BRANCH_OBSERVATION_KIND,
    DeferredBranchObservation, TaskCoordinationCommitOutcome, TaskCoordinationCommitParams,
    TaskCoordinationRow,
};
use crate::repository::task::v2::normalize_v2_artifact_path;

use super::TaskCommitBoundary;

impl TaskCommitBoundary {
    /// Queue or retain one observation under the same exclusive boundary as
    /// claim settlement. Retries retain one artifact and one coordination row.
    pub(crate) fn record_deferred_branch_observation(
        &self,
        observation: &DeferredBranchObservation,
    ) -> Result<BranchObservationOutcome, OrbitError> {
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
        self.with_admission(|| {
            let protecting = self
                .execution_claims()?
                .into_iter()
                .find(|claim| claim.task_id == stored.task_id && claim.phase.protects_footprint());
            stored.claiming_run_id = protecting
                .as_ref()
                .map(|claim| claim.run_context.run_id.clone());
            let digest = sha256_hex(stored.content.as_bytes());
            let mut row = TaskCoordinationRow {
                kind: DEFERRED_BRANCH_OBSERVATION_KIND.to_string(),
                row_id: format!("{}:{digest}", stored.task_id),
                payload_json: String::new(),
            };
            if let Some(claim) = protecting {
                row.payload_json = encode_observation(&stored)?;
                self.insert_or_revive(&row)?;
                return Ok(BranchObservationOutcome::Deferred {
                    claiming_run_id: claim.run_context.run_id,
                });
            }

            let existing =
                self.store
                    .task_coordination_row(&self.workspace_id, &row.kind, &row.row_id)?;
            let recorded = self.artifact_recorded(&stored.task_id, &stored.artifact_path)?;
            if recorded
                && let Some(existing) = &existing
                && serde_json::from_str::<DeferredBranchObservation>(&existing.payload_json)
                    .map_err(|error| OrbitError::Store(error.to_string()))?
                    .applied
            {
                return Ok(BranchObservationOutcome::Retained);
            }
            stored.applied = true;
            row.payload_json = encode_observation(&stored)?;
            let mut params = TaskCoordinationCommitParams {
                task_id: stored.task_id.clone(),
                actor: "system:ci_failure_sweep".into(),
                ..Default::default()
            };
            let mut effects = ClaimCommitEffects::default();
            if let Some(existing) = existing {
                effects.replacements.push((existing, row));
            } else {
                params.rows.push(row);
            }
            let evidence = ClaimEvidence {
                artifacts: if recorded {
                    Vec::new()
                } else {
                    vec![TaskArtifact::from_text(
                        stored.artifact_path,
                        stored.content,
                    )]
                },
                ..Default::default()
            };
            match self.commit_locked_effects(
                &params,
                &mut |_| Ok(params.rows.clone()),
                &effects,
                &evidence,
                None,
            )? {
                TaskCoordinationCommitOutcome::Committed(_) => {
                    Ok(BranchObservationOutcome::Retained)
                }
                outcome => Err(OrbitError::Store(format!(
                    "branch observation retention did not commit: {outcome:?}"
                ))),
            }
        })
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

    /// Caller holds the exclusive admission section, so no settlement or
    /// receipt insertion can change this row between its read and write.
    fn insert_or_revive(&self, incoming: &TaskCoordinationRow) -> Result<(), OrbitError> {
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
        if self.store.replace_task_coordination_payload(
            &self.workspace_id,
            &existing,
            &incoming.payload_json,
        )? {
            Ok(())
        } else {
            Err(OrbitError::Store(
                "deferred branch observation changed inside the commit boundary".into(),
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

fn encode_observation(observation: &DeferredBranchObservation) -> Result<String, OrbitError> {
    serde_json::to_string(observation).map_err(|error| OrbitError::Store(error.to_string()))
}
