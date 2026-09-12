use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::{Value, json};

use orbit_registry::workspace_registry::{
    self, load_registry_from, registry_path_for, save_registry_to,
};

use crate::registry_runtime::{
    RegisteredRuntimeFactory, resolved_workspace_binding, select_workspace_for_cwd_and_roots,
    sync_task_prefix, workspace_runtime_binding,
};

fn workspace(id: &str, ship_mode: &str) -> Workspace {
    Workspace {
        id: id.to_string(),
        name: "orbit".to_string(),
        owner_machine_id: Some("hm_owner".to_string()),
        git_remote: None,
        ship_mode: Some(ship_mode.to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn binding_preserves_logical_and_runtime_ids_and_ship_mode() {
    let root = tempfile::tempdir().expect("root");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_runtime_config".to_string(),
        },
    )
    .expect("workspace config");
    let workspace = workspace("logical-abc123", "pr");
    let checkout = WorkspaceCheckout::owner(workspace.id.clone(), repo.clone(), orbit_dir.clone());

    let resolved = resolved_workspace_binding(&workspace, &checkout).expect("resolved binding");
    assert_eq!(resolved.logical_workspace_id, "logical-abc123");
    assert_eq!(resolved.runtime.logical_workspace_id, "logical-abc123");
    assert_eq!(resolved.runtime.task_partition_id, "ws_runtime_config");
    assert_eq!(
        resolved.runtime.owner_machine_id.as_deref(),
        Some("hm_owner")
    );
    assert_eq!(resolved.runtime.repo_root, repo);
    assert_eq!(resolved.runtime.ship_mode.as_input_value(), "pr");
    assert_eq!(resolved.runtime.base_branch.as_deref(), Some("agent-main"));

    let direct = workspace_runtime_binding(&workspace, &checkout).expect("core binding");
    assert_eq!(direct, resolved.runtime);
}

#[test]
fn registered_checkout_opens_a_bound_runtime() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_runtime".to_string(),
        },
    )
    .expect("workspace config");
    let workspace = workspace("logical-abc123", "local");
    let checkout = WorkspaceCheckout::owner(workspace.id.clone(), repo.clone(), orbit_dir);

    std::fs::write(global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n")
        .expect("host identity");
    let runtime =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &workspace, &checkout)
            .expect("bound runtime");
    assert_eq!(runtime.automation_machine_identity(), Some("hm_local"));
    let binding = runtime
        .workspace_runtime_binding()
        .expect("runtime binding");
    assert_eq!(binding.task_partition_id, "ws_runtime");
    assert_eq!(binding.repo_root, repo);
    assert_eq!(binding.ship_mode.as_input_value(), "local");
    assert_eq!(
        binding.owner_machine_id.as_deref(),
        Some("hm_owner"),
        "delivery automation resolves its default owner from this registry fact"
    );

    assert!(matches!(
        runtime.run_tool("orbit.workspace.list", json!({})),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Tool,
            ..
        })
    ));
}

#[test]
fn explicit_shared_root_selects_checkout_by_cwd_and_does_not_fall_back() {
    let root = tempfile::tempdir().expect("root");
    let shared = root.path().join("shared");
    let alpha_repo = root.path().join("alpha");
    let beta_repo = root.path().join("beta");
    let unregistered_repo = root.path().join("unregistered");
    for directory in [&shared, &alpha_repo, &beta_repo, &unregistered_repo] {
        std::fs::create_dir_all(directory).expect("fixture directory");
    }
    write_workspace_config(
        &shared,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_shared_runtime".to_string(),
        },
    )
    .expect("workspace config");

    let mut alpha = workspace("ws_alpha", "pr");
    alpha.name = "alpha".to_string();
    let mut beta = workspace("ws_beta", "local");
    beta.name = "beta".to_string();
    let alpha_checkout = WorkspaceCheckout::owner(alpha.id.clone(), alpha_repo, shared.clone());
    let beta_checkout =
        WorkspaceCheckout::owner(beta.id.clone(), beta_repo.clone(), shared.clone());
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![alpha, beta],
            checkouts: vec![alpha_checkout, beta_checkout],
            ..Default::default()
        },
        &registry_path_for(&shared),
    )
    .expect("shared registry");
    let roots = OrbitRuntimeRoots {
        global_root: shared.clone(),
        shared_root: shared.clone(),
        local_root: shared,
    };

    let selected = select_workspace_for_cwd_and_roots(&beta_repo, &roots)
        .expect("select beta")
        .expect("registered beta");
    assert_eq!(selected.workspace.id, "ws_beta");
    assert_eq!(selected.checkout.repo_root, beta_repo);
    assert_eq!(
        workspace_runtime_binding(&selected.workspace, &selected.checkout)
            .expect("runtime binding")
            .task_partition_id,
        "ws_shared_runtime"
    );

    assert!(
        select_workspace_for_cwd_and_roots(&unregistered_repo, &roots)
            .expect("unregistered selection")
            .is_none(),
        "a shared orbit_dir must not select the first registered checkout"
    );
}

