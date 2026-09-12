use tempfile::tempdir;

use chrono::Utc;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_registry::workspace_registry;
use orbit_types::workflow::{OverlapPolicy, RoutineTarget};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};

use crate::tests::env_isolation::EnvGuard;

use super::super::init::{
    ONBOARDING_FINALIZE_GUIDANCE, WorkspaceInitArgs, canonical_workspace_id, checked_out_branch,
    onboarding_finalize_guidance, render_task_id_start,
};
use super::super::list::{format_workspace_list, workspace_list_json};
use super::super::role::CliCheckoutRole;
use super::super::show::format_workspace_show;
use super::super::support::orbit_gitignore_block;

#[test]
fn task_id_start_uses_the_host_task_prefix() {
    assert_eq!(render_task_id_start(Some("DANI"), 20_000), "DANI-20000");
}

#[test]
fn workspace_init_uses_the_checked_out_branch_when_base_branch_is_omitted() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_branch\"\nhost_id = \"branch-host\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");

    let git_init = std::process::Command::new("git")
        .args(["init", "--quiet", "--initial-branch", "master"])
        .arg(workspace.path())
        .status()
        .expect("run git init");
    assert!(git_init.success(), "initialize master branch repository");
    let git_commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=branch@example.test",
            "-c",
            "user.name=Branch Test",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "initial commit",
        ])
        .current_dir(workspace.path())
        .status()
        .expect("create initial commit");
    assert!(git_commit.success(), "commit master branch fixture");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    WorkspaceInitArgs {
        name: Some("checked-out-branch".to_string()),
        base_branch: None,
        ship_mode: Some("local".to_string()),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
    .execute_without_runtime(None)
    .expect("workspace init");

    let registry = workspace_registry::load_registry_from(&global.join("workspaces.json"))
        .expect("load workspace registry");
    let registered = registry.workspaces.first().expect("registered workspace");
    assert_eq!(registered.base_branch, "master");
    assert_eq!(registered.ship_mode.as_deref(), Some("local"));
    assert!(
        registered.git_remote.is_none(),
        "fixture must have no remote"
    );
}

#[test]
fn checked_out_branch_keeps_main_as_the_default_for_main_checkouts() {
    let workspace = tempdir().expect("workspace tempdir");
    let git_init = std::process::Command::new("git")
        .args(["init", "--quiet", "--initial-branch", "main"])
        .arg(workspace.path())
        .status()
        .expect("run git init");
    assert!(git_init.success(), "initialize main branch repository");

    assert_eq!(checked_out_branch(workspace.path()), "main");
}

#[test]
fn workspace_reinit_requires_force_and_force_reconciles_matching_registration() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    // A host identity must exist for workspace init to seed default routines
    // (which creates `.orbit/routines/`); `orbit init` owns its creation.
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_reinit\"\nhost_id = \"reinit-host\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let init = |base_branch: Option<&str>, ship_mode: Option<&str>, force| WorkspaceInitArgs {
        name: Some("reinit-merge".to_string()),
        base_branch: base_branch.map(str::to_string),
        ship_mode: ship_mode.map(str::to_string),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };

    init(Some("agent-main"), None, false)
        .execute_without_runtime(None)
        .expect("initial workspace init");

    let registry_path = global.join("workspaces.json");
    let original = workspace_registry::load_registry_from(&registry_path)
        .expect("load initial registry")
        .workspaces
        .into_iter()
        .next()
        .expect("registered workspace");
    assert_eq!(original.ship_mode, None);
    assert_eq!(
        orbit_core::resolved_ship_mode(&original).as_input_value(),
        "pr",
        "an omitted ship mode must preserve the PR delivery default"
    );

    init(None, Some("local"), true)
        .execute_without_runtime(None)
        .expect("re-init with explicit local mode");
    let after_local_registry = workspace_registry::load_registry_from(&registry_path)
        .expect("load registry after local mode update");
    let after_local_mode = after_local_registry
        .workspaces
        .iter()
        .find(|workspace| workspace.id == original.id)
        .expect("registered workspace");
    assert_eq!(after_local_mode.ship_mode.as_deref(), Some("local"));
    assert_eq!(
        orbit_core::resolved_ship_mode(after_local_mode).as_input_value(),
        "local",
        "an explicit local ship mode must retain in-place delivery"
    );
    let authored_routine = r#"schemaVersion: 1
name: custom-ship-sweep
description: operator-authored routine
enabled: true
hosts: [custom-host]
trigger:
  cron: "7 3 * * *"
  missed_run: catch_up_once
target: job:workspace_ship_pipeline
policy:
  timeout_minutes: 77
  overlap: allow
"#;
    let routine_path = workspace.path().join(".orbit/routines/ship_sweep.yaml");
    std::fs::write(&routine_path, authored_routine).expect("author routine");
    let auto_task_path = workspace
        .path()
        .join(".orbit/auto_tasks/friction-curation.yaml");
    let seeded_auto_task =
        std::fs::read_to_string(&auto_task_path).expect("read seeded friction-curation definition");
    assert!(
        seeded_auto_task.contains("enabled: false"),
        "workspace initialization must not enable a default auto-task"
    );
    let qa_auto_task_path = workspace.path().join(".orbit/auto_tasks/qa-sweep.yaml");
    let seeded_qa_auto_task =
        std::fs::read_to_string(&qa_auto_task_path).expect("read seeded qa-sweep definition");
    assert!(
        seeded_qa_auto_task.contains("enabled: false"),
        "workspace initialization must not enable the QA default auto-task"
    );
    let security_auto_task_path = workspace
        .path()
        .join(".orbit/auto_tasks/security-review.yaml");
    let seeded_security_auto_task = std::fs::read_to_string(&security_auto_task_path)
        .expect("read seeded security-review definition");
    assert!(
        seeded_security_auto_task.contains("enabled: false"),
        "workspace initialization must not enable the security-review default auto-task"
    );
    let authored_auto_task = "operator-authored auto-task definition\n";
    let authored_qa_auto_task = "operator-authored QA auto-task definition\n";
    let authored_security_auto_task = "operator-authored security-review auto-task definition\n";
    std::fs::write(&auto_task_path, authored_auto_task).expect("author auto-task definition");
    std::fs::write(&qa_auto_task_path, authored_qa_auto_task)
        .expect("author QA auto-task definition");
    std::fs::write(&security_auto_task_path, authored_security_auto_task)
        .expect("author security-review auto-task definition");

    let registry_bytes = std::fs::read_to_string(&registry_path).expect("read protected registry");
    let identity_path = workspace.path().join(".orbit/config.yaml");
    let identity_bytes = std::fs::read_to_string(&identity_path).expect("read protected identity");
    let error = init(None, Some("pr"), false)
        .execute_without_runtime(None)
        .expect_err("existing checkout must require force")
        .to_string();
    assert!(error.contains("already exists"), "unexpected: {error}");
    assert_eq!(
        std::fs::read_to_string(&registry_path).expect("read registry"),
        registry_bytes
    );
    assert_eq!(
        std::fs::read_to_string(&identity_path).expect("read identity"),
        identity_bytes
    );

    init(None, Some("pr"), true)
        .execute_without_runtime(None)
        .expect("re-init with explicit PR mode");
    let after_ship_mode = workspace_registry::load_registry_from(&registry_path)
        .expect("load registry after ship mode update")
        .workspaces
        .into_iter()
        .next()
        .expect("registered workspace");
    assert_eq!(after_ship_mode.id, original.id);
    assert_eq!(after_ship_mode.created_at, original.created_at);
    assert_eq!(after_ship_mode.base_branch, "agent-main");
    assert_eq!(after_ship_mode.ship_mode.as_deref(), Some("pr"));
    assert_eq!(
        orbit_core::resolved_ship_mode(&after_ship_mode).as_input_value(),
        "pr",
        "workspace_ship_pipeline must receive the persisted PR mode"
    );
    assert_eq!(
        std::fs::read_to_string(&routine_path).expect("read authored routine"),
        authored_routine
    );
    assert_eq!(
        std::fs::read_to_string(&auto_task_path).expect("read authored auto-task definition"),
        authored_auto_task,
        "workspace --force reconciliation must preserve an authored auto-task definition"
    );
    assert_eq!(
        std::fs::read_to_string(&qa_auto_task_path).expect("read authored QA auto-task definition"),
        authored_qa_auto_task,
        "workspace --force reconciliation must preserve an authored QA auto-task definition"
    );
    assert_eq!(
        std::fs::read_to_string(&security_auto_task_path)
            .expect("read authored security-review auto-task definition"),
        authored_security_auto_task,
        "workspace --force reconciliation must preserve an authored security-review auto-task definition"
    );

    init(None, None, true)
        .execute_without_runtime(None)
        .expect("re-init with omitted registration options");
    let after_omitted = workspace_registry::load_registry_from(&registry_path)
        .expect("load registry after omitted options")
        .workspaces
        .into_iter()
        .next()
        .expect("registered workspace");
    assert_eq!(after_omitted.id, original.id);
    assert_eq!(after_omitted.created_at, original.created_at);
    assert_eq!(after_omitted.base_branch, "agent-main");
    assert_eq!(after_omitted.ship_mode.as_deref(), Some("pr"));
    assert_eq!(
        std::fs::read_to_string(&routine_path).expect("read authored routine"),
        authored_routine
    );

    init(Some("release"), None, true)
        .execute_without_runtime(None)
        .expect("re-init with explicit base branch");
    let after_base_branch = workspace_registry::load_registry_from(&registry_path)
        .expect("load registry after base branch update")
        .workspaces
        .into_iter()
        .next()
        .expect("registered workspace");
    assert_eq!(after_base_branch.base_branch, "release");
    assert_eq!(after_base_branch.ship_mode.as_deref(), Some("pr"));

    let before_invalid = after_base_branch.clone();
    let error = init(None, Some("invalid"), true)
        .execute_without_runtime(None)
        .expect_err("invalid ship mode must fail closed");
    assert!(error.to_string().contains("unknown ship mode 'invalid'"));
    let after_invalid = workspace_registry::load_registry_from(&registry_path)
        .expect("load registry after invalid mode")
        .workspaces
        .into_iter()
        .next()
        .expect("registered workspace");
    assert_eq!(after_invalid, before_invalid);
}

