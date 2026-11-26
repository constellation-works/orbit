//! The task-publication workflow: publish the bound workspace's snapshot,
//! compare it with the remote, read a snapshot and restore one.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_registry::{load_machine_identity, workspace_registry};
use orbit_types::workspace::WorkspaceCheckoutRole;

use super::{Outcome, Publication, PublicationSource};
use crate::OrbitRuntime;
use crate::application::task_bundle::publication::{
    PublicationInspectRequest, PublicationRestoreMode, PublicationRestoreRequest,
};

pub(super) fn run(runtime: &OrbitRuntime, command: Publication) -> Result<Outcome, OrbitError> {
    match command {
        Publication::Publish { policy } => {
            let workspace_id = selected_workspace_id(runtime)?;
            runtime
                .publish_bound_task_publication(&policy)?
                .map(|published| Outcome::Published(Box::new(published)))
                .ok_or_else(|| {
                    OrbitError::WorkspaceError(format!(
                        "workspace '{workspace_id}' has no publication binding; run `orbit workspace publication bind` first"
                    ))
                })
        }
        Publication::Status => status(runtime),
        Publication::Inspect(source) => runtime
            .inspect_task_publication(inspect_request(runtime, &source, "inspect"))
            .map(|inspection| Outcome::Inspected(Box::new(inspection))),
        Publication::Restore {
            source,
            allow_identical_retry,
        } => {
            assert_restore_authority(runtime, &source)?;
            let task_workspace_id = runtime
                .workspace_runtime_binding()
                .map(|binding| binding.task_partition_id.clone())
                .map_or_else(|| runtime.workspace_id(), Ok)?;
            runtime.record_task_publication_source(
                &task_workspace_id,
                &source.source_repository_fingerprint,
            )?;
            runtime
                .restore_task_publication(PublicationRestoreRequest {
                    task_workspace_id,
                    publication: inspect_request(runtime, &source, "restore"),
                    mode: if allow_identical_retry {
                        PublicationRestoreMode::AllowIdenticalRetry
                    } else {
                        PublicationRestoreMode::EmptyDestination
                    },
                })
                .map(Outcome::Restored)
        }
    }
}

/// The bound publication's record, and the remote's snapshot once one was
/// published.
fn status(runtime: &OrbitRuntime) -> Result<Outcome, OrbitError> {
    let workspace_id = selected_workspace_id(runtime)?;
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &runtime.global_root(),
    ))?;
    let binding = workspace_registry::find_publication_binding_by_id(&registry, &workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no publication binding"
            ))
        })?;
    let inspection = match binding.last_success_commit {
        None => None,
        Some(_) => Some(runtime.inspect_task_publication(PublicationInspectRequest {
            workspace_id: binding.workspace_id.clone(),
            source_repository_fingerprint: binding.source_repository_fingerprint.clone(),
            publication_id: binding.publication_id.clone(),
            authority_machine_id: binding.authority_machine_id.clone(),
            publication_remote: binding.publication_remote.clone(),
            publication_branch: binding.publication_branch.clone(),
            cache_dir: publication_cache(runtime),
            commit: None,
        })?),
    };
    Ok(Outcome::PublicationStatus(Box::new((
        binding.clone(),
        inspection,
    ))))
}

fn selected_workspace_id(runtime: &OrbitRuntime) -> Result<String, OrbitError> {
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .map_or_else(|| runtime.workspace_id(), Ok)
}

fn publication_cache(runtime: &OrbitRuntime) -> PathBuf {
    runtime.global_root().join("state").join("task-publication")
}

fn inspect_request(
    runtime: &OrbitRuntime,
    source: &PublicationSource,
    cache_leaf: &str,
) -> PublicationInspectRequest {
    PublicationInspectRequest {
        workspace_id: source.workspace_id.clone(),
        source_repository_fingerprint: source.source_repository_fingerprint.clone(),
        publication_id: source.publication_id.clone(),
        authority_machine_id: source.authority_machine_id.clone(),
        publication_remote: source.remote.clone(),
        publication_branch: source.branch.clone(),
        cache_dir: publication_cache(runtime).join(cache_leaf),
        commit: source.commit.clone(),
    }
}

/// A restore writes the canonical store, so it is refused unless this
/// machine is the publication's authority and holds the workspace's owner
/// checkout.
fn assert_restore_authority(
    runtime: &OrbitRuntime,
    expected: &PublicationSource,
) -> Result<(), OrbitError> {
    let selected = selected_workspace_id(runtime)?;
    if selected != expected.workspace_id {
        return Err(OrbitError::PolicyDenied(format!(
            "publication restore targets workspace '{}', but the selected destination is '{selected}'",
            expected.workspace_id
        )));
    }
    let global_root = runtime.global_root();
    let local_machine_id = load_machine_identity(&global_root)?.id;
    if local_machine_id != expected.authority_machine_id {
        return Err(OrbitError::PolicyDenied(format!(
            "publication restore authority '{}' does not match local machine '{}'",
            expected.authority_machine_id, local_machine_id
        )));
    }
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &global_root,
    ))?;
    let workspace =
        workspace_registry::find_workspace_by_id(&registry, &selected).ok_or_else(|| {
            OrbitError::WorkspaceError(format!("workspace '{selected}' is not registered"))
        })?;
    if workspace.owner_machine_id.as_deref() != Some(local_machine_id.as_str()) {
        return Err(OrbitError::PolicyDenied(format!(
            "workspace '{selected}' is not owned by local machine '{local_machine_id}'"
        )));
    }
    let checkout =
        workspace_registry::find_checkout_by_id(&registry, &selected).ok_or_else(|| {
            OrbitError::WorkspaceError(format!("workspace '{selected}' has no local checkout"))
        })?;
    if checkout.role != Some(WorkspaceCheckoutRole::Owner) {
        return Err(OrbitError::PolicyDenied(format!(
            "workspace '{selected}' is a replica checkout; restore requires the declared owner destination"
        )));
    }
    if workspace.git_remote.as_deref() != Some(expected.source_repository_fingerprint.as_str()) {
        return Err(OrbitError::PolicyDenied(format!(
            "publication restore source remote does not match selected workspace '{selected}'"
        )));
    }
    Ok(())
}