#[test]
fn registered_checkout_task_creation_uses_host_task_prefix() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_runtime_test\"\nhost_id = \"runtime-test\"\ntask_prefix = \"DE\"\n",
    )
    .expect("host identity");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_prefixed_runtime".to_string(),
        },
    )
    .expect("workspace config");
    let workspace = workspace("logical-prefixed", "local");
    let checkout = WorkspaceCheckout::owner(workspace.id.clone(), repo, orbit_dir);
    let runtime =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &workspace, &checkout)
            .expect("bound runtime");

    let task = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Prefix-aware task",
                "description": "Mint through the normal task creation surface.",
                "complexity": "low",
                "workspace": "."
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task creation");
    assert_eq!(task["id"], "DE-00000");
}

#[test]
fn sync_task_prefix_rejects_legacy_host_identity() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("host.toml"), "host_id = \"legacy\"\n")
        .expect("legacy host identity");

    let error =
        sync_task_prefix(root.path()).expect_err("legacy host identity must require migration");

    assert!(
        matches!(error, OrbitError::InvalidInput(message) if message.contains("legacy pre-migration"))
    );
}

#[cfg(unix)]
#[test]
fn sync_task_prefix_rejects_a_symlinked_host_identity() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    let outside_identity = outside.path().join("host.toml");
    std::fs::write(
        &outside_identity,
        "schema_version = 2\nmachine_id = \"hm_outside\"\nhost_id = \"outside\"\ntask_prefix = \"DE\"\n",
    )
    .expect("outside host identity");
    symlink(&outside_identity, root.path().join("host.toml")).expect("host identity symlink");

    let error =
        sync_task_prefix(root.path()).expect_err("symlinked host identity must fail closed");

    assert!(
        matches!(error, OrbitError::InvalidInput(message) if message.contains("regular host.toml"))
    );
}

#[test]
fn replica_runtime_refuses_task_writes_and_hides_coordination_reads() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let repo = root.path().join("replica");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::create_dir_all(&orbit_dir).expect("orbit directory");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_runtime".to_string(),
        },
    )
    .expect("workspace config");
    let workspace = workspace("logical-replica", "local");
    let checkout = WorkspaceCheckout {
        workspace_id: workspace.id.clone(),
        repo_root: repo,
        orbit_dir,
        role: Some(WorkspaceCheckoutRole::Replica),
        owner_machine_id: Some("hm_owner".to_string()),
        path_overrides: Vec::new(),
    };
    let runtime =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &workspace, &checkout)
            .expect("replica runtime");

    let error = runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "must not fork".to_string(),
            ..Default::default()
        })
        .expect_err("replica task write must fail closed");
    // A catalog-role refusal, not a malformed call: federated routing has to
    // report `capability_refused` rather than `invalid_input` [ORB-11012].
    assert!(
        matches!(&error, OrbitError::CapabilityRefused(message) if message.contains("hm_owner")),
        "{error}"
    );
    assert!(
        runtime
            .list_tasks()
            .expect("empty replica task list")
            .is_empty()
    );

    // The gate covers control-plane coordination only. A replica is the
    // execution binding, so execute-class tools pass it and are judged on
    // their own terms — here the operator-capability axis, which stays a
    // distinct `capability_denied` rather than a catalog-role refusal.
    for (name, input) in [
        ("orbit.workflow.run.show", json!({"run_id": "jrun-missing"})),
        ("orbit.command.exec", json!({"command": "true"})),
    ] {
        let error = execute_cli_tool(&runtime, name, input)
            .expect_err("both execute-class tools are operator surfaces");
        assert!(
            matches!(&error, OrbitError::CapabilityDenied(message) if message.contains("operator")),
            "{name} must clear the catalog-role gate: {error}"
        );
    }
}

struct DualWorkspaceFixture {
    _root: tempfile::TempDir,
    alpha: OrbitRuntime,
    beta_repo: PathBuf,
    beta_task_id: String,
}

fn execute_cli_tool(
    runtime: &OrbitRuntime,
    name: &str,
    mut input: Value,
) -> Result<Value, OrbitError> {
    let bound = RegisteredRuntimeFactory::bind_cli_tool_workspace(runtime, &mut input)?;
    bound.as_ref().unwrap_or(runtime).execute_tool_command(
        name,
        input,
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    )
}

fn dual_workspace_fixture() -> DualWorkspaceFixture {
    use orbit_types::workspace::WorkspaceRegistry;

    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_cli_bind\"\nhost_id = \"cli-bind\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (ws_alpha, checkout_alpha) =
        registered_workspace(root.path(), "ws_alpha", "alpha", "hm_cli_bind");
    let (ws_beta, checkout_beta) =
        registered_workspace(root.path(), "ws_beta", "beta", "hm_cli_bind");
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![ws_alpha.clone(), ws_beta.clone()],
            checkouts: vec![checkout_alpha.clone(), checkout_beta.clone()],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let alpha =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &ws_alpha, &checkout_alpha)
            .expect("alpha runtime");
    let beta =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &ws_beta, &checkout_beta)
            .expect("beta runtime");
    let created = beta
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Beta-only task",
                "description": "Lives in the beta workspace.",
                "complexity": "low",
                "workspace": checkout_beta.repo_root
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("seed beta task");
    DualWorkspaceFixture {
        _root: root,
        alpha,
        beta_repo: checkout_beta.repo_root,
        beta_task_id: created["id"].as_str().expect("created task id").to_string(),
    }
}