#[test]
fn workspace_init_rejects_existing_checkout_path_with_different_id_without_force() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_path_collision\"\nhost_id = \"path-collision\"\nmode = \"standalone\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    let args = |name: &str| WorkspaceInitArgs {
        name: Some(name.to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    };
    args("path-owner")
        .execute_without_runtime(None)
        .expect("initial workspace init");
    let registry_path = global.join("workspaces.json");
    let identity_path = workspace.path().join(".orbit/config.yaml");
    let registry_bytes = std::fs::read_to_string(&registry_path).expect("read protected registry");
    let identity_bytes = std::fs::read_to_string(&identity_path).expect("read protected identity");

    let error = args("different-id")
        .execute_without_runtime(None)
        .expect_err("existing checkout path must require force")
        .to_string();
    assert!(error.contains("already exists"), "unexpected: {error}");
    assert_eq!(
        std::fs::read_to_string(&registry_path).expect("read registry"),
        registry_bytes
    );
    assert_eq!(
        std::fs::read_to_string(&identity_path).expect("read identity"),
        identity_bytes
    );
}

#[test]
fn workspace_init_rejects_existing_durable_id_without_force() {
    let first = tempdir().expect("first workspace tempdir");
    let second = tempdir().expect("second workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_id_collision\"\nhost_id = \"id-collision\"\nmode = \"standalone\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(first.path());
    let args = |force| WorkspaceInitArgs {
        name: Some("shared-id".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };
    args(false)
        .execute_without_runtime(None)
        .expect("initial workspace init");
    let registry_path = global.join("workspaces.json");
    let registry_bytes = std::fs::read_to_string(&registry_path).expect("read protected registry");

    std::env::set_current_dir(second.path()).expect("switch to second workspace");
    let error = args(false)
        .execute_without_runtime(None)
        .expect_err("existing durable ID must require force")
        .to_string();
    assert!(error.contains("already exists"), "unexpected: {error}");
    assert_eq!(
        std::fs::read_to_string(&registry_path).expect("read registry"),
        registry_bytes
    );
    assert!(!second.path().join(".orbit").exists());
}

#[test]
fn force_replaces_a_checkout_identity_that_no_registration_claims() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_bootstrap\"\nhost_id = \"bootstrap-host\"\nmode = \"standalone\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    // Any command that opens a runtime in an uninitialized checkout seeds a
    // legacy bootstrap identity here before `workspace init` ever runs.
    let orbit_dir = workspace.path().join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create checkout orbit dir");
    let identity_path = orbit_dir.join("config.yaml");
    let bootstrap_identity = "schema_version: 1\nworkspace_id: work-a1b2c3\n";
    std::fs::write(&identity_path, bootstrap_identity).expect("seed bootstrap identity");

    let args = |force| WorkspaceInitArgs {
        name: Some("bootstrap-claim".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };

    let registry_path = global.join("workspaces.json");
    let error = args(false)
        .execute_without_runtime(None)
        .expect_err("a conflicting checkout identity must require force")
        .to_string();
    assert!(error.contains("rerun with --force"), "unexpected: {error}");
    assert_eq!(
        std::fs::read_to_string(&identity_path).expect("read identity"),
        bootstrap_identity
    );
    assert!(!registry_path.exists(), "refusal must not seed a registry");

    args(true)
        .execute_without_runtime(None)
        .expect("force must reconcile an unclaimed checkout identity");
    let expected_id = canonical_workspace_id("bootstrap-claim");
    assert!(
        std::fs::read_to_string(&identity_path)
            .expect("read reconciled identity")
            .contains(&expected_id),
        "force must rewrite the checkout identity"
    );
    let registry = workspace_registry::load_registry_from(&registry_path).expect("load registry");
    assert_eq!(registry.workspaces.len(), 1);
    assert_eq!(registry.workspaces[0].id, expected_id);
    assert_eq!(registry.checkouts.len(), 1);
    assert_eq!(registry.checkouts[0].workspace_id, expected_id);
    assert_eq!(
        std::fs::canonicalize(&registry.checkouts[0].repo_root).expect("canonical checkout root"),
        std::fs::canonicalize(workspace.path()).expect("canonical workspace root")
    );
}

#[test]
fn force_refuses_to_replace_a_checkout_identity_a_registration_still_claims() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_claimed\"\nhost_id = \"claimed-host\"\nmode = \"standalone\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    // A registered workspace bound to some *other* checkout: this checkout's
    // stray identity claims it, so no registry lookup by path reconciles it.
    let now = Utc::now();
    let claimed = Workspace {
        id: "ws_claimed".to_string(),
        name: "claimed".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let other_root = home.path().join("elsewhere");
    let registry = WorkspaceRegistry {
        workspaces: vec![claimed.clone()],
        checkouts: vec![WorkspaceCheckout::owner(
            claimed.id.clone(),
            other_root.clone(),
            other_root.join(".orbit"),
        )],
        ..Default::default()
    };
    let registry_path = global.join("workspaces.json");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("seed registry");
    let registry_bytes = std::fs::read_to_string(&registry_path).expect("read protected registry");

    let orbit_dir = workspace.path().join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create checkout orbit dir");
    let identity_path = orbit_dir.join("config.yaml");
    let claimed_identity = "schema_version: 1\nworkspace_id: ws_claimed\n";
    std::fs::write(&identity_path, claimed_identity).expect("seed claimed identity");

    let error = WorkspaceInitArgs {
        name: Some("claim-jumper".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: true,
    }
    .execute_without_runtime(None)
    .expect_err("force must not detach a claimed checkout identity")
    .to_string();
    assert!(
        error.contains("claimed by an existing registration"),
        "unexpected: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&identity_path).expect("read identity"),
        claimed_identity
    );
    assert_eq!(
        std::fs::read_to_string(&registry_path).expect("read registry"),
        registry_bytes
    );
}

#[test]
fn forced_workspace_reconciliation_preserves_registry_and_identity_on_validation_failure() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_force_failure\"\nhost_id = \"force-failure\"\nmode = \"standalone\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    let args = |force| WorkspaceInitArgs {
        name: Some("force-failure".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: Some("pr".to_string()),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };
    args(false)
        .execute_without_runtime(None)
        .expect("initial workspace init");
    let registry_path = global.join("workspaces.json");
    let identity_path = workspace.path().join(".orbit/config.yaml");
    std::fs::write(
        &identity_path,
        "schema_version: 1\nworkspace_id: ws_other\n",
    )
    .expect("corrupt identity for validation test");
    let registry_bytes = std::fs::read_to_string(&registry_path).expect("read protected registry");
    let identity_bytes = std::fs::read_to_string(&identity_path).expect("read protected identity");

    let error = args(true)
        .execute_without_runtime(None)
        .expect_err("mismatched identity must reject forced reconciliation")
        .to_string();
    assert!(error.contains("checkout identity"), "unexpected: {error}");
    assert_eq!(
        std::fs::read_to_string(&registry_path).expect("read registry"),
        registry_bytes
    );
    assert_eq!(
        std::fs::read_to_string(&identity_path).expect("read identity"),
        identity_bytes
    );
}

#[test]
fn force_recovers_empty_or_missing_identity_only_for_the_exact_registration() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_identity_recovery\"\nhost_id = \"identity-recovery\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");
    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    let args = |force| WorkspaceInitArgs {
        name: Some("identity-recovery".to_string()),
        base_branch: None,
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };

    args(false)
        .execute_without_runtime(None)
        .expect("initial workspace init");
    let registry_path = global.join("workspaces.json");
    let registry_without_refresh_time = || {
        let mut registry: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&registry_path).expect("read registry for comparison"),
        )
        .expect("parse registry for comparison");
        for workspace in registry["workspaces"]
            .as_array_mut()
            .expect("workspace registry array")
        {
            workspace
                .as_object_mut()
                .expect("workspace registry object")
                .remove("updated_at");
        }
        registry
    };
    let registry_before = registry_without_refresh_time();
    let identity_path = workspace.path().join(".orbit/config.yaml");
    let expected_id = canonical_workspace_id("identity-recovery");

    std::fs::write(&identity_path, []).expect("truncate identity to zero bytes");
    let error = args(false)
        .execute_without_runtime(None)
        .expect_err("recovery must require force")
        .to_string();
    assert!(error.contains("rerun with --force"), "unexpected: {error}");
    assert_eq!(
        std::fs::read(&identity_path).expect("read refused empty identity"),
        Vec::<u8>::new()
    );

    args(true)
        .execute_without_runtime(None)
        .expect("force must recover an empty identity for the exact registration");
    let recovered = std::fs::read_to_string(&identity_path).expect("read recovered identity");
    assert!(recovered.contains(&format!("workspace_id: {expected_id}")));
    let evidence_dir = workspace
        .path()
        .join(".orbit/state/recovery/workspace-identity");
    let evidence = std::fs::read_dir(&evidence_dir)
        .expect("read identity recovery evidence")
        .map(|entry| entry.expect("read evidence entry").path())
        .collect::<Vec<_>>();
    assert_eq!(evidence.len(), 1, "unexpected evidence: {evidence:?}");
    assert_eq!(
        std::fs::read(&evidence[0]).expect("read archived corrupt identity"),
        Vec::<u8>::new(),
        "the exact corrupt bytes must be preserved before recovery"
    );
    assert_eq!(
        registry_without_refresh_time(),
        registry_before,
        "identity recovery must preserve the global registration"
    );

    std::fs::remove_file(&identity_path).expect("remove identity for missing recovery");
    args(true)
        .execute_without_runtime(None)
        .expect("force must recover a missing identity for the exact registration");
    let recovered = std::fs::read_to_string(&identity_path).expect("read recovered identity");
    assert!(recovered.contains(&format!("workspace_id: {expected_id}")));
    assert_eq!(
        std::fs::read_dir(&evidence_dir)
            .expect("read evidence after missing recovery")
            .count(),
        1,
        "a missing identity has no corrupt bytes to archive"
    );
    assert_eq!(
        registry_without_refresh_time(),
        registry_before,
        "missing-identity recovery must preserve the global registration"
    );
}

