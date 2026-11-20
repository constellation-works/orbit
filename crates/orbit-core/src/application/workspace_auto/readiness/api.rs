use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::super::drains::workspace_drains;
use super::snapshot::{self, MAX_READINESS_LIMIT, ReadinessDepth, ReadinessDetail};
use super::{drain_progress, hosts};

/// Host-wide facts one fleet read fetches once and hands to every workspace's
/// row. A fact that could not be read is absent, and each row reads its own and
/// reports its own failure.
#[derive(Default)]
pub struct FleetReadinessFacts {
    pub(super) status_index: Option<BTreeMap<String, TaskStatus>>,
}

fn attach_readiness_runtime_fields(runtime: &OrbitRuntime, payload: &mut Value) {
    payload["replica"] = json!(runtime.replica_owner_machine().is_some());
    payload["validation_env_warning"] = json!(runtime.readiness_validation_env_warning());
    // [ORB-25965] The recorded drain intent, with who paused it and when. An
    // unreadable row is reported as such, never as `off`.
    payload["drain_intent"] = match runtime.workspace_drain_intent() {
        Ok(record) => crate::application::job::drain_intent_json(record.as_ref()),
        Err(error) => json!({ "intent": "unknown", "error": error.to_string() }),
    };
}

/// Explain the same snapshot that auto-drain uses without performing its
/// stale-run reconciliation. This is deliberately an observation API: it
/// neither reserves work nor creates a pipeline run, and its answer can go
/// stale immediately after the stores are read.
fn explain_workspace_auto_readiness(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    max_active_leaf_runs: Option<u32>,
    limit: usize,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    snapshot::readiness_snapshot(
        runtime,
        task_ids,
        max_active_leaf_runs,
        limit,
        allowed_crews,
        ReadinessDepth::Full,
    )
}

impl OrbitRuntime {
    /// Read-only projection of the current auto-drain admission snapshot.
    pub fn workspace_auto_readiness(
        &self,
        task_ids: &[String],
        max_active_leaf_runs: Option<u32>,
        limit: usize,
        allowed_crews: &[String],
    ) -> Result<Value, OrbitError> {
        let mut payload = explain_workspace_auto_readiness(
            self,
            task_ids,
            max_active_leaf_runs,
            limit,
            allowed_crews,
        )?;
        attach_readiness_runtime_fields(self, &mut payload);
        Ok(payload)
    }

    /// [`Self::workspace_auto_readiness`] plus one task's entry under
    /// `detail_tasks`, from a single read of the shared admission snapshot.
    ///
    /// `detail_task_id` is the dashboard's `also_task` id. An id this workspace
    /// does not have leaves the snapshot in place and sets `detail_unavailable`
    /// (`reason: task_not_found`) instead of failing the listing. `None`
    /// is [`Self::workspace_auto_readiness`].
    pub fn workspace_auto_readiness_with_detail(
        &self,
        task_ids: &[String],
        detail_task_id: Option<&str>,
        max_active_leaf_runs: Option<u32>,
        limit: usize,
        allowed_crews: &[String],
    ) -> Result<Value, OrbitError> {
        let Some(detail_task_id) = detail_task_id else {
            return self.workspace_auto_readiness(
                task_ids,
                max_active_leaf_runs,
                limit,
                allowed_crews,
            );
        };
        let detail_ids = vec![detail_task_id.to_string()];
        let (mut payload, detail) = snapshot::readiness_snapshot_with_detail(
            self,
            task_ids,
            &detail_ids,
            max_active_leaf_runs,
            limit,
            allowed_crews,
            ReadinessDepth::Full,
        )?;
        attach_readiness_runtime_fields(self, &mut payload);
        if let Some(detail) = detail {
            match detail {
                ReadinessDetail::Tasks(tasks) => payload["detail_tasks"] = json!(tasks),
                ReadinessDetail::Unavailable { task_id } => {
                    payload["detail_tasks"] = json!([]);
                    payload["detail_unavailable"] = json!({
                        "task_id": task_id,
                        "reason": "task_not_found",
                    });
                }
            }
        }
        Ok(payload)
    }

    /// The registry-global facts one fleet read shares across its workspaces.
    #[must_use]
    pub fn fleet_readiness_facts(&self) -> FleetReadinessFacts {
        // A replica's own reads hide task statuses, and that says nothing
        // about the other workspaces this fact would serve.
        let status_index = self
            .coordination_task_reads_visible()
            .then(|| self.task_status_index().ok())
            .flatten();
        FleetReadinessFacts { status_index }
    }

    /// The readiness snapshot a fleet row summarizes: the same admission
    /// decisions and holds as [`Self::workspace_auto_readiness`] over the
    /// default listing, without the diagnostics a row does not report
    /// (`approvals` is null; `blocking_claims`, `replica` and
    /// `validation_env_warning` are absent). Read-only like that snapshot.
    pub fn workspace_fleet_readiness(
        &self,
        facts: &FleetReadinessFacts,
    ) -> Result<Value, OrbitError> {
        snapshot::readiness_snapshot(
            self,
            &[],
            None,
            MAX_READINESS_LIMIT,
            &[],
            ReadinessDepth::FleetRow(facts),
        )
    }

    /// The workspace's drain phase as readiness reports it (`draining`,
    /// `winding_down` or `idle`), without the rest of the readiness snapshot.
    pub fn workspace_drain_phase(&self) -> Result<&'static str, OrbitError> {
        let (active_drain, _) = workspace_drains(self)?;
        Ok(drain_progress::drain_progress(self, active_drain.as_ref())?.phase)
    }

    /// Observation keys for changes to follower capabilities, drain identity,
    /// local stop evidence or freshness. Used by the dashboard change stream;
    /// reads no remote host and changes neither census nor admission state.
    pub fn executor_readiness_change_keys(&self) -> Result<Vec<String>, OrbitError> {
        let evidence = hosts::FollowerEvidence::read(self);
        let rows = evidence
            .rows
            .as_ref()
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        Ok(rows
            .iter()
            .map(|(row, state)| {
                json!({"machine": row.machine_id, "run": row.run_id,
            "capability": row.capability, "state": state})
                .to_string()
            })
            .collect())
    }
}