fn registered_workspace(
    root: &Path,
    id: &str,
    name: &str,
    owner_machine_id: &str,
) -> (Workspace, WorkspaceCheckout) {
    let repo = root.join(name);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: id.to_string(),
        },
    )
    .expect("workspace config");
    let workspace = Workspace {
        id: id.to_string(),
        name: name.to_string(),
        owner_machine_id: Some(owner_machine_id.to_string()),
        git_remote: None,
        ship_mode: Some("local".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let checkout = WorkspaceCheckout::owner(id.to_string(), repo, orbit_dir);
    (workspace, checkout)
}

#[test]
fn workspace_registered_during_selector_resolution_survives_validation_save() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global root");

    let mut stale_workspace = workspace("ws_stale", "local");
    stale_workspace.name = "stale".to_string();
    let stale_repo = root.path().join("missing");
    let stale_checkout = WorkspaceCheckout::owner(
        stale_workspace.id.clone(),
        stale_repo.clone(),
        stale_repo.join(".orbit"),
    );
    let registry_path = registry_path_for(&global);
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![stale_workspace],
            checkouts: vec![stale_checkout],
            ..Default::default()
        },
        &registry_path,
    )
    .expect("initial registry");

    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let resolver_global = global.clone();
    let (new_workspace, new_checkout) =
        registered_workspace(root.path(), "ws_new", "new", "hm_owner");
    let mut resolver = None;
    workspace_registry::with_registry_lock(&registry_path, || {
        resolver = Some(std::thread::spawn(move || {
            started_tx.send(()).expect("announce resolution");
            let result =
                RegisteredRuntimeFactory::resolve_workspace_selector(&resolver_global, "ws_new");
            finished_tx.send(()).expect("announce completion");
            result
        }));
        started_rx.recv().expect("resolution started");
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(500))
                .is_err(),
            "selector resolution must wait for the registry lock"
        );

        let mut registry = load_registry_from(&registry_path)?;
        registry.workspaces.push(new_workspace);
        registry.checkouts.push(new_checkout);
        save_registry_to(&registry, &registry_path)
    })
    .expect("register workspace while resolution waits");

    let selected = resolver
        .expect("selector thread started")
        .join()
        .expect("selector thread")
        .expect("new workspace resolves after registration");
    assert_eq!(selected.workspace.id, "ws_new");

    let registry = load_registry_from(&registry_path).expect("final registry");
    assert!(
        registry
            .workspaces
            .iter()
            .any(|workspace| workspace.id == "ws_new"),
        "validation save must retain the concurrently registered workspace"
    );
    assert_eq!(
        registry
            .workspaces
            .iter()
            .find(|workspace| workspace.id == "ws_stale")
            .map(|workspace| workspace.status.clone()),
        Some(WorkspaceStatus::Invalid),
        "resolution must exercise and persist the validation write"
    );
}

#[test]
fn bootstrap_hint_stays_within_its_registered_git_repository() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let global = home.join(".orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_nested_hint\"\nhost_id = \"nested-hint\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (parent, checkout) =
        registered_workspace(root.path(), "ws_parent", "parent", "hm_nested_hint");
    std::fs::create_dir_all(checkout.repo_root.join(".git")).expect("parent git directory");
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![parent],
            checkouts: vec![checkout.clone()],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let same_repo_subdir = checkout.repo_root.join("packages/demo");
    let independent_child = checkout.repo_root.join("codebases/child");
    std::fs::create_dir_all(&same_repo_subdir).expect("same-repo subdirectory");
    std::fs::create_dir_all(independent_child.join(".git")).expect("child git directory");
    let home_var = home.to_string_lossy().into_owned();
    let _env = orbit_common::test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", None),
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_WORKSPACE", None),
    ]);

    let same_repo =
        RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&same_repo_subdir, None)
            .expect("resolve registered subdirectory");
    assert_eq!(same_repo.shared_root, checkout.orbit_dir);

    let child = RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&independent_child, None)
        .expect("resolve independent child repository");
    assert_eq!(child.shared_root, independent_child.join(".orbit"));
    assert_eq!(child.local_root, independent_child.join(".orbit"));
    assert_eq!(child.global_root, global);
}

fn unsupported_workspace_message(error: OrbitError, selector: &str) -> String {
    match error {
        OrbitError::InvalidInput(message) => {
            assert!(
                message.contains(selector),
                "error must name the rejected workspace '{selector}': {message}"
            );
            message
        }
        other => panic!("expected InvalidInput, got {other}"),
    }
}

#[test]
fn cli_tool_run_lists_the_named_workspace_not_the_cwd_runtime() {
    let fixture = dual_workspace_fixture();
    let cwd_list = execute_cli_tool(&fixture.alpha, "orbit.task.list", json!({ "limit": 10 }))
        .expect("list without workspace stays on alpha");
    assert!(
        !task_ids(&cwd_list).contains(&fixture.beta_task_id),
        "cwd-bound list must not silently return the other workspace: {cwd_list}"
    );

    let named = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.list",
        json!({ "workspace": "beta", "limit": 10 }),
    )
    .expect("list should rebind to the named workspace");
    assert!(
        task_ids(&named).contains(&fixture.beta_task_id),
        "named workspace list must return beta tasks: {named}"
    );

    let by_path = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.list",
        json!({
            "workspace": fixture.beta_repo,
            "limit": 10
        }),
    )
    .expect("list should rebind to the checkout path");
    assert!(
        task_ids(&by_path).contains(&fixture.beta_task_id),
        "absolute checkout path must return beta tasks: {by_path}"
    );

    let by_id = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.list",
        json!({ "workspace": "ws_beta", "limit": 10 }),
    )
    .expect("list should rebind to the logical id");
    assert!(
        task_ids(&by_id).contains(&fixture.beta_task_id),
        "logical workspace id must return beta tasks: {by_id}"
    );
}