#[test]
fn multi_host_workspace_init_persists_an_explicit_local_owner() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    WorkspaceInitArgs {
        name: Some("local-owner".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: Some("pr".to_string()),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
    .execute_without_runtime(None)
    .expect("explicit owner workspace init");

    let registry = workspace_registry::load_registry_from(&global.join("workspaces.json"))
        .expect("reload owner registry");
    assert_eq!(
        registry.workspaces[0].owner_machine_id.as_deref(),
        Some("hm_local")
    );
    assert_eq!(
        registry.owner_host_ids.get("hm_local").map(String::as_str),
        Some("local")
    );
    assert_eq!(
        registry.checkouts[0].role,
        Some(orbit_types::workspace::WorkspaceCheckoutRole::Owner)
    );
}

#[test]
fn workspace_init_can_atomically_declare_a_remote_owner_replica() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    WorkspaceInitArgs {
        name: Some("replica".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: Some("pr".to_string()),
        role: Some(CliCheckoutRole::Replica),
        owner: Some("hm_owner".to_string()),
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
    .execute_without_runtime(None)
    .expect("atomic replica workspace init");

    let registry = workspace_registry::load_registry_from(&global.join("workspaces.json"))
        .expect("reload replica registry");
    assert_eq!(
        registry.workspaces[0].owner_machine_id.as_deref(),
        Some("hm_owner")
    );
    assert_eq!(
        registry.checkouts[0].role,
        Some(CliCheckoutRole::Replica.into())
    );
    assert_eq!(
        registry.checkouts[0].owner_machine_id.as_deref(),
        Some("hm_owner")
    );
    assert_eq!(
        registry.owner_host_ids.get("hm_owner").map(String::as_str),
        Some("hm_owner")
    );
}

#[test]
fn invalid_replica_init_fails_before_workspace_artifacts_or_registry_mutation() {
    for (rejected_owner, expected) in [("hm_local", "local machine"), ("ssh:hub", "machine_id")] {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let global = home.path().join(".orbit");
        std::fs::create_dir_all(&global).expect("create global orbit");
        std::fs::write(
            global.join("host.toml"),
            "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n",
        )
        .expect("write host identity");

        let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
        let error = WorkspaceInitArgs {
            name: Some("invalid-replica".to_string()),
            base_branch: Some("agent-main".to_string()),
            ship_mode: Some("pr".to_string()),
            role: Some(CliCheckoutRole::Replica),
            owner: Some(rejected_owner.to_string()),
            task_id_start: None,
            mcp: false,
            inject_agent_rules: false,
            refresh_defaults: false,
            force: false,
        }
        .execute_without_runtime(None)
        .expect_err("invalid replica declaration must fail before bootstrap")
        .to_string();
        assert!(error.contains(expected), "unexpected: {error}");
        assert!(!workspace.path().join(".orbit").exists());
        assert!(!workspace.path().join(".gitignore").exists());
        assert!(!global.join("workspaces.json").exists());
    }
}

#[test]
fn workspace_list_and_show_report_effective_ship_mode() {
    let now = Utc::now();
    let workspace = Workspace {
        id: "ws_constellation".to_string(),
        name: "pr-gated".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let registry = WorkspaceRegistry {
        workspaces: vec![workspace.clone()],
        checkouts: vec![WorkspaceCheckout::owner(
            workspace.id.clone(),
            "/work/pr-gated".into(),
            "/work/pr-gated/.orbit".into(),
        )],
        ..Default::default()
    };

    let list = format_workspace_list(&registry, false);
    assert!(list.contains("SHIP MODE"), "{list}");
    assert!(list.contains("pr"), "{list}");
    let mut lines = list.lines();
    let header = lines.next().expect("workspace list header");
    let row = lines.next().expect("workspace list row");
    let status_column = header.find("STATUS").expect("status column");
    let ship_mode_column = header.find("SHIP MODE").expect("ship mode column");
    assert!(row[status_column..].starts_with("active"), "{list}");
    assert!(row[ship_mode_column..].starts_with("pr"), "{list}");

    let show = format_workspace_show(&workspace, &registry.checkouts[0]);
    assert!(show.contains("ship_mode:   pr"), "{show}");
}

#[test]
fn workspace_list_hides_replicas_unless_all_and_marks_their_owner() {
    let now = Utc::now();
    let owner = Workspace {
        id: "ws_owner".to_string(),
        name: "owner".to_string(),
        owner_machine_id: Some("hm_local".to_string()),
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let replica = Workspace {
        id: "ws_replica".to_string(),
        name: "replica".to_string(),
        owner_machine_id: Some("hm_owner".to_string()),
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let registry = WorkspaceRegistry {
        workspaces: vec![owner.clone(), replica.clone()],
        checkouts: vec![
            WorkspaceCheckout::owner(
                owner.id.clone(),
                "/work/owner".into(),
                "/work/owner/.orbit".into(),
            ),
            WorkspaceCheckout {
                workspace_id: replica.id.clone(),
                repo_root: "/work/replica".into(),
                orbit_dir: "/work/replica/.orbit".into(),
                role: Some(WorkspaceCheckoutRole::Replica),
                owner_machine_id: Some("hm_owner".to_string()),
                path_overrides: Vec::new(),
            },
        ],
        ..Default::default()
    };

    assert_eq!(
        workspace_list_json(&registry, false)
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!format_workspace_list(&registry, false).contains("replica"));

    let all = workspace_list_json(&registry, true);
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(all[1]["owner_machine_id"], "hm_owner");
    let text = format_workspace_list(&registry, true);
    assert!(text.contains("replica"));
    assert!(text.contains("hm_owner"));
}

#[test]
fn workspace_init_seeds_disabled_routines_and_reinit_preserves_authored_files() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_inithost\"\nhost_id = \"init-host\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let init = |force| WorkspaceInitArgs {
        name: Some("routine-seed-test".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: Some("pr".to_string()),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };
    init(false)
        .execute_without_runtime(None)
        .expect("first workspace init");

    let routines_dir = workspace.path().join(".orbit/routines");
    for (stem, target) in [
        ("auto_task_scheduler", "auto_task_scheduler_pipeline"),
        ("task_triage", "task_triage_pipeline"),
        ("ship_sweep", "workspace_ship_pipeline"),
    ] {
        let yaml = std::fs::read_to_string(routines_dir.join(format!("{stem}.yaml")))
            .expect("read seeded routine");
        let definition = parse_routine_yaml(&yaml).expect("parse seeded routine");
        assert_eq!(
            definition.name,
            format!("{}-routine-seed-test", stem.replace('_', "-"))
        );
        assert_eq!(definition.target, RoutineTarget::Job(target.to_string()));
        assert_eq!(definition.policy.overlap, OverlapPolicy::Forbid);
        assert!(!definition.enabled);
    }

    let authored_ship = r#"schemaVersion: 1
name: custom-ship-sweep
description: authored values must survive re-init
enabled: true
trigger:
  cron: "7 3 * * *"
  missed_run: catch_up_once
target: job:workspace_ship_pipeline
policy:
  timeout_minutes: 77
  overlap: allow
"#;
    std::fs::write(routines_dir.join("ship_sweep.yaml"), authored_ship)
        .expect("author ship routine");
    std::fs::remove_file(routines_dir.join("task_triage.yaml"))
        .expect("remove one default routine");

    init(true)
        .execute_without_runtime(None)
        .expect("second workspace init");

    assert_eq!(
        std::fs::read_to_string(routines_dir.join("ship_sweep.yaml"))
            .expect("read authored ship routine"),
        authored_ship,
        "plain re-init must preserve workspace-authored routine bytes"
    );
    let recreated = std::fs::read_to_string(routines_dir.join("task_triage.yaml"))
        .expect("missing default recreated");
    assert!(
        !parse_routine_yaml(&recreated)
            .expect("parse recreated routine")
            .enabled
    );
}

/// Routine names are host-wide identifiers, so their per-workspace suffix must
/// come from the registered workspace name. Seeding from the checkout directory
/// left `orbit routine show <routine>-<workspace-name>` with nothing to find and
/// made two `repo`/`src`/`app` checkouts collide on one host [ORB-12107].
#[test]
fn seeded_routine_names_follow_the_workspace_name_not_the_checkout_directory() {
    let base = tempdir().expect("base tempdir");
    let home = tempdir().expect("home tempdir");
    seed_host_identity(home.path());
    let checkout = base.path().join("repo");
    std::fs::create_dir_all(&checkout).expect("create checkout directory");

    let _env = EnvGuard::acquire().home(home.path()).cwd(&checkout);
    routine_seed_init("qa-sweep")
        .execute_without_runtime(None)
        .expect("workspace init");

    for name in seeded_routine_names(&checkout) {
        assert!(
            name.ends_with("-qa-sweep"),
            "routine '{name}' must be suffixed with the registered workspace name"
        );
        assert!(
            !name.contains("repo"),
            "routine '{name}' must not carry the checkout directory name"
        );
    }
}

/// Two checkouts whose directories share a basename are the collision the
/// directory-derived suffix could not survive; distinct workspace names must
/// keep their seeded routines distinct [ORB-12107].
#[test]
fn same_basename_checkouts_with_distinct_names_seed_distinct_routine_names() {
    let base = tempdir().expect("base tempdir");
    let home = tempdir().expect("home tempdir");
    seed_host_identity(home.path());
    let alpha = base.path().join("a/server");
    let beta = base.path().join("b/server");
    std::fs::create_dir_all(&alpha).expect("create first checkout");
    std::fs::create_dir_all(&beta).expect("create second checkout");

    let env = EnvGuard::acquire().home(home.path()).cwd(&alpha);
    routine_seed_init("alpha")
        .execute_without_runtime(None)
        .expect("first workspace init");
    let _env = env.cwd(&beta);
    routine_seed_init("beta")
        .execute_without_runtime(None)
        .expect("second workspace init with the same directory basename");

    let alpha_names = seeded_routine_names(&alpha);
    let beta_names = seeded_routine_names(&beta);
    assert!(alpha_names.iter().all(|name| name.ends_with("-alpha")));
    assert!(beta_names.iter().all(|name| name.ends_with("-beta")));
    assert!(
        alpha_names.iter().all(|name| !beta_names.contains(name)),
        "same-basename checkouts must not seed colliding routine names: {alpha_names:?} / {beta_names:?}"
    );
}

/// Seeded definitions carry no host id [ORB-12236], so the same workspace name
/// initialized on two machines produces the same bytes — a repository can be
/// registered on a second host with no definition edits.
#[test]
fn seeded_routines_are_byte_identical_across_host_identities() {
    let base = tempdir().expect("base tempdir");
    let first_home = tempdir().expect("first home tempdir");
    let second_home = tempdir().expect("second home tempdir");
    seed_named_host_identity(first_home.path(), "hm_first", "first-host");
    seed_named_host_identity(second_home.path(), "hm_second", "second-host");
    let first = base.path().join("first/server");
    let second = base.path().join("second/server");
    std::fs::create_dir_all(&first).expect("create first checkout");
    std::fs::create_dir_all(&second).expect("create second checkout");

    let env = EnvGuard::acquire().home(first_home.path()).cwd(&first);
    routine_seed_init("shared")
        .execute_without_runtime(None)
        .expect("first host workspace init");
    let _env = env.home(second_home.path()).cwd(&second);
    routine_seed_init("shared")
        .execute_without_runtime(None)
        .expect("second host workspace init");

    let read_routines = |checkout: &std::path::Path| -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = std::fs::read_dir(checkout.join(".orbit/routines"))
            .expect("read seeded routines directory")
            .map(|entry| entry.expect("routines directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
            .map(|path| {
                (
                    path.file_name()
                        .expect("routine file name")
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read_to_string(&path).expect("read seeded routine"),
                )
            })
            .collect();
        files.sort();
        files
    };

    let first_routines = read_routines(&first);
    assert!(!first_routines.is_empty(), "init must seed routines");
    assert_eq!(first_routines, read_routines(&second));
}

/// Routine discovery drops *every* definition sharing a name, so a duplicate
/// would silently disable both workspaces' routines. Init reports it instead,
/// and leaves the second checkout uninitialized [ORB-12107].
#[test]
fn workspace_init_refuses_a_name_whose_seeded_routines_already_exist() {
    let base = tempdir().expect("base tempdir");
    let home = tempdir().expect("home tempdir");
    seed_host_identity(home.path());
    let alpha = base.path().join("a/server");
    let beta = base.path().join("b/server");
    std::fs::create_dir_all(&alpha).expect("create first checkout");
    std::fs::create_dir_all(&beta).expect("create second checkout");

    let env = EnvGuard::acquire().home(home.path()).cwd(&alpha);
    routine_seed_init("alpha")
        .execute_without_runtime(None)
        .expect("first workspace init");

    // The registered workspace already claims a name the next one would seed —
    // the shape a directory-derived seed left behind on a host with two
    // `server` checkouts.
    let claimed = std::fs::read_to_string(alpha.join(".orbit/routines/task_pilot.yaml"))
        .expect("read seeded routine")
        .replace("task-pilot-alpha", "task-pilot-beta");
    std::fs::write(alpha.join(".orbit/routines/claimed.yaml"), &claimed)
        .expect("author the claiming routine");

    let _env = env.cwd(&beta);
    let error = routine_seed_init("beta")
        .execute_without_runtime(None)
        .expect_err("a colliding routine name must fail workspace init");
    let message = error.to_string();
    assert!(message.contains("task-pilot-beta"), "{message}");
    assert!(message.contains("--name"), "{message}");

    assert!(
        !beta.join(".orbit/routines").exists(),
        "a refused init must not seed routines into the second checkout"
    );
    let registry =
        workspace_registry::load_registry_from(&home.path().join(".orbit").join("workspaces.json"))
            .expect("load registry");
    assert!(
        !registry
            .workspaces
            .iter()
            .any(|workspace| workspace.id == canonical_workspace_id("beta")),
        "a refused init must not register the workspace"
    );
}

fn seed_host_identity(home: &std::path::Path) {
    seed_named_host_identity(home, "hm_inithost", "init-host");
}

fn seed_named_host_identity(home: &std::path::Path, machine_id: &str, host_id: &str) {
    let global = home.join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        format!(
            "schema_version = 2\nmachine_id = \"{machine_id}\"\nhost_id = \"{host_id}\"\ntask_prefix = \"ORB\"\n"
        ),
    )
    .expect("write host identity");
}

fn routine_seed_init(name: &str) -> WorkspaceInitArgs {
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

/// Names of every managed routine seeded into `checkout`, read back from the
/// definitions themselves rather than from the seeding inputs.
fn seeded_routine_names(checkout: &std::path::Path) -> Vec<String> {
    let routines_dir = checkout.join(".orbit/routines");
    let mut names: Vec<String> = std::fs::read_dir(&routines_dir)
        .expect("read seeded routines directory")
        .map(|entry| entry.expect("routines directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .map(|path| {
            let yaml = std::fs::read_to_string(&path).expect("read seeded routine");
            parse_routine_yaml(&yaml)
                .expect("seeded routine parses")
                .name
        })
        .collect();
    assert!(!names.is_empty(), "init must seed routines");
    names.sort();
    names
}

#[test]
fn workspace_init_seeds_auto_detected_mcp_configs() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");

    std::fs::create_dir_all(workspace.path().join(".claude")).expect("create .claude");
    std::fs::create_dir_all(workspace.path().join(".gemini")).expect("create .gemini");
    std::fs::create_dir_all(workspace.path().join(".grok")).expect("create .grok");
    std::fs::create_dir_all(home.path().join(".codex")).expect("create global .codex");
    std::fs::write(
        home.path().join(".codex").join("config.toml"),
        "model = \"gpt-5.4\"\n",
    )
    .expect("write global codex config");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: None,
        base_branch: Some("main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: true,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
    .execute_without_runtime(None);

    result.expect("workspace init");
    assert!(
        workspace
            .path()
            .join(".claude")
            .join("settings.json")
            .exists()
    );
    assert!(workspace.path().join(".codex").join("config.toml").exists());
    assert!(
        workspace
            .path()
            .join(".gemini")
            .join("settings.json")
            .exists()
    );
    assert!(workspace.path().join(".grok").join("config.toml").exists());

    // `--mcp` from `orbit workspace init` is the operator-facing orchestrator
    // bootstrap path (ORB-10960): every auto-detected client must launch the
    // Orbit server with `mcp serve --operator`, exactly.
    let claude_mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.path().join(".mcp.json")).expect("read claude mcp"),
    )
    .expect("parse claude mcp");
    let workspace_id = registered_workspace_id(home.path());
    assert_operator_argv(&claude_mcp["mcpServers"]["orbit"]["args"], &workspace_id);

    let codex_config = std::fs::read_to_string(workspace.path().join(".codex/config.toml"))
        .expect("read codex config");
    let codex_parsed: toml::Value = toml::from_str(&codex_config).expect("parse codex config");
    assert_operator_argv_toml(&codex_parsed["mcp_servers"]["orbit"]["args"], &workspace_id);

    let gemini_settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.path().join(".gemini/settings.json"))
            .expect("read gemini settings"),
    )
    .expect("parse gemini settings");
    assert_operator_argv(
        &gemini_settings["mcpServers"]["orbit"]["args"],
        &workspace_id,
    );

    let grok_config = std::fs::read_to_string(workspace.path().join(".grok/config.toml"))
        .expect("read grok config");
    let grok_parsed: toml::Value = toml::from_str(&grok_config).expect("parse grok config");
    assert_operator_argv_toml(&grok_parsed["mcp_servers"]["orbit"]["args"], &workspace_id);
}

/// Every generated integration's argv must be exactly `mcp serve --operator
/// --workspace <ws_id>`: the authority this bootstrap path grants, and the
/// workspace it was generated for, so a client that cannot announce
/// `_meta.orbit.workspace` at initialize still routes workspace-scoped tools.
fn assert_operator_argv(args: &serde_json::Value, workspace_id: &str) {
    let args = args
        .as_array()
        .expect("args array")
        .iter()
        .map(|arg| arg.as_str().expect("string arg"))
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        ["mcp", "serve", "--operator", "--workspace", workspace_id]
    );
}

