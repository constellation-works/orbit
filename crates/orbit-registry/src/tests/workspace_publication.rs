use std::fs;
use std::path::Path;

use chrono::{TimeZone, Utc};
use orbit_types::workspace::{
    WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace, WorkspaceCheckout, WorkspaceCheckoutRole,
    WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::workspace_registry::{
    bind_publication, bind_publication_by_id, find_publication_binding,
    find_publication_binding_by_id, load_registry_from, rebind_publication,
    rebind_publication_by_id, record_publication_success, record_publication_success_by_id,
    save_registry_to, unbind_publication, unbind_publication_by_id,
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

fn write_host_identity(root: &Path, machine_id: &str) {
    fs::write(
        root.join("host.toml"),
        format!(
            "schema_version = 2\nmachine_id = \"{machine_id}\"\nhost_id = \"test-host\"\ntask_prefix = \"ORB\"\n"
        ),
    )
    .expect("write host identity");
}

fn write_json(path: &Path, value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(value).expect("serialize fixture");
    fs::write(path, &bytes).expect("write fixture");
    bytes
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
fn bind_round_trips_through_atomic_save_and_rebind_is_the_only_replace_path() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "hm_owner");
    let path = root.path().join("workspaces.json");
    let mut registry = owner_registry();

    let created = bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect("bind");
    assert_eq!(created.workspace_id, "ws_orbit");
    assert_eq!(
        created.source_repository_fingerprint,
        "git@github.com:example/source.git"
    );
    assert_eq!(created.publication_branch, "refs/heads/main");
    assert_eq!(created.authority_machine_id, "hm_owner");
    assert_eq!(created.last_success_generation, None);

    let duplicate = bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/other.git",
        "refs/heads/main",
        "tp_other",
        Some("hm_owner"),
    )
    .expect_err("second bind must use rebind")
    .to_string();
    assert!(
        duplicate.contains("already has a publication binding"),
        "{duplicate}"
    );
    assert!(duplicate.contains("rebind"), "{duplicate}");

    record_publication_success(
        &mut registry,
        "ws_orbit",
        3,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("hm_owner"),
    )
    .expect("record success");
    save_registry_to(&registry, &path).expect("save binding");

    let loaded = load_registry_from(&path).expect("reload");
    let binding = find_publication_binding(&loaded, "orbit")
        .expect("lookup by name")
        .expect("find binding");
    assert_eq!(binding.publication_id, "tp_orbit_tasks");
    assert_eq!(binding.last_success_generation, Some(3));

    let mut rebound = loaded;
    let replaced = rebind_publication(
        &mut rebound,
        "ws_orbit",
        "https://github.com/example/tasks-v2.git",
        "refs/heads/publication",
        "tp_orbit_tasks_v2",
        Some("hm_owner"),
    )
    .expect("rebind");
    assert_eq!(
        replaced.publication_remote,
        "https://github.com/example/tasks-v2.git"
    );
    assert_eq!(replaced.last_success_generation, None);
    save_registry_to(&rebound, &path).expect("save rebind");

    let mut unbound = load_registry_from(&path).expect("reload rebound");
    let removed = unbind_publication(&mut unbound, "ws_orbit", Some("hm_owner")).expect("unbind");
    assert_eq!(removed.publication_id, "tp_orbit_tasks_v2");
    save_registry_to(&unbound, &path).expect("save unbind");
    let empty = load_registry_from(&path).expect("reload empty");
    assert!(
        find_publication_binding(&empty, "ws_orbit")
            .expect("lookup empty registry")
            .is_none()
    );
    let persisted: Value =
        serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse saved registry");
    assert!(persisted.get("publication_bindings").is_none());
}

#[test]
fn existing_registry_without_publication_bindings_loads_and_saves_without_data_loss() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "hm_owner");
    let path = root.path().join("workspaces.json");
    write_json(
        &path,
        &json!({
            "schema_version": WORKSPACE_REGISTRY_SCHEMA_VERSION,
            "workspaces": [logical_workspace(
                "ws_orbit",
                "hm_owner",
                "git@github.com:example/source.git"
            )],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/ws_orbit",
                "orbit_dir": "/repos/ws_orbit/.orbit",
                "role": "owner"
            }]
        }),
    );

    let loaded = load_registry_from(&path).expect("load legacy registry");
    assert!(loaded.publication_bindings.is_empty());
    assert_eq!(loaded.workspaces[0].id, "ws_orbit");
    assert_eq!(
        loaded.workspaces[0].git_remote.as_deref(),
        Some("git@github.com:example/source.git")
    );
    save_registry_to(&loaded, &path).expect("save without bindings");
    let persisted: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse");
    assert!(persisted.get("publication_bindings").is_none());
    assert_eq!(persisted["workspaces"][0]["id"], "ws_orbit");
    assert_eq!(
        persisted["workspaces"][0]["git_remote"],
        "git@github.com:example/source.git"
    );
}

