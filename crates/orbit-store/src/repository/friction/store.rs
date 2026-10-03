//! FrictionStore construction and read surface.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::identity::validate_friction_id;

use super::{FrictionListFilter, FrictionStore, StoredFrictionRecord, queries};
use crate::Store;
use crate::driver::file::friction_store::{load_tag_taxonomy, load_tag_taxonomy_with_descriptions};

impl FrictionStore {
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
        read_page(&self.store, &self.workspace_id, filter)
    }

    pub fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        validate_friction_id(id)?;
        self.store
            .with_read_connection(|conn| queries::show_record(conn, &self.workspace_id, id))
    }

    /// Other workspaces on this host that already hold `id`.
    ///
    /// Returns IDs only — never another workspace's body — so callers can
    /// refuse an unqualified cross-workspace `resolves` edge without treating
    /// friction IDs as global (ORB-11078).
    pub fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        validate_friction_id(id)?;
        self.store
            .with_read_connection(|conn| queries::foreign_owners_of(conn, &self.workspace_id, id))
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

/// Shared read entry point so the export workflow pages
/// through the same bounded query the tool surfaces use.
pub(crate) fn read_page(
    store: &Store,
    workspace_id: &str,
    filter: &FrictionListFilter,
) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
    store.with_read_connection(|conn| queries::list_records(conn, workspace_id, filter))
}

pub(super) fn validate_workspace_id(workspace_id: &str) -> Result<(), OrbitError> {
    if workspace_id.trim().is_empty() || workspace_id.trim() != workspace_id {
        return Err(OrbitError::InvalidInput(format!(
            "invalid workspace ID '{workspace_id}' for friction records"
        )));
    }
    Ok(())
}