fn assert_operator_argv_toml(args: &toml::Value, workspace_id: &str) {
    let args = args
        .as_array()
        .expect("args array")
        .iter()
        .map(|arg| arg.as_str().expect("string arg"))
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        ["mcp", "serve", "--operator", "--workspace", workspace_id]
    );
}

/// The logical ID `orbit workspace init` just registered, read back from the
/// machine registry so the expectation is the real binding rather than a
/// re-derivation of the temp directory's name.
fn registered_workspace_id(home: &std::path::Path) -> String {
    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".orbit").join("workspaces.json"))
            .expect("read workspace registry"),
    )
    .expect("parse workspace registry");
    registry["workspaces"]
        .as_array()
        .and_then(|workspaces| workspaces.first())
        .and_then(|workspace| workspace["id"].as_str())
        .expect("one registered workspace")
        .to_string()
}

#[test]
fn workspace_reinit_with_force_mcp_refreshes_operator_argv_without_duplicating_it() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(workspace.path().join(".claude")).expect("create .claude");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let init = |force| {
        WorkspaceInitArgs {
            name: None,
            base_branch: Some("main".to_string()),
            ship_mode: None,
            role: None,
            owner: None,
            task_id_start: None,
            mcp: true,
            inject_agent_rules: false,
            refresh_defaults: false,
            force,
        }
        .execute_without_runtime(None)
        .expect("workspace init with --mcp")
    };

    init(false);
    let claude_path = workspace.path().join(".mcp.json");
    let first: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_path).expect("read claude mcp"))
            .expect("parse claude mcp");
    let workspace_id = registered_workspace_id(home.path());
    assert_operator_argv(&first["mcpServers"]["orbit"]["args"], &workspace_id);

    // Re-running the supported reconciliation path (`--force --mcp`) must
    // refresh the managed Orbit entry to a single `--operator` argument and a
    // single `--workspace` binding, not append a second one, while leaving
    // unrelated config untouched.
    init(true);
    let second: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_path).expect("read claude mcp"))
            .expect("parse claude mcp");
    assert_operator_argv(&second["mcpServers"]["orbit"]["args"], &workspace_id);
}