#[test]
fn malformed_publication_bindings_fail_closed_without_rewriting() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "hm_owner");
    let path = root.path().join("workspaces.json");
    let original = write_json(
        &path,
        &json!({
            "schema_version": WORKSPACE_REGISTRY_SCHEMA_VERSION,
            "workspaces": [logical_workspace(
                "ws_orbit",
                "hm_owner",
                "git@github.com:example/source.git"
            )],
            "checkouts": [{
                "workspace_id": "ws_orbit",
                "repo_root": "/repos/ws_orbit",
                "orbit_dir": "/repos/ws_orbit/.orbit",
                "role": "owner"
            }],
            "publication_bindings": [{
                "workspace_id": "ws_orbit",
                "source_repository_fingerprint": "git@github.com:example/source.git",
                "publication_remote": "https://x-access-token:ghp_s3cret@github.com/example/tasks.git",
                "publication_branch": "refs/heads/main",
                "publication_id": "tp_orbit_tasks",
                "authority_machine_id": "hm_owner"
            }]
        }),
    );

    let error = load_registry_from(&path)
        .expect_err("credential-bearing binding must fail")
        .to_string();
    assert!(error.contains("credentials"), "{error}");
    assert_redacted(&error);
    assert_eq!(fs::read(&path).expect("unchanged"), original);
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

#[test]
fn rejected_bind_leaves_persisted_registry_byte_identical() {
    let root = tempdir().expect("tempdir");
    write_host_identity(root.path(), "hm_owner");
    let path = root.path().join("workspaces.json");
    let mut registry = owner_registry();
    bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "refs/heads/main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect("bind");
    save_registry_to(&registry, &path).expect("save");
    let original = fs::read(&path).expect("original bytes");

    let mut dirty = load_registry_from(&path).expect("load");
    let error = rebind_publication(
        &mut dirty,
        "ws_orbit",
        "/repos/ws_orbit",
        "refs/heads/main",
        "tp_path",
        Some("hm_owner"),
    )
    .expect_err("local path")
    .to_string();
    assert!(error.contains("local checkout path"), "{error}");
    assert_redacted(&error);
    assert_eq!(
        fs::read(&path).expect("still original"),
        original,
        "failed rebind must not persist"
    );
}