#[test]
fn cli_tool_run_fails_closed_on_unresolvable_workspace_for_read_and_write() {
    let fixture = dual_workspace_fixture();
    const BOGUS: &str = "bogus-nonexistent-xyz";

    let list_error = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.list",
        json!({ "workspace": BOGUS, "limit": 2 }),
    )
    .expect_err("unresolvable workspace must not list the cwd workspace");
    unsupported_workspace_message(list_error, BOGUS);

    let add_error = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.add",
        json!({
            "title": "must not land in cwd",
            "description": "unresolvable workspace must fail closed",
            "complexity": "low",
            "workspace": BOGUS
        }),
    )
    .expect_err("unresolvable workspace must not create a cwd task");
    unsupported_workspace_message(add_error, BOGUS);

    let after = execute_cli_tool(&fixture.alpha, "orbit.task.list", json!({ "limit": 10 }))
        .expect("cwd list after failed add");
    assert!(
        task_ids(&after).is_empty(),
        "failed write must not create a task in the cwd workspace: {after}"
    );
}

#[test]
fn cli_tool_run_write_rebounds_to_the_named_workspace() {
    let fixture = dual_workspace_fixture();
    let created = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.add",
        json!({
            "title": "Filed onto beta by name",
            "description": "CLI workspace selector must rebind writes.",
            "complexity": "low",
            "workspace": "beta"
        }),
    )
    .expect("add should rebind to the named workspace");
    let created_id = created["id"].as_str().expect("created id").to_string();

    let alpha_list = execute_cli_tool(&fixture.alpha, "orbit.task.list", json!({ "limit": 10 }))
        .expect("alpha list");
    assert!(
        !task_ids(&alpha_list).contains(&created_id),
        "named-workspace write must not land in the cwd workspace: {alpha_list}"
    );

    let beta_list = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.list",
        json!({ "workspace": "beta", "limit": 10 }),
    )
    .expect("beta list");
    assert!(
        task_ids(&beta_list).contains(&created_id),
        "named-workspace write must be visible on the target workspace: {beta_list}"
    );
}

#[test]
fn initialize_with_workspace_selector_binds_the_named_checkout() {
    let fixture = dual_workspace_fixture();
    let global = fixture.alpha.global_root();
    let runtime = RegisteredRuntimeFactory::initialize_with_overrides(Some(&global), Some("beta"))
        .expect("selector should bind beta");
    let listed = runtime
        .execute_tool_command(
            "orbit.task.list",
            json!({ "limit": 10 }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("list beta");
    assert!(
        task_ids(&listed).contains(&fixture.beta_task_id),
        "initialize --workspace beta must open the beta checkout: {listed}"
    );

    let unknown = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&global),
        Some("no-such-workspace"),
    ) {
        Ok(_) => panic!("unknown selector must fail closed"),
        Err(error) => error,
    };
    unsupported_workspace_message(unknown, "no-such-workspace");
}