#[test]
fn workspace_init_skips_mcp_by_default() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");

    std::fs::create_dir_all(workspace.path().join(".claude")).expect("create .claude");
    std::fs::create_dir_all(workspace.path().join(".gemini")).expect("create .gemini");
    std::fs::create_dir_all(workspace.path().join(".grok")).expect("create .grok");
    std::fs::create_dir_all(home.path().join(".codex")).expect("create global .codex");
    std::fs::write(
        home.path().join(".codex").join("config.toml"),
        "model = \"gpt-5.4\"\n",
    )
    .expect("write global codex config");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: None,
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
    .execute_without_runtime(None);

    result.expect("workspace init");
    assert!(
        !workspace
            .path()
            .join(".claude")
            .join("settings.json")
            .exists()
    );
    assert!(!workspace.path().join(".codex").join("config.toml").exists());
    assert!(
        !workspace
            .path()
            .join(".gemini")
            .join("settings.json")
            .exists()
    );
    assert!(!workspace.path().join(".grok").join("config.toml").exists());
}

#[test]
fn workspace_init_under_home_with_global_orbit_creates_repo_orbit() {
    let home = tempdir().expect("home tempdir");
    let workspace = home.path().join("work").join("repo");
    let managed_registry = tempdir().expect("managed registry tempdir");
    std::fs::create_dir_all(workspace.join(".git")).expect("create workspace repo");
    std::fs::create_dir_all(home.path().join(".orbit")).expect("create global orbit root");
    let managed_registry_path = managed_registry.path().join("workspaces.json");
    std::fs::write(&managed_registry_path, "managed registry sentinel\n")
        .expect("seed managed registry");

    {
        let mut env = EnvGuard::acquire();
        let previous_registry_root = std::env::var_os("ORBIT_REGISTRY_ROOT");
        let previous_managed_context = std::env::var_os("ORBIT_MANAGED_RUN_CONTEXT");
        let previous_run_id = std::env::var_os("ORBIT_RUN_ID");

        env = env.managed_registry_root(managed_registry.path());

        assert_eq!(
            orbit_core::runtime::resolve_global_root().expect("resolve managed registry root"),
            managed_registry.path(),
            "a trusted managed child must retain registry-root precedence"
        );

        env = env.home(home.path()).cwd(&workspace);

        assert_eq!(
            orbit_core::runtime::resolve_global_root().expect("resolve fixture registry root"),
            home.path().join(".orbit"),
            "the fixture must clear the managed registry locator in favor of its temporary home"
        );

        let result = WorkspaceInitArgs {
            name: None,
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
        .execute_without_runtime(None);

        result.expect("workspace init");
        assert!(workspace.join(".orbit").join("state").is_dir());
        assert!(workspace.join(".orbit").join("knowledge").is_dir());
        assert!(!workspace.join(".orbit").join("adrs").exists());
        assert!(!home.path().join(".orbit").join("state").exists());
        assert!(!home.path().join(".orbit").join("knowledge").exists());
        assert!(home.path().join(".orbit/workspaces.json").is_file());
        assert_eq!(
            std::fs::read_to_string(&managed_registry_path).expect("read managed registry"),
            "managed registry sentinel\n",
            "workspace init must not write through the ambient managed registry locator"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join(".gitignore")).expect("read .gitignore"),
            orbit_gitignore_block()
        );
        assert!(!orbit_gitignore_block().contains(".orbit/adrs"));

        env.restore_now();
        assert_eq!(
            std::env::var_os("ORBIT_REGISTRY_ROOT"),
            previous_registry_root
        );
        assert_eq!(
            std::env::var_os("ORBIT_MANAGED_RUN_CONTEXT"),
            previous_managed_context
        );
        assert_eq!(std::env::var_os("ORBIT_RUN_ID"), previous_run_id);
    }
}