#[test]
fn record_publication_success_is_idempotent_across_commit_hex_case() {
    const UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
    const LOWER: &str = "abcdef0123456789abcdef0123456789abcdef01";

    let mut upper_then_lower = owner_registry();
    bind_publication(
        &mut upper_then_lower,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect("bind");
    let first = record_publication_success(
        &mut upper_then_lower,
        "ws_orbit",
        3,
        UPPER,
        Some("hm_owner"),
    )
    .expect("record uppercase");
    let second = record_publication_success(
        &mut upper_then_lower,
        "ws_orbit",
        3,
        LOWER,
        Some("hm_owner"),
    )
    .expect("retry lowercase");
    assert_eq!(first, second);
    assert_eq!(first.last_success_generation, Some(3));
    assert_eq!(first.last_success_commit.as_deref(), Some(LOWER));

    let mut lower_then_upper = owner_registry();
    bind_publication(
        &mut lower_then_upper,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect("bind");
    let first = record_publication_success(
        &mut lower_then_upper,
        "ws_orbit",
        3,
        LOWER,
        Some("hm_owner"),
    )
    .expect("record lowercase");
    let second = record_publication_success(
        &mut lower_then_upper,
        "ws_orbit",
        3,
        UPPER,
        Some("hm_owner"),
    )
    .expect("retry uppercase");
    assert_eq!(first, second);
    assert_eq!(first.last_success_generation, Some(3));
    assert_eq!(first.last_success_commit.as_deref(), Some(LOWER));
}

#[test]
fn publication_lifecycle_survives_workspace_id_and_name_collision() {
    let mut registry = owner_registry();
    registry.workspaces.push(Workspace {
        id: "ws_other".to_string(),
        name: "ws_orbit".to_string(),
        owner_machine_id: Some("hm_owner".to_string()),
        git_remote: Some("git@github.com:example/other.git".to_string()),
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: timestamp(),
        updated_at: timestamp(),
    });
    registry.checkouts.push(owner_checkout("ws_other"));

    // 1. Binding unambiguously by name "orbit" must succeed.
    let bound = bind_publication(
        &mut registry,
        "orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect("bind publication by unambiguous name must succeed");
    assert_eq!(bound.workspace_id, "ws_orbit");

    // 2. Lookup unambiguously by name "orbit" must succeed.
    let found = find_publication_binding(&registry, "orbit")
        .expect("lookup by name")
        .expect("found binding");
    assert_eq!(found.workspace_id, "ws_orbit");

    // 3. Recording publication success unambiguously by name "orbit" must succeed.
    let recorded = record_publication_success(
        &mut registry,
        "orbit",
        1,
        "1111111111111111111111111111111111111111",
        Some("hm_owner"),
    )
    .expect("record publication success by unambiguous name must succeed");
    assert_eq!(recorded.last_success_generation, Some(1));

    // 4. Rebinding unambiguously by name "orbit" must succeed.
    let rebound = rebind_publication(
        &mut registry,
        "orbit",
        "https://github.com/example/tasks-v2.git",
        "refs/heads/publication",
        "tp_orbit_tasks_v2",
        Some("hm_owner"),
    )
    .expect("rebind publication by unambiguous name must succeed");
    assert_eq!(rebound.publication_id, "tp_orbit_tasks_v2");

    // 5. Unbinding unambiguously by name "orbit" must succeed.
    let unbound = unbind_publication(&mut registry, "orbit", Some("hm_owner"))
        .expect("unbind publication by unambiguous name must succeed");
    assert_eq!(unbound.workspace_id, "ws_orbit");

    // 6. Operator-typed colliding selector "ws_orbit" genuinely matches two workspaces
    // and must fail closed with the ambiguous-selector error.
    let err = bind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect_err("colliding selector must fail closed on bind")
    .to_string();
    assert!(err.contains("ambiguous workspace selector"), "{err}");

    let err = find_publication_binding(&registry, "ws_orbit")
        .expect_err("colliding selector must fail closed on find")
        .to_string();
    assert!(err.contains("ambiguous workspace selector"), "{err}");

    let err = rebind_publication(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks",
        Some("hm_owner"),
    )
    .expect_err("colliding selector must fail closed on rebind")
    .to_string();
    assert!(err.contains("ambiguous workspace selector"), "{err}");

    let err = unbind_publication(&mut registry, "ws_orbit", Some("hm_owner"))
        .expect_err("colliding selector must fail closed on unbind")
        .to_string();
    assert!(err.contains("ambiguous workspace selector"), "{err}");

    let err = record_publication_success(
        &mut registry,
        "ws_orbit",
        2,
        "2222222222222222222222222222222222222222",
        Some("hm_owner"),
    )
    .expect_err("colliding selector must fail closed on record_publication_success")
    .to_string();
    assert!(err.contains("ambiguous workspace selector"), "{err}");

    // 7. Direct exact-ID publication operations for "ws_orbit" succeed even in the presence of the collision.
    let by_id_bound = bind_publication_by_id(
        &mut registry,
        "ws_orbit",
        "git@github.com:example/tasks.git",
        "main",
        "tp_orbit_tasks_exact",
        Some("hm_owner"),
    )
    .expect("bind publication by exact id must succeed");
    assert_eq!(by_id_bound.workspace_id, "ws_orbit");

    let by_id_found = find_publication_binding_by_id(&registry, "ws_orbit")
        .expect("find publication binding by exact id");
    assert_eq!(by_id_found.publication_id, "tp_orbit_tasks_exact");

    let by_id_recorded = record_publication_success_by_id(
        &mut registry,
        "ws_orbit",
        2,
        "2222222222222222222222222222222222222222",
        Some("hm_owner"),
    )
    .expect("record publication success by exact id must succeed");
    assert_eq!(by_id_recorded.last_success_generation, Some(2));

    let by_id_rebound = rebind_publication_by_id(
        &mut registry,
        "ws_orbit",
        "https://github.com/example/tasks-v2.git",
        "refs/heads/publication",
        "tp_orbit_tasks_exact_v2",
        Some("hm_owner"),
    )
    .expect("rebind publication by exact id must succeed");
    assert_eq!(by_id_rebound.publication_id, "tp_orbit_tasks_exact_v2");

    let by_id_unbound = unbind_publication_by_id(&mut registry, "ws_orbit", Some("hm_owner"))
        .expect("unbind publication by exact id must succeed");
    assert_eq!(by_id_unbound.workspace_id, "ws_orbit");
}