fn task_ids(value: &Value) -> Vec<String> {
    let items = value
        .as_array()
        .or_else(|| value.get("tasks").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|task| {
            task.get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

/// A managed run executes its CLI agent in a linked worktree that is not a
/// registered checkout and whose worktree-local `.orbit` is mounted read-only.
/// The managed registry locator must keep the authoritative global registry
/// distinct from both workspace roots while registered shared-root routing
/// still finds the injected task. [ORB-10980] [ORB-11066]
struct ManagedWorktreeFixture {
    _root: tempfile::TempDir,
    registry_root: PathBuf,
    repo_root: PathBuf,
    worktree_root: PathBuf,
    task_id: String,
}

impl ManagedWorktreeFixture {
    /// Explicit operator root semantics remain pinned and distinct from the
    /// managed registry-locator contract.
    fn pinned_registry_roots(&self) -> OrbitRuntimeRoots {
        OrbitRuntimeRoots {
            global_root: self.registry_root.clone(),
            shared_root: self.registry_root.clone(),
            local_root: self.registry_root.clone(),
        }
    }

    /// Roots as resolved with no root override: the registry stays distinct
    /// from the checkout's shared `.orbit` and the worktree-local one.
    fn unpinned_roots(&self) -> OrbitRuntimeRoots {
        OrbitRuntimeRoots {
            global_root: self.registry_root.clone(),
            shared_root: self.repo_root.join(".orbit"),
            local_root: self.worktree_root.join(".orbit"),
        }
    }

    fn task_store_dir(&self) -> PathBuf {
        let partitions = self.registry_root.join("tasks/workspaces");
        std::fs::read_dir(&partitions)
            .expect("task partitions")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join(&self.task_id))
            .find(|candidate| candidate.is_dir())
            .expect("authoritative task store directory")
    }

    fn show_runtime(&self) -> OrbitRuntime {
        crate::task_owner::initialize_for_task_show(Some(&self.registry_root), None, &self.task_id)
            .expect("task-show runtime from the registry root alone")
    }

    fn workspace_scoped_runtime(&self) -> OrbitRuntime {
        RegisteredRuntimeFactory::initialize_with_overrides(
            Some(&self.registry_root),
            Some(&self.repo_root.to_string_lossy()),
        )
        .expect("workspace-scoped runtime")
    }
}

fn run_tool(runtime: &OrbitRuntime, name: &str, input: Value) -> Result<Value, OrbitError> {
    runtime.execute_tool_command(
        name,
        input,
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    )
}

fn managed_worktree_fixture() -> ManagedWorktreeFixture {
    let root = tempfile::tempdir().expect("root");
    let registry_root = root.path().join("registry");
    std::fs::create_dir_all(&registry_root).expect("registry root");
    std::fs::write(
        registry_root.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_managed\"\nhost_id = \"managed\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (workspace, checkout) =
        registered_workspace(root.path(), "ws_managed", "managed", "hm_managed");
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![workspace.clone()],
            checkouts: vec![checkout.clone()],
            ..Default::default()
        },
        &registry_path_for(&registry_root),
    )
    .expect("workspace registry");

    let runtime =
        RegisteredRuntimeFactory::open_registered_checkout(&registry_root, &workspace, &checkout)
            .expect("registered runtime");
    let created = run_tool(
        &runtime,
        "orbit.task.add",
        json!({
            "title": "Injected managed task",
            "description": "Seeded into the authoritative registered store.",
            "complexity": "low",
            "workspace": checkout.repo_root
        }),
    )
    .expect("seed managed task");

    // The linked worktree is deliberately unregistered and its worktree-local
    // `.orbit` is read-only, exactly as the managed sandbox mounts it.
    let worktree_root = checkout
        .repo_root
        .join(".orbit/state/worktrees/jrun-managed-fixture");
    let worktree_git_dir = checkout
        .repo_root
        .join(".git/worktrees/jrun-managed-fixture");
    std::fs::create_dir_all(&worktree_root).expect("worktree root");
    std::fs::create_dir_all(&worktree_git_dir).expect("worktree git dir");
    std::fs::write(
        worktree_root.join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .expect("worktree gitfile");
    let worktree_state = worktree_root.join(".orbit");
    std::fs::create_dir_all(&worktree_state).expect("worktree state root");
    set_readonly(&worktree_state, true);

    ManagedWorktreeFixture {
        _root: root,
        registry_root,
        repo_root: checkout.repo_root,
        worktree_root,
        task_id: created["id"].as_str().expect("seeded task id").to_string(),
    }
}

fn set_readonly(path: &Path, readonly: bool) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .expect("fixture path metadata")
        .permissions();
    permissions.set_mode(if readonly { 0o555 } else { 0o755 });
    std::fs::set_permissions(path, permissions).expect("fixture permissions");
}

#[test]
fn managed_registry_root_serves_task_show_and_workspace_scoped_update() {
    let fixture = managed_worktree_fixture();

    let shown = run_tool(
        &fixture.show_runtime(),
        "orbit.task.show",
        json!({ "id": fixture.task_id }),
    )
    .expect("registry root alone must resolve the injected task");
    assert_eq!(shown["id"], fixture.task_id);

    run_tool(
        &fixture.workspace_scoped_runtime(),
        "orbit.task.update",
        json!({
            "id": fixture.task_id,
            "plan": "Authored through the documented CLI fallback."
        }),
    )
    .expect("workspace-scoped update must reach the registered store");

    let after = run_tool(
        &fixture.show_runtime(),
        "orbit.task.show",
        json!({ "id": fixture.task_id, "field": "plan" }),
    )
    .expect("re-read through the registry");
    assert_eq!(
        after, "Authored through the documented CLI fallback.",
        "the update must land in the authoritative registered store: {after}"
    );
}

#[test]
fn explicit_operator_root_still_pins_data_root_and_invalid_selector_fails_closed() {
    let fixture = managed_worktree_fixture();

    assert!(
        select_workspace_for_cwd_and_roots(
            &fixture.worktree_root,
            &fixture.pinned_registry_roots()
        )
        .expect("selection")
        .is_none(),
        "an unregistered worktree cwd must not silently bind a workspace under a pinned root"
    );

    let unknown = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.registry_root),
        Some("no-such-workspace"),
    ) {
        Ok(_) => panic!("an invalid selector must fail closed"),
        Err(error) => error,
    };
    unsupported_workspace_message(unknown, "no-such-workspace");
}