#[test]
fn workspace_init_appends_orbit_to_existing_gitignore() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(workspace.path().join(".git")).expect("create .git");
    std::fs::write(workspace.path().join(".gitignore"), "target/\n.DS_Store")
        .expect("write .gitignore");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: None,
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
    .execute_without_runtime(None);

    result.expect("workspace init");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".gitignore")).expect("read .gitignore"),
        format!("target/\n.DS_Store\n{}", orbit_gitignore_block())
    );
}

#[test]
fn workspace_init_replaces_legacy_bare_orbit_gitignore_line_with_managed_block() {
    // A bare `.orbit` line (written by earlier init versions) ignores the whole
    // directory, so artifact re-includes can never apply. Init must
    // replace the legacy line with the managed block, not merely append.
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(workspace.path().join(".git")).expect("create .git");
    std::fs::write(workspace.path().join(".gitignore"), "target/\n/.orbit/\n")
        .expect("write .gitignore");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let init = |force| WorkspaceInitArgs {
        name: None,
        base_branch: Some("main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };

    init(false)
        .execute_without_runtime(None)
        .expect("workspace init");
    let expected = format!("target/\n{}", orbit_gitignore_block());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".gitignore")).expect("read .gitignore"),
        expected,
        "legacy bare `.orbit` must be replaced by the managed block"
    );

    // Re-init is idempotent: the block is not duplicated or reordered.
    init(true)
        .execute_without_runtime(None)
        .expect("workspace re-init");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".gitignore")).expect("read .gitignore"),
        expected,
        "re-init must be idempotent once the managed block is present"
    );
}

