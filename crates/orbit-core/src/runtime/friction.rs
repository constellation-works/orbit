//! Runtime-owned access to the workspace-partitioned friction repository.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::compose::workspace_friction_store;
use orbit_store::contracts::{FrictionRehomeOutcome, FrictionRehomeParams, FrictionStoreBackend};
use std::path::PathBuf;
use std::sync::Arc;

use crate::OrbitRuntime;
use crate::runtime::workspace::catalog::WorkspaceScope;

/// Friction repository scoped by this runtime's workspace identity.
///
/// Uses the runtime-owned host store rather than opening the audit database
/// again on every call.
pub(crate) fn store_for(
    runtime: &OrbitRuntime,
) -> Result<Arc<dyn FrictionStoreBackend>, OrbitError> {
    workspace_friction_store(
        runtime.sqlite_store()?,
        runtime.workspace_id()?,
        files_root(runtime),
    )
}

/// Where this runtime's workspace keeps its friction tag taxonomy.
pub(crate) fn files_root(runtime: &OrbitRuntime) -> PathBuf {
    runtime.data_root().join("frictions")
}

impl OrbitRuntime {
    /// Workspace tag names and descriptions used to advertise friction inputs.
    pub fn friction_tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError> {
        store_for(self)?.tag_taxonomy()
    }

    /// Move friction `id` into the registered workspace `to_workspace` names.
    ///
    /// The target resolves through the same catalog `--workspace` selectors
    /// use, and both sides must accept coordination writes here: a replica
    /// checkout of the owning workspace cannot receive the record any more
    /// than it could receive a task.
    pub(crate) fn rehome_friction(
        &self,
        id: &str,
        to_workspace: &str,
    ) -> Result<FrictionRehomeOutcome, OrbitError> {
        let catalog = self.workspace_catalog().ok_or_else(|| {
            OrbitError::WorkspaceError(
                "friction re-homing needs the workspace registry; run it from a registered workspace"
                    .to_string(),
            )
        })?;
        let target = catalog
            .resolve_scope(&WorkspaceScope::Selectors(vec![to_workspace.to_string()]))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "workspace '{to_workspace}' is not registered here"
                ))
            })?;
        let owner = catalog.open(&target)?;
        owner.ensure_coordination_task_write_permitted()?;
        let source_label = match self.workspace_runtime_binding() {
            Some(binding) => binding.logical_workspace_id.clone(),
            None => self.workspace_id()?,
        };
        store_for(self)?.rehome(
            id,
            FrictionRehomeParams {
                target_workspace_id: owner.workspace_id()?,
                target_files_root: files_root(&owner),
                target_label: target.workspace_id,
                source_label,
                rehomed_at: Utc::now(),
            },
        )
    }
}