#[test]
fn managed_registry_locator_routes_linked_worktree_to_authoritative_store() {
    let fixture = managed_worktree_fixture();
    let provider_home = fixture._root.path().join("provider-home");
    std::fs::create_dir_all(&provider_home).expect("provider home");
    let home_var = provider_home.to_string_lossy().into_owned();
    let registry_var = fixture.registry_root.to_string_lossy().into_owned();
    let _env = orbit_common::test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", Some(registry_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-managed-fixture")),
    ]);

    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&fixture.worktree_root, None)
        .expect("resolve production-shaped managed roots");
    assert_eq!(roots, fixture.unpinned_roots());
    let bootstrap_roots =
        RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&fixture.worktree_root, None)
            .expect("resolve linked-worktree bootstrap roots");
    assert_eq!(bootstrap_roots, fixture.unpinned_roots());
    let runtime = RegisteredRuntimeFactory::open_resolved_roots(roots)
        .expect("open registered linked-worktree runtime");

    let shown = run_tool(
        &runtime,
        "orbit.task.show",
        json!({ "id": fixture.task_id }),
    )
    .expect("managed runtime must reach the authoritative task store");
    assert_eq!(shown["id"], fixture.task_id);
    assert_eq!(runtime.shared_root(), fixture.repo_root.join(".orbit"));
    assert_eq!(runtime.local_root(), fixture.worktree_root.join(".orbit"));
    assert!(
        !fixture.worktree_root.join(".orbit/tasks").exists(),
        "managed routing must not create a worktree-local shadow task store"
    );

    let explicitly_selected =
        RegisteredRuntimeFactory::initialize_with_overrides(None, Some("managed"))
            .expect("managed registry locator must serve explicit workspace selectors");
    let selected_task = run_tool(
        &explicitly_selected,
        "orbit.task.show",
        json!({ "id": fixture.task_id }),
    )
    .expect("selected managed workspace must use authoritative task store");
    assert_eq!(selected_task["id"], fixture.task_id);

    let unknown = match RegisteredRuntimeFactory::initialize_with_overrides(
        None,
        Some("no-such-managed-workspace"),
    ) {
        Ok(_) => panic!("unknown managed workspace selector must fail closed"),
        Err(error) => error,
    };
    unsupported_workspace_message(unknown, "no-such-managed-workspace");

    for workspace_only in [
        "state/job-runs",
        "state/diagnostics",
        "state/scoreboard",
        "state/worktrees",
        "knowledge",
    ] {
        assert!(
            !fixture.registry_root.join(workspace_only).exists(),
            "managed registry discovery must not create global workspace-only path {workspace_only}"
        );
    }
}

#[test]
fn managed_workspace_envelope_routes_linked_worktree_tools_to_canonical_workspace() {
    let fixture = managed_worktree_fixture();
    let worktree_state = fixture.worktree_root.join(".orbit");
    set_readonly(&worktree_state, false);
    write_workspace_config(
        &worktree_state,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "daniel-e9c542".to_string(),
        },
    )
    .expect("shadow worktree identity");
    set_readonly(&worktree_state, true);

    let provider_home = fixture._root.path().join("provider-home");
    std::fs::create_dir_all(&provider_home).expect("provider home");
    let home_var = provider_home.to_string_lossy().into_owned();
    let registry_var = fixture.registry_root.to_string_lossy().into_owned();
    let _env = orbit_common::test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", Some(registry_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-managed-fixture")),
        ("ORBIT_WORKSPACE", Some("ws_managed")),
    ]);

    let runtime = RegisteredRuntimeFactory::initialize_with_overrides(None, None)
        .expect("managed envelope must bind the canonical workspace");
    let binding = runtime
        .workspace_runtime_binding()
        .expect("managed envelope runtime is bound");
    assert_eq!(binding.logical_workspace_id, "ws_managed");
    assert_ne!(
        binding.logical_workspace_id, "daniel-e9c542",
        "linked-worktree identity must not become the durable workspace"
    );

    run_tool(
        &runtime,
        "orbit.task.update",
        json!({
            "id": fixture.task_id,
            "plan": "Authored through the managed envelope."
        }),
    )
    .expect("task update without a payload workspace must use the envelope");

    let friction = run_tool(
        &runtime,
        "orbit.friction.add",
        json!({
            "body": "Managed routing must not mint a shadow workspace.",
            "model": "codex"
        }),
    )
    .expect("friction add without a payload workspace must use the envelope");
    assert!(
        friction.get("id").and_then(Value::as_str).is_some(),
        "friction add must persist a record: {friction}"
    );

    let listed = run_tool(&runtime, "orbit.task.list", json!({ "limit": 10 }))
        .expect("task list without a payload workspace must use the envelope");
    assert!(
        task_ids(&listed).contains(&fixture.task_id),
        "canonical workspace list must include the injected task: {listed}"
    );

    let after = run_tool(
        &fixture.show_runtime(),
        "orbit.task.show",
        json!({ "id": fixture.task_id, "field": "plan" }),
    )
    .expect("re-read through the registry");
    assert_eq!(
        after, "Authored through the managed envelope.",
        "the update must land in the authoritative registered store: {after}"
    );

    assert!(
        !fixture.worktree_root.join(".orbit/tasks").exists(),
        "managed routing must not create a worktree-local shadow task store"
    );

    let unknown = match RegisteredRuntimeFactory::initialize_with_overrides(
        None,
        Some("no-such-managed-workspace"),
    ) {
        Ok(_) => panic!("an explicit invalid selector must fail closed without cwd fallback"),
        Err(error) => error,
    };
    unsupported_workspace_message(unknown, "no-such-managed-workspace");

    let rebound = execute_cli_tool(
        &runtime,
        "orbit.task.list",
        json!({ "workspace": "ws_managed", "limit": 10 }),
    )
    .expect("explicit valid selector continues to route");
    assert!(task_ids(&rebound).contains(&fixture.task_id));

    let mismatched = execute_cli_tool(
        &runtime,
        "orbit.task.update",
        json!({
            "id": fixture.task_id,
            "plan": "must not fall back",
            "workspace": "ws_not_registered"
        }),
    )
    .expect_err("mismatched selector must fail closed");
    unsupported_workspace_message(mismatched, "ws_not_registered");
}