#[test]
fn workspace_init_retires_adr_store_gitignore_lines() {
    // Workspaces initialized before ORB-10726 may carry ADR partition rules.
    // Re-init must remove every retired line.
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(workspace.path().join(".git")).expect("create .git");
    let older_managed_block = concat!(
        "target/\n",
        ".orbit/*\n",
        "!.orbit/adrs/\n",
        ".orbit/adrs/index.sqlite*\n",
        ".orbit/adrs/proposed/\n",
        ".orbit/adrs/superseded/\n",
        "!.orbit/auto_tasks/\n",
        "!.orbit/resources/\n",
        "!.orbit/routines/\n",
        "!.orbit/config.toml\n",
        ".orbit/**/*.lock\n",
    );
    std::fs::write(workspace.path().join(".gitignore"), older_managed_block)
        .expect("write .gitignore");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let init = |force| WorkspaceInitArgs {
        name: None,
        base_branch: Some("main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force,
    };

    init(false)
        .execute_without_runtime(None)
        .expect("workspace init");
    let expected = format!("target/\n{}", orbit_gitignore_block());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".gitignore")).expect("read .gitignore"),
        expected,
        "retired partition lines must be stripped, not stacked under a second block"
    );

    // Re-init converges: no duplicated block, no resurrected retired line.
    init(true)
        .execute_without_runtime(None)
        .expect("workspace re-init");
    let converged =
        std::fs::read_to_string(workspace.path().join(".gitignore")).expect("read .gitignore");
    assert_eq!(converged, expected, "re-init must converge on one block");
    for retired in [
        "!.orbit/adrs/",
        ".orbit/adrs/index.sqlite*",
        ".orbit/adrs/proposed/",
        ".orbit/adrs/superseded/",
    ] {
        assert!(
            !converged.lines().any(|line| line.trim() == retired),
            "retired ADR store line `{retired}` must be absent"
        );
    }
    assert_eq!(
        converged.matches(".orbit/*\n").count(),
        1,
        "the managed block must appear exactly once"
    );
}

#[test]
fn workspace_init_from_git_subdir_gitignores_repo_orbit_dir() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let nested = repo.path().join("packages").join("demo");
    std::fs::create_dir_all(repo.path().join(".git")).expect("create .git");
    std::fs::create_dir_all(&nested).expect("create nested workspace");

    let _env = EnvGuard::acquire().home(home.path()).cwd(&nested);

    let result = WorkspaceInitArgs {
        name: None,
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
    .execute_without_runtime(None);

    result.expect("workspace init");
    assert_eq!(
        std::fs::read_to_string(repo.path().join(".gitignore")).expect("read repo .gitignore"),
        orbit_gitignore_block()
    );
    assert!(!nested.join(".gitignore").exists());
}

#[test]
fn workspace_init_in_independent_nested_git_repo_preserves_parent_binding() {
    let parent = tempdir().expect("parent workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_nested_init\"\nhost_id = \"nested-init\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");
    let parent_git = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(parent.path())
        .status()
        .expect("run git init for parent");
    assert!(parent_git.success(), "initialize parent git repository");

    let _env = EnvGuard::acquire().home(home.path()).cwd(parent.path());
    let init = |name: &str| WorkspaceInitArgs {
        name: Some(name.to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: Some("local".to_string()),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    };
    init("registered-parent")
        .execute_without_runtime(None)
        .expect("initialize registered parent");

    let registry_path = global.join("workspaces.json");
    let parent_identity_path = parent.path().join(".orbit/config.yaml");
    let parent_identity_before =
        std::fs::read(&parent_identity_path).expect("read parent identity before child init");
    let parent_gitignore_before =
        std::fs::read(parent.path().join(".gitignore")).expect("read parent gitignore");
    let registry_before =
        workspace_registry::load_registry_from(&registry_path).expect("load parent registry");
    let parent_id = canonical_workspace_id("registered-parent");
    let parent_workspace_before = serde_json::to_vec(
        registry_before
            .workspaces
            .iter()
            .find(|workspace| workspace.id == parent_id)
            .expect("parent workspace registration"),
    )
    .expect("serialize parent workspace registration");
    let parent_checkout_before = serde_json::to_vec(
        workspace_registry::find_checkout(&registry_before, &parent_id)
            .expect("lookup parent checkout")
            .expect("parent checkout registration"),
    )
    .expect("serialize parent checkout registration");

    let child = parent.path().join("codebases/independent-child");
    let child_git = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(&child)
        .status()
        .expect("run git init for child");
    assert!(child_git.success(), "initialize independent child git repo");
    std::env::set_current_dir(&child).expect("switch to independent child repo");
    init("independent-child")
        .execute_without_runtime(None)
        .expect("initialize independent child workspace");

    let child_id = canonical_workspace_id("independent-child");
    let child_orbit = child.join(".orbit");
    let child_identity =
        std::fs::read_to_string(child_orbit.join("config.yaml")).expect("read child identity");
    assert!(
        child_identity.contains(&format!("workspace_id: {child_id}")),
        "child repository must own its workspace identity: {child_identity}"
    );
    std::fs::write(child_orbit.join("config.yaml"), [])
        .expect("truncate child identity for recovery");
    let mut child_recovery = init("independent-child");
    child_recovery.force = true;
    child_recovery
        .execute_without_runtime(None)
        .expect("recover exactly registered child identity");
    assert_eq!(
        std::fs::read_to_string(child_orbit.join("config.yaml"))
            .expect("read recovered child identity"),
        child_identity,
        "child recovery must restore its own identity"
    );
    for state_dir in ["resources", "state"] {
        assert!(
            child_orbit.join(state_dir).is_dir(),
            "child workspace must own its {state_dir} state"
        );
    }
    assert!(
        !child_orbit.join("tasks").exists(),
        "child workspace must not project a tasks directory"
    );
    assert!(
        !parent.path().join("codebases/.orbit").exists(),
        "bootstrap must not create an intermediate shadow store"
    );

    let registry_after =
        workspace_registry::load_registry_from(&registry_path).expect("load child registry");
    let child_workspace = registry_after
        .workspaces
        .iter()
        .find(|workspace| workspace.id == child_id)
        .expect("child workspace registration");
    let child_checkout = workspace_registry::find_checkout(&registry_after, &child_id)
        .expect("lookup child checkout")
        .expect("child checkout registration");
    assert_eq!(child_workspace.name, "independent-child");
    assert_eq!(
        std::fs::canonicalize(&child_checkout.repo_root).expect("canonical child checkout"),
        std::fs::canonicalize(&child).expect("canonical child repo")
    );
    assert_eq!(child_checkout.orbit_dir, child_orbit);

    let parent_workspace_after = serde_json::to_vec(
        registry_after
            .workspaces
            .iter()
            .find(|workspace| workspace.id == parent_id)
            .expect("preserved parent workspace registration"),
    )
    .expect("serialize preserved parent workspace registration");
    let parent_checkout_after = serde_json::to_vec(
        workspace_registry::find_checkout(&registry_after, &parent_id)
            .expect("lookup parent checkout")
            .expect("preserved parent checkout registration"),
    )
    .expect("serialize preserved parent checkout registration");
    assert_eq!(parent_workspace_after, parent_workspace_before);
    assert_eq!(parent_checkout_after, parent_checkout_before);
    assert_eq!(
        std::fs::read(&parent_identity_path).expect("read parent identity after child init"),
        parent_identity_before
    );
    assert_eq!(
        std::fs::read(parent.path().join(".gitignore"))
            .expect("read parent gitignore after child init"),
        parent_gitignore_before
    );
}

#[test]
fn workspace_init_with_root_override_uses_custom_registry() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let custom_root_parent = tempdir().expect("custom root parent");
    let custom_root = custom_root_parent.path().join("custom-orbit");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: Some("custom-root".to_string()),
        base_branch: None,
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
    .execute_without_runtime(Some(custom_root.as_path()));

    result.expect("workspace init with root override");

    let custom_registry_path = custom_root.join("workspaces.json");
    assert!(custom_registry_path.exists());
    assert!(!home.path().join(".orbit").join("workspaces.json").exists());

    let registry = workspace_registry::load_registry_from(&custom_registry_path)
        .expect("load custom registry");
    let workspace_record = registry
        .workspaces
        .iter()
        .find(|workspace| workspace.name == "custom-root")
        .expect("registered workspace");
    let checkout = workspace_registry::find_checkout(&registry, &workspace_record.id)
        .expect("lookup registered checkout")
        .expect("registered checkout");
    assert_eq!(
        std::fs::canonicalize(&checkout.repo_root).expect("canonical registered root"),
        std::fs::canonicalize(workspace.path()).expect("canonical workspace")
    );
    assert_eq!(
        std::fs::canonicalize(&checkout.orbit_dir).expect("canonical registered root"),
        std::fs::canonicalize(&custom_root).expect("canonical custom root")
    );
    assert_eq!(workspace_record.base_branch, "main");
    assert!(
        !workspace.path().join(".orbitignore").exists(),
        "workspace init must not create the retired graph ignore file"
    );
}

