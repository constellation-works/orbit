use crate::workspace_registry::bind_publication;
use chrono::{TimeZone, Utc};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};

fn timestamp() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 29, 1, 2, 3)
        .single()
        .expect("fixed timestamp")
}

fn logical_workspace(id: &str, owner: &str, git_remote: &str) -> Workspace {
    Workspace {
        id: id.to_string(),
        name: id.trim_start_matches("ws_").to_string(),
        owner_machine_id: Some(owner.to_string()),
        git_remote: Some(git_remote.to_string()),
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: timestamp(),
        updated_at: timestamp(),
    }
}

fn owner_checkout(workspace_id: &str) -> WorkspaceCheckout {
    WorkspaceCheckout::owner(
        workspace_id.to_string(),
        format!("/repos/{workspace_id}").into(),
        format!("/repos/{workspace_id}/.orbit").into(),
    )
}

fn owner_registry() -> WorkspaceRegistry {
    WorkspaceRegistry {
        workspaces: vec![logical_workspace(
            "ws_orbit",
            "hm_owner",
            "git@github.com:example/source.git",
        )],
        checkouts: vec![owner_checkout("ws_orbit")],
        ..WorkspaceRegistry::default()
    }
}

fn assert_redacted(message: &str) {
    assert!(
        !message.contains("ghp_s3cret")
            && !message.contains("/repos/")
            && !message.contains("/home/"),
        "diagnostic leaked a secret or checkout path: {message}"
    );
}

#[test]
fn bind_rejects_replica_equivalent_remote_credentials_branch_authority_and_reused_ids() {
    let mut registry = owner_registry();
    registry.workspaces.push(logical_workspace(
        "ws_other",
        "hm_owner",
        "git@github.com:example/other-source.git",
    ));
    registry.checkouts.push(owner_checkout("ws_other"));

    bind_publication(
        &mut registry,
        "ws_other",
        "git@github.com:example/other-tasks.git",
        "refs/heads/main",
        "tp_shared",
        Some("hm_owner"),
    )
    .expect("first lineage");

    let replica = bind_publication(
        &mut WorkspaceRegistry {
            checkouts: vec![WorkspaceCheckout {
                workspace_id: "ws_orbit".to_string(),
                repo_root: "/repos/ws_orbit".into(),
                orbit_dir: "/repos/ws_orbit/.orbit".into(),
                role: Some(WorkspaceCheckoutRole::Replica),
                owner_machine_id: Some("hm_owner".to_string()),
                path_overrides: Vec::new(),
            }],
            ..owner_registry()
        },
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "refs/heads/main",
        "tp_replica",
        Some("hm_other"),
    )
    .expect_err("replica")
    .to_string();
    assert!(replica.contains("replica checkout"), "{replica}");
    assert_redacted(&replica);

    let equivalent = bind_publication(
        &mut registry,
        "ws_orbit",
        "https://github.com/example/source.git",
        "refs/heads/main",
        "tp_same_repo",
        Some("hm_owner"),
    )
    .expect_err("source-equivalent remote")
    .to_string();
    assert!(
        equivalent.contains("equivalent to the workspace source remote"),
        "{equivalent}"
    );
    assert_redacted(&equivalent);

    let credentials = bind_publication(
        &mut registry,
        "ws_orbit",
        "https://x-access-token:ghp_s3cret@github.com/example/tasks.git",
        "refs/heads/main",
        "tp_secret",
        Some("hm_owner"),
    )
    .expect_err("credentials")
    .to_string();
    assert!(credentials.contains("credentials"), "{credentials}");
    assert_redacted(&credentials);

    let branch = bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "refs/tags/v1",
        "tp_tag",
        Some("hm_owner"),
    )
    .expect_err("tag ref")
    .to_string();
    assert!(branch.contains("ordinary refs/heads"), "{branch}");

    let authority = bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "refs/heads/main",
        "tp_authority",
        Some("hm_other"),
    )
    .expect_err("wrong machine")
    .to_string();
    assert!(authority.contains("declared owner machine"), "{authority}");
    assert_redacted(&authority);

    let reused = bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "refs/heads/main",
        "tp_shared",
        Some("hm_owner"),
    )
    .expect_err("reused lineage")
    .to_string();
    assert!(
        reused.contains("already bound to workspace 'ws_other'"),
        "{reused}"
    );
}
