use tempfile::tempdir;

use orbit_types::workspace::WorkspaceCheckoutRole;

use crate::tests::env_isolation::EnvGuard;

use super::super::init::WorkspaceInitArgs;
use super::super::role::assign_role_at;

fn init_args(name: &str) -> WorkspaceInitArgs {
    WorkspaceInitArgs {
        name: Some(name.to_string()),
        base_branch: Some("main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
}

/// Registers an owner checkout on a host whose machine id is `hm_local`, then
/// runs `check` against its registry path while the isolated environment is
/// still in place.
fn with_registered_owner(check: impl FnOnce(&std::path::Path)) {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_local\"\nname = \"local\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    init_args("roles")
        .execute_without_runtime(None)
        .expect("register the owner checkout");
    check(&global.join("workspaces.json"));
}

#[test]
fn replica_without_an_owner_is_refused_naming_the_owner_flag() {
    with_registered_owner(|registry| {
        let error = assign_role_at(
            registry,
            "ws_roles",
            WorkspaceCheckoutRole::Replica,
            None,
            Some("hm_local"),
        )
        .expect_err("a replica declaration needs an owner")
        .to_string();
        assert!(error.contains("--owner"), "{error}");
    });
}

#[test]
fn owner_role_with_an_owner_flag_is_refused_naming_the_flag_to_drop() {
    with_registered_owner(|registry| {
        let error = assign_role_at(
            registry,
            "ws_roles",
            WorkspaceCheckoutRole::Owner,
            Some("hm_other"),
            Some("hm_local"),
        )
        .expect_err("the owner role takes no owner")
        .to_string();
        assert!(error.contains("--owner"), "{error}");
    });
}

#[test]
fn a_malformed_owner_id_says_where_a_valid_one_comes_from() {
    with_registered_owner(|registry| {
        let error = assign_role_at(
            registry,
            "ws_roles",
            WorkspaceCheckoutRole::Replica,
            Some("not-a-machine"),
            Some("hm_local"),
        )
        .expect_err("owner id must be a machine id")
        .to_string();
        assert!(error.contains("--owner"), "{error}");
        assert!(error.contains("machine.id"), "{error}");
    });
}

#[test]
fn the_local_machine_as_replica_owner_is_refused_naming_the_owner_flag() {
    with_registered_owner(|registry| {
        let error = assign_role_at(
            registry,
            "ws_roles",
            WorkspaceCheckoutRole::Replica,
            Some("hm_local"),
            Some("hm_local"),
        )
        .expect_err("a machine cannot be its own replica owner")
        .to_string();
        assert!(error.contains("--owner"), "{error}");
    });
}

#[test]
fn a_recorded_role_reports_the_same_facts_in_json_and_text() {
    with_registered_owner(|registry| {
        let assignment = assign_role_at(
            registry,
            "roles",
            WorkspaceCheckoutRole::Owner,
            None,
            Some("hm_local"),
        )
        .expect("owner role is idempotent");
        let doc = assignment.to_json();
        // The selector may be a name, the report carries the logical id.
        assert_eq!(doc["workspace_id"], "ws_roles");
        assert_eq!(doc["role"], "owner");
        assert_eq!(doc["owner_machine_id"], "hm_local");
        let text = assignment.to_text();
        assert!(text.contains("ws_roles"), "{text}");
        assert!(text.contains("owner"), "{text}");
    });
}
