//! Workspace scope and read operations for friction records.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::identity::validate_friction_id;

use orbit_store::Store;
use orbit_store::contract::{FrictionListFilter, StoredFrictionRecord};
use orbit_store::friction_taxonomy::{load_tag_taxonomy, load_tag_taxonomy_with_descriptions};

/// Application operations scoped to the workspace that owns the records.
pub struct WorkspaceFrictions {
    pub(super) store: Store,
    pub(super) workspace_id: String,
    pub(super) files_root: PathBuf,
}

impl WorkspaceFrictions {
    /// Bind application operations to a workspace partition. The caller
    /// runs the legacy import at its composition boundary.
    pub fn open(
        store: Store,
        workspace_id: impl Into<String>,
        files_root: impl Into<PathBuf>,
    ) -> Result<Self, OrbitError> {
        let workspace_id = workspace_id.into();
        let files_root = files_root.into();
        validate_workspace_id(&workspace_id)?;
        Ok(Self {
            store,
            workspace_id,
            files_root,
        })
    }

    pub fn list(
        &self,
        filter: &FrictionListFilter,
    ) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
        self.store
            .read()?
            .friction()
            .list(&self.workspace_id, filter)
    }

    pub fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        validate_friction_id(id)?;
        self.store.read()?.friction().get(&self.workspace_id, id)
    }

    /// Other workspaces on this host that already hold `id`.
    ///
    /// Returns IDs only — never another workspace's body — so callers can
    /// refuse an unqualified cross-workspace `resolves` edge without treating
    /// friction IDs as global (ORB-11078).
    pub fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        validate_friction_id(id)?;
        self.store
            .read()?
            .friction()
            .foreign_owners_of(&self.workspace_id, id)
    }

    pub fn tags(&self) -> Result<Vec<String>, OrbitError> {
        Ok(load_tag_taxonomy(&self.files_root)?.into_iter().collect())
    }

    pub fn tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError> {
        Ok(load_tag_taxonomy_with_descriptions(&self.files_root)?
            .into_iter()
            .collect())
    }
}

pub(super) fn validate_workspace_id(workspace_id: &str) -> Result<(), OrbitError> {
    if workspace_id.trim().is_empty() || workspace_id.trim() != workspace_id {
        return Err(OrbitError::InvalidInput(format!(
            "invalid workspace ID '{workspace_id}' for friction records"
        )));
    }
    Ok(())
}