#[test]
fn workspace_init_does_not_create_orbitignore() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: None,
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
    .execute_without_runtime(None);

    result.expect("workspace init");
    assert!(
        !workspace.path().join(".orbitignore").exists(),
        "workspace init must not create the retired graph ignore file"
    );
}

#[test]
fn workspace_init_preserves_existing_orbitignore() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::write(
        workspace.path().join(".orbitignore"),
        "custom-output/\n!custom-output/keep.txt\n",
    )
    .expect("seed existing .orbitignore");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: None,
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
    .execute_without_runtime(None);

    result.expect("workspace init");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".orbitignore")).expect("read .orbitignore"),
        "custom-output/\n!custom-output/keep.txt\n"
    );
}

#[test]
fn workspace_init_with_root_override_does_not_modify_repo_gitignore() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let custom_root_parent = tempdir().expect("custom root parent");
    let custom_root = custom_root_parent.path().join("custom-orbit");

    // Seed the workspace as a git repo so the pre-fix code would have
    // appended `.orbit` to <workspace>/.gitignore.
    std::fs::create_dir_all(workspace.path().join(".git")).expect("seed git dir");

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());

    let result = WorkspaceInitArgs {
        name: Some("custom-root-git".to_string()),
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
    .execute_without_runtime(Some(custom_root.as_path()));

    result.expect("workspace init with root override in a git repo");

    let gitignore = workspace.path().join(".gitignore");
    assert!(
        !gitignore.exists(),
        "`--root` outside the workspace must not create <workspace>/.gitignore",
    );

    let guidance = onboarding_finalize_guidance(workspace.path(), &custom_root);
    assert!(
        !guidance.contains("review and commit"),
        "relocated-root guidance must not tell operators to commit checkout files: {guidance}"
    );
    assert!(
        !guidance.contains(".gitignore"),
        "relocated-root guidance must not name a checkout .gitignore: {guidance}"
    );
    assert!(
        !guidance.contains(".orbit/auto_tasks") && !guidance.contains(".orbit/routines"),
        "relocated-root guidance must not name checkout-local definitions: {guidance}"
    );
}

/// Regression (ORB-10293): a nameless workspace whose default name is derived
/// from a `.tmpXXXXXX` cwd must register only in the isolated fixture registry
/// and never touch a synthetic "outer" HOME registry standing in for the
/// operator's real `~/.orbit/workspaces.json`. This reproduces the exact shape
/// (`ws_.tmpXXXXXX`) that leaked into the operator's registry before the shared
/// env guard serialized these tests.
#[test]
fn nameless_tmp_workspace_registers_only_in_isolated_registry() {
    // Synthetic operator HOME with a sentinel registry that must never change.
    let outer_home = tempdir().expect("outer home tempdir");
    let outer_registry = outer_home.path().join(".orbit").join("workspaces.json");
    std::fs::create_dir_all(outer_registry.parent().expect("outer .orbit parent"))
        .expect("create outer .orbit");
    let sentinel = "{\"sentinel\":\"operator-registry\"}\n";
    std::fs::write(&outer_registry, sentinel).expect("seed sentinel registry");

    // Isolated fixture HOME plus a nameless workspace directory. `tempdir()`
    // yields `/tmp/.tmpXXXXXX`, so the default workspace name is `.tmpXXXXXX`.
    let fixture_home = tempdir().expect("fixture home tempdir");
    let workspace = tempdir().expect("nameless workspace tempdir");
    let workspace_name = workspace
        .path()
        .file_name()
        .expect("workspace dir name")
        .to_string_lossy()
        .into_owned();
    assert!(
        workspace_name.starts_with(".tmp"),
        "fixture must reproduce the nameless `.tmpXXXXXX` shape, got {workspace_name}"
    );

    {
        let _env = EnvGuard::acquire()
            .home(fixture_home.path())
            .cwd(workspace.path());
        WorkspaceInitArgs {
            name: None,
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
        .execute_without_runtime(None)
        .expect("nameless workspace init");
    }

    // The nameless workspace registered only in the isolated fixture registry.
    let fixture_registry = fixture_home.path().join(".orbit").join("workspaces.json");
    let registry =
        workspace_registry::load_registry_from(&fixture_registry).expect("load fixture registry");
    assert!(
        registry
            .workspaces
            .iter()
            .any(|w| w.id == canonical_workspace_id(&workspace_name)),
        "nameless workspace must register in the isolated fixture registry"
    );

    // The synthetic outer registry is byte-for-byte unchanged: workspace init
    // never touched the operator's real machine-global registry.
    assert_eq!(
        std::fs::read_to_string(&outer_registry).expect("read outer registry"),
        sentinel,
        "workspace init must never mutate the operator's real registry"
    );
}

#[test]
fn workspace_init_guidance_and_generated_onboarding_files_lifecycle() {
    let workspace = tempdir().expect("workspace tempdir");
    let home = tempdir().expect("home tempdir");
    let global = home.path().join(".orbit");
    std::fs::create_dir_all(&global).expect("create global orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_guidance\"\nhost_id = \"guidance-host\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write host identity");

    // Initialize git repo
    let git_init = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(workspace.path())
        .status()
        .expect("git init");
    assert!(git_init.success());

    // Verify guidance explicitly explains generated files and operator remediation
    assert!(ONBOARDING_FINALIZE_GUIDANCE.contains(".gitignore"));
    assert!(ONBOARDING_FINALIZE_GUIDANCE.contains(".orbit/auto_tasks"));
    assert!(ONBOARDING_FINALIZE_GUIDANCE.contains(".orbit/routines"));
    assert!(ONBOARDING_FINALIZE_GUIDANCE.contains("review and commit"));
    assert!(ONBOARDING_FINALIZE_GUIDANCE.contains("does not auto-commit or discard"));
    assert_eq!(
        onboarding_finalize_guidance(workspace.path(), &workspace.path().join(".orbit")),
        ONBOARDING_FINALIZE_GUIDANCE,
        "checkout-local initialization must retain the commit guidance"
    );

    let _env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
    WorkspaceInitArgs {
        name: Some("guidance-test".to_string()),
        base_branch: Some("agent-main".to_string()),
        ship_mode: Some("local".to_string()),
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: false,
    }
    .execute_without_runtime(None)
    .expect("workspace init");

    // Verify the generated files exist
    assert!(workspace.path().join(".gitignore").exists());
    assert!(workspace.path().join(".orbit/auto_tasks").is_dir());
    assert!(workspace.path().join(".orbit/routines").is_dir());

    // Git status shows dirt from generated files
    let status_output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace.path())
        .output()
        .expect("git status");
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        status_str.contains(".gitignore"),
        "git status: {status_str}"
    );
    assert!(status_str.contains(".orbit/"), "git status: {status_str}");
}
