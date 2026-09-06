use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspacePublicationBinding,
    WorkspaceRegistry, WorkspaceStatus,
};

use crate::workspace_registry::rebind_workspace_source_remote;

fn timestamp() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 18, 1, 2, 3)
        .single()
        .expect("fixed timestamp")
}

fn logical_workspace(id: &str, owner_machine_id: Option<&str>) -> Workspace {
    Workspace {
        id: id.to_string(),
        name: id.trim_start_matches("ws_").to_string(),
        owner_machine_id: owner_machine_id.map(str::to_string),
        git_remote: Some("git@example.test:orbit/repo.git".to_string()),
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: timestamp(),
        updated_at: timestamp(),
    }
}

fn owned_registry() -> WorkspaceRegistry {
    WorkspaceRegistry {
        owner_host_ids: [("hm_owner".to_string(), "owner-host".to_string())]
            .into_iter()
            .collect(),
        workspaces: vec![logical_workspace("ws_orbit", Some("hm_owner"))],
        checkouts: vec![WorkspaceCheckout::owner(
            "ws_orbit".to_string(),
            PathBuf::from("/repos/orbit"),
            PathBuf::from("/repos/orbit/.orbit"),
        )],
        ..Default::default()
    }
}

#[test]
fn source_remote_rebind_changes_only_remote_and_updated_at() {
    let mut registry = owned_registry();
    let before = registry.clone();

    let outcome = rebind_workspace_source_remote(
        &mut registry,
        "orbit",
        "ssh://github.com/example/orbit-renamed.git",
        Some("hm_owner"),
        false,
    )
    .expect("rebind source remote");

    assert!(outcome.changed);
    assert!(!outcome.dry_run);
    assert_eq!(outcome.workspace_id, "ws_orbit");
    assert_eq!(outcome.old_remote, "git@example.test:orbit/repo.git");
    assert_eq!(
        outcome.old_repository_identity.as_deref(),
        Some("example.test/orbit/repo")
    );
    assert_eq!(
        outcome.new_repository_identity,
        "github.com/example/orbit-renamed"
    );
    assert_eq!(
        registry.workspaces[0].git_remote.as_deref(),
        Some("ssh://github.com/example/orbit-renamed.git")
    );
    assert!(registry.workspaces[0].updated_at >= before.workspaces[0].updated_at);

    let mut expected_workspace = before.workspaces[0].clone();
    expected_workspace.git_remote = registry.workspaces[0].git_remote.clone();
    expected_workspace.updated_at = registry.workspaces[0].updated_at;
    assert_eq!(registry.workspaces[0], expected_workspace);
    assert_eq!(registry.checkouts, before.checkouts);
    assert_eq!(registry.owner_host_ids, before.owner_host_ids);
}

#[test]
fn source_remote_rebind_equivalent_retry_is_a_no_op() {
    let mut registry = owned_registry();
    registry.workspaces[0].git_remote = Some("git@github.com:Example/Orbit.git".to_string());
    let before = registry.clone();

    let outcome = rebind_workspace_source_remote(
        &mut registry,
        "ws_orbit",
        "https://github.com/example/orbit.git",
        Some("hm_owner"),
        false,
    )
    .expect("equivalent retry");

    assert!(!outcome.changed);
    assert_eq!(registry, before);
}

#[test]
fn source_remote_rebind_rejects_invalid_and_credential_bearing_inputs_before_mutation() {
    for remote in [
        "/srv/git/orbit.git",
        "origin",
        "https://operator:secret@github.com/example/orbit.git",
    ] {
        let mut registry = owned_registry();
        let before = registry.clone();
        let error = rebind_workspace_source_remote(
            &mut registry,
            "ws_orbit",
            remote,
            Some("hm_owner"),
            false,
        )
        .expect_err("invalid remote must fail");

        assert_eq!(
            registry, before,
            "rejected input mutated registry: {remote}"
        );
        assert!(
            error.to_string().contains("must") || error.to_string().contains("valid Git URL"),
            "unexpected error: {error}"
        );
        assert!(
            !error.to_string().contains("operator:secret"),
            "credential leaked in error: {error}"
        );
    }
}

#[test]
fn source_remote_rebind_denies_replica_and_non_owner_before_mutation() {
    let mut replica = owned_registry();
    replica.checkouts[0].role = Some(WorkspaceCheckoutRole::Replica);
    replica.checkouts[0].owner_machine_id = Some("hm_owner".to_string());
    let before_replica = replica.clone();
    let error = rebind_workspace_source_remote(
        &mut replica,
        "ws_orbit",
        "ssh://github.com/example/orbit-renamed.git",
        Some("hm_replica"),
        false,
    )
    .expect_err("replica must be denied");
    assert!(error.to_string().contains("replica checkout"), "{error}");
    assert_eq!(replica, before_replica);

    let mut wrong_owner = owned_registry();
    let before_owner = wrong_owner.clone();
    let error = rebind_workspace_source_remote(
        &mut wrong_owner,
        "ws_orbit",
        "ssh://github.com/example/orbit-renamed.git",
        Some("hm_other"),
        false,
    )
    .expect_err("non-owner must be denied");
    assert!(error.to_string().contains("cannot rebind"), "{error}");
    assert_eq!(wrong_owner, before_owner);
}

#[test]
fn source_remote_rebind_dry_run_reports_change_without_mutation() {
    let mut registry = owned_registry();
    let before = registry.clone();

    let outcome = rebind_workspace_source_remote(
        &mut registry,
        "ws_orbit",
        "ssh://github.com/example/orbit-renamed.git",
        Some("hm_owner"),
        true,
    )
    .expect("dry run");

    assert!(outcome.changed);
    assert!(outcome.dry_run);
    assert_eq!(registry, before);
}

#[test]
fn source_remote_rebind_refuses_publication_lineage_change_before_mutation() {
    let mut registry = owned_registry();
    registry
        .publication_bindings
        .push(WorkspacePublicationBinding {
            workspace_id: "ws_orbit".to_string(),
            source_repository_fingerprint: "git@example.test:orbit/repo.git".to_string(),
            publication_remote: "ssh://publication.example.test/orbit-tasks.git".to_string(),
            publication_branch: "refs/heads/main".to_string(),
            publication_id: "pub_orbit".to_string(),
            authority_machine_id: "hm_owner".to_string(),
            last_success_generation: Some(3),
            last_success_commit: Some("a".repeat(40)),
        });
    let before = registry.clone();

    let error = rebind_workspace_source_remote(
        &mut registry,
        "ws_orbit",
        "ssh://github.com/example/orbit-renamed.git",
        Some("hm_owner"),
        false,
    )
    .expect_err("publication binding must fail closed");
    let message = error.to_string();

    assert!(message.contains("publication show --json"), "{message}");
    assert!(
        message.contains("publication remove --confirm"),
        "{message}"
    );
    assert!(
        message.contains("create a new binding explicitly"),
        "{message}"
    );
    assert_eq!(registry, before);

    let error = rebind_workspace_source_remote(
        &mut registry,
        "ws_orbit",
        "ssh://github.com/example/orbit-renamed.git",
        Some("hm_owner"),
        true,
    )
    .expect_err("dry run must report the same publication blocker");
    assert!(error.to_string().contains("publication show --json"));
    assert_eq!(registry, before);
}
