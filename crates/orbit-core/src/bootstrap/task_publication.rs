//! Runtime facades over the `orbit-store` task-publication workflows.
//!
//! Operator commands and scheduled publication share binding resolution,
//! authority checks, the Store publisher and last-success bookkeeping.

use std::path::PathBuf;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_registry::{load_machine_identity, workspace_registry};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_store::workflow::task::{
    inspect_publication, publish_task_snapshot, restore_publication,
};
use orbit_types::workspace::{WorkspaceCheckoutRole, WorkspacePublicationBinding};
use serde_json::{Value, json};

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
    /// Publish this runtime's selected workspace, or return `None` if unbound.
    /// Both manual and scheduled publication use the same authority and
    /// destination checks and record success only against an unchanged binding.
    pub fn publish_bound_task_publication(
        &self,
        policy: &AttachmentPolicy,
    ) -> Result<Option<(WorkspacePublicationBinding, PublicationPublishOutcome)>, OrbitError> {
        let workspace_id = self
            .workspace_runtime_binding()
            .map(|binding| binding.logical_workspace_id.clone())
            .map_or_else(|| self.workspace_id(), Ok)?;
        let task_workspace_id = self
            .workspace_runtime_binding()
            .map(|binding| binding.task_partition_id.clone())
            .map_or_else(|| self.workspace_id(), Ok)?;
        let global_root = self.global_root();
        let machine = load_machine_identity(&global_root)?;
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let registry = workspace_registry::load_registry_from_read_only(&registry_path)?.registry;
        let Some(binding) =
            workspace_registry::find_publication_binding_by_id(&registry, &workspace_id).cloned()
        else {
            return Ok(None);
        };
        let checkout = workspace_registry::find_checkout_by_id(&registry, &workspace_id)
            .ok_or_else(|| {
                OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' has no local checkout"
                ))
            })?;
        let caller_role = match checkout.role {
            Some(WorkspaceCheckoutRole::Owner) => PublicationCallerRole::Owner,
            Some(WorkspaceCheckoutRole::Replica) | None => PublicationCallerRole::Replica,
        };

        let request = publish_request(
            &binding,
            task_workspace_id,
            machine.id,
            caller_role,
            global_root.join("state").join("task-publication"),
        );
        let outcome = self.publish_task_publication(request, policy)?;
        record_success_at_registry_path(&registry_path, &binding, &outcome)?;
        Ok(Some((binding, outcome)))
    }

    pub(crate) fn run_task_publication(&self) -> Result<Value, OrbitError> {
        let settings = self.context.settings();
        let kind = match settings.publication_routine_attachments() {
            "omit" => AttachmentPolicyKind::Omit,
            "fail" => AttachmentPolicyKind::Fail,
            "include" => AttachmentPolicyKind::Include,
            _ => {
                return Err(OrbitError::InvalidInput(
                    "invalid routine attachment policy".into(),
                ));
            }
        };
        let policy = AttachmentPolicy {
            kind,
            max_file_bytes: 10 * 1024 * 1024,
            max_total_bytes: 100 * 1024 * 1024,
            deny_patterns: [
                "**/.env",
                "**/.env.*",
                "**/*.pem",
                "**/*.key",
                "**/id_rsa",
                "**/credentials.json",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            scanner_failure_behavior: if settings.publication_routine_allow_unscanned_attachments()
            {
                ScannerFailureBehavior::AllowUnchecked
            } else {
                ScannerFailureBehavior::Reject
            },
        };
        let Some((_, outcome)) = self.publish_bound_task_publication(&policy)? else {
            return Ok(json!({ "skipped": "publication_unbound" }));
        };
        Ok(json!({
            "status": match outcome.status {
                PublicationPublishStatus::Initialized => "initialized",
                PublicationPublishStatus::Advanced => "advanced",
                PublicationPublishStatus::Reconciled => "reconciled",
                PublicationPublishStatus::Unchanged => "unchanged",
            },
            "generation": outcome.generation,
            "commit_id": outcome.commit_id,
        }))
    }

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

// External publication deliberately runs without the catalog lock. Revalidate
// its destination on a fresh snapshot before recording the completed publish.
fn record_success_at_registry_path(
    registry_path: &std::path::Path,
    expected: &WorkspacePublicationBinding,
    outcome: &PublicationPublishOutcome,
) -> Result<(), OrbitError> {
    workspace_registry::with_registry_lock(registry_path, || {
        let mut registry = workspace_registry::load_registry_from(registry_path)?;
        let current =
            workspace_registry::find_publication_binding_by_id(&registry, &expected.workspace_id);
        let same_destination = current.is_some_and(|current| {
            current.workspace_id == expected.workspace_id
                && current.source_repository_fingerprint == expected.source_repository_fingerprint
                && current.publication_remote == expected.publication_remote
                && current.publication_branch == expected.publication_branch
                && current.publication_id == expected.publication_id
                && current.authority_machine_id == expected.authority_machine_id
        });
        if !same_destination {
            return Err(OrbitError::WorkspaceError(
                "publication completed remotely, but its local binding changed; success was not recorded".to_string(),
            ));
        }
        workspace_registry::record_publication_success_by_id(
            &mut registry,
            &expected.workspace_id,
            outcome.generation,
            &outcome.commit_id,
            Some(&expected.authority_machine_id),
        )?;
        workspace_registry::save_registry_to(&registry, registry_path)
    })
}

fn publish_request(
    binding: &WorkspacePublicationBinding,
    task_workspace_id: String,
    local_machine_id: String,
    caller_role: PublicationCallerRole,
    cache_dir: PathBuf,
) -> PublicationPublishRequest {
    PublicationPublishRequest {
        workspace_id: binding.workspace_id.clone(),
        task_workspace_id,
        source_repository_fingerprint: binding.source_repository_fingerprint.clone(),
        publication_id: binding.publication_id.clone(),
        authority_machine_id: binding.authority_machine_id.clone(),
        local_machine_id,
        caller_role,
        publication_remote: binding.publication_remote.clone(),
        publication_branch: binding.publication_branch.clone(),
        cache_dir,
        published_at: Utc::now(),
        last_success: binding
            .last_success_generation
            .zip(binding.last_success_commit.as_ref())
            .map(|(generation, commit)| PublicationLastSuccess {
                generation,
                commit: commit.clone(),
            }),
    }
}