#[test]
fn read_only_authoritative_store_reports_a_path_attributed_permission_error() {
    let fixture = managed_worktree_fixture();
    let store_dir = fixture.task_store_dir();
    let runtime = fixture.workspace_scoped_runtime();
    set_readonly(&store_dir, true);

    let error = run_tool(
        &runtime,
        "orbit.task.update",
        json!({
            "id": fixture.task_id,
            "plan": "Must not be written to a read-only store."
        }),
    )
    .expect_err("a genuine write against a read-only store must fail");
    let message = error.to_string();
    set_readonly(&store_dir, false);

    assert!(
        message.contains(&store_dir.display().to_string()),
        "a permission failure must name the path it could not write: {message}"
    );
    assert!(
        message.contains("Permission denied"),
        "a read-only store must be reported as a permission failure, not a missing task: {message}"
    );
}

#[test]
fn unpinned_root_resolution_still_binds_the_registered_checkout() {
    let fixture = managed_worktree_fixture();
    let roots = fixture.unpinned_roots();

    let from_checkout = select_workspace_for_cwd_and_roots(&fixture.repo_root, &roots)
        .expect("selection from the registered checkout")
        .expect("registered checkout must bind");
    assert_eq!(from_checkout.workspace.id, "ws_managed");

    let from_worktree = select_workspace_for_cwd_and_roots(&fixture.worktree_root, &roots)
        .expect("selection from the linked worktree")
        .expect("shared-root fallback must bind the registered checkout");
    assert_eq!(from_worktree.workspace.id, "ws_managed");
    assert_eq!(from_worktree.checkout.repo_root, fixture.repo_root);
}

struct CollidingIdNameFixture {
    _root: tempfile::TempDir,
    global: PathBuf,
    alpha_repo: PathBuf,
    alpha_orbit_dir: PathBuf,
}

fn colliding_id_name_fixture() -> CollidingIdNameFixture {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_collision\"\nhost_id = \"collision\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (ws_alpha, checkout_alpha) =
        registered_workspace(root.path(), "ws_alpha", "alpha", "hm_collision");
    let (ws_shadow, checkout_shadow) =
        registered_workspace(root.path(), "ws_ws_alpha", "ws_alpha", "hm_collision");
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![ws_alpha, ws_shadow],
            checkouts: vec![checkout_alpha.clone(), checkout_shadow],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    CollidingIdNameFixture {
        _root: root,
        global,
        alpha_repo: checkout_alpha.repo_root,
        alpha_orbit_dir: checkout_alpha.orbit_dir,
    }
}

#[test]
fn cwd_and_absolute_path_select_a_workspace_whose_id_collides_with_another_name() {
    let fixture = colliding_id_name_fixture();
    let roots = OrbitRuntimeRoots {
        global_root: fixture.global.clone(),
        shared_root: fixture.alpha_orbit_dir.clone(),
        local_root: fixture.alpha_orbit_dir.clone(),
    };

    let from_cwd = select_workspace_for_cwd_and_roots(&fixture.alpha_repo, &roots)
        .expect("cwd selection must not treat the checkout id as an ambiguous selector")
        .expect("alpha checkout must bind");
    assert_eq!(from_cwd.workspace.id, "ws_alpha");
    assert_eq!(from_cwd.workspace.name, "alpha");
    assert_eq!(from_cwd.checkout.repo_root, fixture.alpha_repo);

    let alpha_path = fixture.alpha_repo.to_str().expect("utf8 checkout path");
    let from_path = RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.global),
        Some(alpha_path),
    )
    .expect("absolute checkout path must bind alpha");
    let binding = from_path
        .workspace_runtime_binding()
        .expect("path-selected runtime is bound");
    assert_eq!(binding.logical_workspace_id, "ws_alpha");
    assert_eq!(binding.repo_root, fixture.alpha_repo);

    let ambiguous = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.global),
        Some("ws_alpha"),
    ) {
        Ok(_) => panic!("a directly entered colliding selector must fail closed"),
        Err(error) => error,
    };
    match ambiguous {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("ws_alpha"), "{message}");
            assert!(
                message.contains("ambiguous workspace selector"),
                "{message}"
            );
        }
        other => panic!("expected InvalidInput, got {other}"),
    }
}

