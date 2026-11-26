//! Stored-layer accessors on [`RuntimeKernel`]: opened stores, definition stores, and workspace catalog.

use std::sync::Arc;

use orbit_common::OrbitError;

use super::stores::OrbitStores;
use super::workspace::catalog;
use crate::RuntimeKernel;

impl RuntimeKernel {
    pub fn stores(&self) -> &OrbitStores {
        self.stored().stores()
    }

    /// Attach the workspace catalog that resolves federated search scope. Like
    /// the replica owner, it is supplied by the registry-owning layer and
    /// never constructed by Core.
    pub fn with_workspace_catalog(mut self, catalog: Arc<dyn catalog::WorkspaceCatalog>) -> Self {
        self.stored_mut().workspace_catalog = Some(catalog);
        self
    }

    pub fn workspace_catalog(&self) -> Option<&Arc<dyn catalog::WorkspaceCatalog>> {
        self.stored().workspace_catalog.as_ref()
    }

    pub fn list_executor_defs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().read().executor_def().list()
    }

    pub fn get_executor_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().read().executor_def().get(name)
    }

    pub fn upsert_executor_def(
        &self,
        def: &orbit_types::workflow::ExecutorDef,
    ) -> Result<(), OrbitError> {
        self.stores()
            .executors()
            .write(|tx| tx.executor_def().put(def))
    }

    pub fn list_policy_defs(&self) -> Result<Vec<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().read().policy_def().list()
    }

    pub fn get_policy_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().read().policy_def().get(name)
    }

    pub fn upsert_policy_def(
        &self,
        def: &orbit_types::policy::PolicyDef,
    ) -> Result<(), OrbitError> {
        self.stores()
            .policies()
            .write(|tx| tx.policy_def().put(def))
    }
}
