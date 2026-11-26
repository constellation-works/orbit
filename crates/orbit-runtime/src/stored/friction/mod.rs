//! Runtime-owned access to the workspace-partitioned friction repository.

mod metrics;
mod mutations;
mod workspace;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::RegisteredTaskResolution;
use orbit_store::compose::prepare_workspace_frictions;
use orbit_store::contract::{FrictionRehomeOutcome, FrictionRehomeParams, FrictionUpdateParams};
use orbit_types::record::FrictionStatus;
use std::path::PathBuf;

use crate::RuntimeKernel;
use crate::workspace::catalog::WorkspaceScope;

pub use workspace::WorkspaceFrictions;
use workspace::validate_workspace_id;

impl RuntimeKernel {
    /// Permit resolution of a replica's existing host-local friction corpus.
    ///
    /// Friction reads use this host's workspace partition, which can retain
    /// records authored before the checkout became a replica. Closing those
    /// records changes no owner state. The caller must only resolve an
    /// existing record, never add, reopen, or move one through this exception.
    /// Claimed workers still have to use their owner route.
    pub fn ensure_local_friction_resolution_permitted(&self, id: &str) -> Result<(), OrbitError> {
        if !self.role().is_replica() {
            return self.ensure_coordination_task_write_permitted();
        }
        store_for(self)?
            .show(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Friction, id))?;
        Ok(())
    }

    /// Workspace tag names and descriptions used to advertise friction inputs.
    pub fn friction_tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError> {
        store_for(self)?.tag_taxonomy()
    }

    /// Refuse a `during_task` that names no task, so a typo cannot record
    /// friction against a task that does not exist and skew the per-task
    /// friction rates.
    ///
    /// Task ids are machine-global, so the lookup follows the registry to
    /// whichever local workspace owns the id, the way a dependency read does.
    /// An id whose prefix this registry has never issued belongs to another
    /// host's authority: this machine cannot show it is unknown, so it is
    /// accepted as written.
    pub fn ensure_friction_task_exists(&self, task_id: &str) -> Result<(), OrbitError> {
        match self.stores().tasks().registered_task(task_id)? {
            RegisteredTaskResolution::Resolved(_) | RegisteredTaskResolution::ForeignAuthority => {
                Ok(())
            }
            RegisteredTaskResolution::Missing => Err(OrbitError::not_found(
                NotFoundKind::Task,
                task_id.to_string(),
            )),
        }
    }

    /// Move friction `id` into the registered workspace `to_workspace` names,
    /// first applying `edits` so the copy carries them.
    ///
    /// The target resolves through the same catalog `--workspace` selectors
    /// use, and both sides must accept coordination writes here: a replica
    /// checkout of the owning workspace cannot receive the record any more
    /// than it could receive a task. Every refusal the store can decide
    /// without its write lock (same-workspace target, malformed id,
    /// unreadable target taxonomy, tags the target rejects) is checked before
    /// the edits land, so those refusals leave the record as it was. The
    /// edits and the move are still two commits: a failure only visible under
    /// the write lock, such as a concurrent resolve, can land between them.
    pub fn rehome_friction(
        &self,
        id: &str,
        to_workspace: &str,
        edits: Option<FrictionUpdateParams>,
    ) -> Result<FrictionRehomeOutcome, OrbitError> {
        let params = self.friction_rehome_target(to_workspace)?;
        let store = store_for(self)?;
        if let Some(edits) = edits {
            if edits.status == Some(FrictionStatus::Resolved) {
                return Err(OrbitError::InvalidInput(
                    "moving a friction resolves it with a pointer to the copy; drop `status: resolved`"
                        .to_string(),
                ));
            }
            let current = store
                .show(id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::Friction, id.to_string()))?;
            if current.record.status == FrictionStatus::Resolved {
                return Err(OrbitError::InvalidInput(format!(
                    "friction {id} is already resolved; there is nothing to re-home"
                )));
            }
            store.preflight_rehome(id, &params, &edits)?;
            store.update(id, edits)?;
        }
        store.rehome(id, params)
    }

    /// Resolve the workspace a friction moves into, refusing a target this
    /// runtime cannot write to.
    fn friction_rehome_target(
        &self,
        to_workspace: &str,
    ) -> Result<FrictionRehomeParams, OrbitError> {
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
        Ok(FrictionRehomeParams {
            target_workspace_id: owner.workspace_id()?,
            target_files_root: files_root(&owner),
            target_label: target.workspace_id,
            source_label,
            rehomed_at: Utc::now(),
        })
    }
}

/// Friction operations scoped to this runtime's workspace identity.
pub fn store_for(runtime: &RuntimeKernel) -> Result<WorkspaceFrictions, OrbitError> {
    let store = runtime.sqlite_store()?;
    let workspace_id = runtime.workspace_id()?;
    let files_root = files_root(runtime);
    prepare_workspace_frictions(&store, &workspace_id, &files_root)?;
    WorkspaceFrictions::open(store, workspace_id, files_root)
}

/// Where this runtime's workspace keeps its friction tag taxonomy.
pub(crate) fn files_root(runtime: &RuntimeKernel) -> PathBuf {
    runtime.data_root().join("frictions")
}
