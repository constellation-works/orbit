//! Runtime facades over the `orbit-store` task-publication workflows.
//!
//! The CLI owns operator input and registry composition. Core keeps the
//! existing dependency boundary by opening the coordination task registry and
//! forwarding already-resolved publication requests to Store.

use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_store::workflow::task::{
    inspect_publication, publish_task_snapshot, restore_publication,
};

use crate::OrbitRuntime;

pub use orbit_store::workflow::task::{
    AttachmentPolicy, AttachmentPolicyKind, InspectedPublicationTask, OmittedAttachment,
    PublicationCallerRole, PublicationFreshness, PublicationInspectLabel,
    PublicationInspectRequest, PublicationInspection, PublicationLastSuccess,
    PublicationPublishOutcome, PublicationPublishRequest, PublicationPublishStatus,
    PublicationRecoveryCompleteness, PublicationRenderAuthority, PublicationRestoreMode,
    PublicationRestoreOutcome, PublicationRestoreRequest, ScannerFailureBehavior,
};

impl OrbitRuntime {
    fn open_publication_task_registry(&self) -> Result<TaskRegistryStore, OrbitError> {
        TaskRegistryStore::open(&task_registry_path(&self.global_root()))
    }

    /// Pair the coordination registry with the explicit publication binding's
    /// portable source identity. A previously different repository is refused;
    /// equivalent SSH and HTTPS URLs are accepted without rewriting old rows.
    pub fn record_task_publication_source(
        &self,
        workspace_id: &str,
        source_repository_fingerprint: &str,
    ) -> Result<(), OrbitError> {
        self.open_publication_task_registry()?
            .record_workspace_repo_fingerprint(workspace_id, source_repository_fingerprint)?;
        Ok(())
    }

    /// Reconcile the task partition after the owner explicitly rebinds its
    /// registered source remote. Call after saving the workspace registration,
    /// under its registry lock, including on retries of an unchanged source.
    pub fn rebind_task_publication_source(
        &self,
        task_workspace_id: &str,
        source_remote: &str,
    ) -> Result<(), OrbitError> {
        self.open_publication_task_registry()?
            .rebind_workspace_repo_fingerprint(task_workspace_id, source_remote)?;
        Ok(())
    }

    /// Publish a validated snapshot with a caller-resolved owner binding.
    pub fn publish_task_publication(
        &self,
        request: PublicationPublishRequest,
        policy: &AttachmentPolicy,
    ) -> Result<PublicationPublishOutcome, OrbitError> {
        let registry = self.open_publication_task_registry()?;
        publish_task_snapshot(&registry, request, policy, None)
    }

    /// Fetch, validate, and render a publication without mutating task state.
    pub fn inspect_task_publication(
        &self,
        request: PublicationInspectRequest,
    ) -> Result<PublicationInspection, OrbitError> {
        inspect_publication(request)
    }

    /// Restore a validated publication into the selected task registry.
    pub fn restore_task_publication(
        &self,
        request: PublicationRestoreRequest,
    ) -> Result<PublicationRestoreOutcome, OrbitError> {
        let registry = self.open_publication_task_registry()?;
        restore_publication(&registry, request)
    }
}