#[test]
fn deleted_checkout_workspace_selector_reports_inactive_status_and_recorded_path() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_deleted_test\"\nhost_id = \"deleted-test\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (ws_alpha, checkout_alpha) =
        registered_workspace(root.path(), "ws_alpha", "alpha", "hm_deleted_test");
    let (ws_beta, checkout_beta) =
        registered_workspace(root.path(), "ws_beta", "beta", "hm_deleted_test");

    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![ws_alpha.clone(), ws_beta.clone()],
            checkouts: vec![checkout_alpha.clone(), checkout_beta.clone()],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let alpha_runtime =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &ws_alpha, &checkout_alpha)
            .expect("alpha runtime");

    // Delete beta checkout without teardown
    std::fs::remove_dir_all(&checkout_beta.repo_root).expect("remove beta checkout");
    let beta_repo_str = checkout_beta.repo_root.to_str().expect("utf8 beta path");

    let selectors = [
        ("registered name", "beta"),
        ("logical id", "ws_beta"),
        ("checkout path", beta_repo_str),
    ];

    for (label, selector) in selectors {
        // Direct resolution via RegisteredRuntimeFactory
        let direct_err = match RegisteredRuntimeFactory::initialize_with_overrides(
            Some(&global),
            Some(selector),
        ) {
            Ok(_) => panic!("deleted checkout workspace must not resolve ({label})"),
            Err(e) => e,
        };
        let msg = match direct_err {
            OrbitError::InvalidInput(msg) => msg,
            other => panic!("expected InvalidInput for {label}, got {other}"),
        };
        assert!(
            msg.contains("workspace 'beta' (ws_beta) is invalid on this machine"),
            "error for {label} must report name, id, and invalid status: {msg}"
        );
        assert!(
            msg.contains(beta_repo_str),
            "error for {label} must report recorded checkout path: {msg}"
        );
        assert!(
            !msg.contains("unknown workspace selector"),
            "error for {label} must not report unknown workspace selector: {msg}"
        );

        // Rebinding via cli_tool on an active runtime
        let tool_err = execute_cli_tool(
            &alpha_runtime,
            "orbit.task.list",
            json!({ "workspace": selector, "limit": 10 }),
        )
        .expect_err("rebinding to deleted checkout workspace must fail");
        let tool_msg = match tool_err {
            OrbitError::InvalidInput(msg) => msg,
            other => panic!("expected InvalidInput for {label}, got {other}"),
        };
        assert!(
            tool_msg.contains("workspace 'beta' (ws_beta) is invalid on this machine"),
            "tool error for {label} must report invalid status: {tool_msg}"
        );
        assert!(
            tool_msg.contains(beta_repo_str),
            "tool error for {label} must report recorded checkout path: {tool_msg}"
        );
    }

    // Distinct message for unknown selector
    let unknown_direct = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&global),
        Some("unknown-workspace"),
    ) {
        Ok(_) => panic!("unknown selector must fail"),
        Err(e) => e,
    };
    unsupported_workspace_message(unknown_direct, "unknown-workspace");

    let unknown_tool = execute_cli_tool(
        &alpha_runtime,
        "orbit.task.list",
        json!({ "workspace": "unknown-workspace", "limit": 10 }),
    )
    .expect_err("unknown selector tool call must fail");
    unsupported_workspace_message(unknown_tool, "unknown-workspace");
}

#[test]
fn non_active_workspace_with_readable_orbit_root_fails_to_bind_for_read_verbs() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_readable_test\"\nhost_id = \"readable-test\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (ws_alpha, checkout_alpha) =
        registered_workspace(root.path(), "ws_alpha", "alpha", "hm_readable_test");

    // Create gamma with split checkout: repo_root is deleted, but orbit_dir is readable
    let gamma_repo = root.path().join("gamma_repo");
    let gamma_orbit_dir = root.path().join("gamma_orbit");
    std::fs::create_dir_all(&gamma_orbit_dir).expect("gamma orbit dir");
    write_workspace_config(
        &gamma_orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_gamma".to_string(),
        },
    )
    .expect("gamma workspace config");

    let ws_gamma = Workspace {
        id: "ws_gamma".to_string(),
        name: "gamma".to_string(),
        owner_machine_id: Some("hm_readable_test".to_string()),
        git_remote: None,
        ship_mode: Some("local".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Invalid,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let checkout_gamma = WorkspaceCheckout::owner(
        "ws_gamma".to_string(),
        gamma_repo.clone(),
        gamma_orbit_dir.clone(),
    );

    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![ws_alpha.clone(), ws_gamma.clone()],
            checkouts: vec![checkout_alpha.clone(), checkout_gamma.clone()],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let alpha_runtime =
        RegisteredRuntimeFactory::open_registered_checkout(&global, &ws_alpha, &checkout_alpha)
            .expect("alpha runtime");

    assert!(!gamma_repo.exists(), "gamma repo checkout must not exist");
    assert!(
        gamma_orbit_dir.exists(),
        "gamma orbit root must still be readable"
    );

    let gamma_repo_str = gamma_repo.to_str().expect("utf8 gamma path");
    let selectors = [
        ("registered name", "gamma"),
        ("logical id", "ws_gamma"),
        ("checkout path", gamma_repo_str),
    ];

    for (label, selector) in selectors {
        let err = match RegisteredRuntimeFactory::initialize_with_overrides(
            Some(&global),
            Some(selector),
        ) {
            Ok(_) => panic!("invalid workspace must not resolve ({label})"),
            Err(e) => e,
        };
        let msg = match err {
            OrbitError::InvalidInput(msg) => msg,
            other => panic!("expected InvalidInput for {label}, got {other}"),
        };
        assert!(
            msg.contains("workspace 'gamma' (ws_gamma) is invalid on this machine"),
            "error for {label} must report invalid status: {msg}"
        );
        assert!(
            msg.contains(gamma_repo_str),
            "error for {label} must report recorded checkout path: {msg}"
        );

        let tool_err = execute_cli_tool(
            &alpha_runtime,
            "orbit.task.list",
            json!({ "workspace": selector, "limit": 10 }),
        )
        .expect_err("rebinding to invalid workspace must fail");
        let tool_msg = match tool_err {
            OrbitError::InvalidInput(msg) => msg,
            other => panic!("expected InvalidInput for {label}, got {other}"),
        };
        assert!(
            tool_msg.contains("workspace 'gamma' (ws_gamma) is invalid on this machine"),
            "tool error for {label} must report invalid status: {tool_msg}"
        );
    }
}
