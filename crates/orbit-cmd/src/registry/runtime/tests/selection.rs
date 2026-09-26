use super::*;

#[test]
pub(super) fn bootstrap_hint_stays_within_its_registered_git_repository() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let global = home.join(".orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_nested_hint\"\nname = \"nested-hint\"\ntask_prefix = \"ORB\"\n",
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

pub(super) fn unsupported_workspace_message(error: OrbitError, selector: &str) -> String {
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
pub(super) fn cli_tool_run_lists_the_named_workspace_not_the_cwd_runtime() {
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
pub(super) fn local_host_qualified_selector_binds_and_foreign_host_fails_closed() {
    let fixture = dual_workspace_fixture();
    let selected = execute_cli_tool(
        &fixture.alpha,
        "orbit.task.list",
        json!({ "workspace": "hm_cli_bind/ws_beta", "limit": 10 }),
    )
    .expect("local federated selector binds beta");
    assert!(task_ids(&selected).contains(&fixture.beta_task_id));

    let global = fixture.alpha.global_root();
    let runtime = RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&global),
        Some("hm_cli_bind/ws_beta"),
    )
    .expect("global --workspace accepts the local federated selector");
    assert_eq!(runtime.paths().repo_root, fixture.beta_repo);

    let error = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&global),
        Some("hm_other/ws_beta"),
    ) {
        Ok(_) => panic!("foreign qualified selector must fail closed"),
        Err(error) => error,
    };
    let message = unsupported_workspace_message(error, "hm_other/ws_beta");
    assert!(message.contains("host 'hm_other'"), "{message}");
}

#[test]
pub(super) fn cli_tool_run_fails_closed_on_unresolvable_workspace_for_read_and_write() {
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
pub(super) fn cli_tool_run_write_rebounds_to_the_named_workspace() {
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
pub(super) fn initialize_with_workspace_selector_binds_the_named_checkout() {
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

pub(super) fn task_ids(value: &Value) -> Vec<String> {
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
pub(super) struct ManagedWorktreeFixture {
    pub(super) _root: tempfile::TempDir,
    pub(super) registry_root: PathBuf,
    pub(super) repo_root: PathBuf,
    pub(super) worktree_root: PathBuf,
    pub(super) task_id: String,
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

pub(super) fn run_tool(
    runtime: &OrbitRuntime,
    name: &str,
    input: Value,
) -> Result<Value, OrbitError> {
    runtime.execute_tool_command(
        name,
        input,
        Some("codex".to_string()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
    )
}

pub(super) fn managed_worktree_fixture() -> ManagedWorktreeFixture {
    let root = tempfile::tempdir().expect("root");
    let registry_root = root.path().join("registry");
    std::fs::create_dir_all(&registry_root).expect("registry root");
    std::fs::write(
        registry_root.join("config.toml"),
        "[machine]\nid = \"hm_managed\"\nname = \"managed\"\ntask_prefix = \"ORB\"\n",
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

pub(super) fn set_readonly(path: &Path, readonly: bool) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .expect("fixture path metadata")
        .permissions();
    permissions.set_mode(if readonly { 0o555 } else { 0o755 });
    std::fs::set_permissions(path, permissions).expect("fixture permissions");
}

#[test]
pub(super) fn managed_registry_root_serves_task_show_and_workspace_scoped_update() {
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
pub(super) fn explicit_operator_root_still_pins_data_root_and_invalid_selector_fails_closed() {
    let fixture = managed_worktree_fixture();

    assert!(
        select_for_cwd_and_roots(&fixture.worktree_root, &fixture.pinned_registry_roots())
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
pub(super) fn managed_registry_locator_routes_linked_worktree_to_authoritative_store() {
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
    assert_eq!(
        explicitly_selected.local_root(),
        fixture.repo_root.join(".orbit"),
        "logical selectors inspect the registered primary checkout"
    );
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
pub(super) fn managed_workspace_envelope_routes_linked_worktree_tools_to_canonical_workspace() {
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
pub(super) fn read_only_authoritative_store_reports_a_path_attributed_permission_error() {
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
pub(super) fn unpinned_root_resolution_still_binds_the_registered_checkout() {
    let fixture = managed_worktree_fixture();
    let roots = fixture.unpinned_roots();

    let from_checkout = select_for_cwd_and_roots(&fixture.repo_root, &roots)
        .expect("selection from the registered checkout")
        .expect("registered checkout must bind");
    assert_eq!(from_checkout.workspace.id, "ws_managed");

    let from_worktree = select_for_cwd_and_roots(&fixture.worktree_root, &roots)
        .expect("selection from the linked worktree")
        .expect("shared-root fallback must bind the registered checkout");
    assert_eq!(from_worktree.workspace.id, "ws_managed");
    assert_eq!(from_worktree.checkout.repo_root, fixture.repo_root);
}

pub(super) struct CollidingIdNameFixture {
    pub(super) _root: tempfile::TempDir,
    pub(super) global: PathBuf,
    pub(super) alpha_repo: PathBuf,
    pub(super) alpha_orbit_dir: PathBuf,
}
