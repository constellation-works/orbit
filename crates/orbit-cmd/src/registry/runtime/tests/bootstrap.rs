use super::*;

/// Select against the registry under `roots.global_root`, as the runtime open
/// does with the registry it loaded once up front.
pub(super) fn select_for_cwd_and_roots(
    cwd: &Path,
    roots: &OrbitRuntimeRoots,
) -> Result<Option<ResolvedWorkspaceSelection>, OrbitError> {
    let registry = load_registry_from(&registry_path_for(&roots.global_root))?;
    select_workspace_for_cwd_and_roots(cwd, roots, &registry)
}

#[test]
pub(super) fn pipeline_worker_bootstrap_retries_only_typed_sqlite_contention() {
    let contention = || {
        OrbitError::SqliteContention(Box::new(orbit_common::SqliteContention {
            path: "/isolated/audit.db".to_string(),
            phase: "set synchronous=NORMAL".to_string(),
            detail: "database is locked".to_string(),
        }))
    };
    let mut attempts = 0;
    let recovered = retry_pipeline_worker_bootstrap(
        || {
            attempts += 1;
            if attempts < 3 {
                Err(contention())
            } else {
                Ok("claimed-once")
            }
        },
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .expect("contention released inside the worker budget");
    assert_eq!(recovered, "claimed-once");
    assert_eq!(attempts, 3);

    let started = Instant::now();
    let error = retry_pipeline_worker_bootstrap(
        || Err::<(), _>(OrbitError::InvalidInput("unchanged".to_string())),
        Duration::from_secs(1),
        Duration::from_millis(50),
    )
    .expect_err("non-lock failures are not retried");
    assert!(matches!(error, OrbitError::InvalidInput(message) if message == "unchanged"));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
pub(super) fn pipeline_worker_bootstrap_deadline_returns_precise_last_contention() {
    let started = Instant::now();
    let error = retry_pipeline_worker_bootstrap(
        || {
            Err::<(), _>(OrbitError::SqliteContention(Box::new(
                orbit_common::SqliteContention {
                    path: "/isolated/tasks/registry.db".to_string(),
                    phase: "task prefix write admission".to_string(),
                    detail: "database is busy".to_string(),
                },
            )))
        },
        Duration::from_millis(20),
        Duration::from_millis(2),
    )
    .expect_err("deadline exhaustion returns the last typed failure");
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert!(started.elapsed() < Duration::from_millis(500));
    let message = error.to_string();
    assert!(message.contains("/isolated/tasks/registry.db"), "{message}");
    assert!(message.contains("task prefix write admission"), "{message}");
}

#[test]
pub(super) fn managed_worker_current_schema_bootstrap_does_not_wait_for_audit_writer() {
    let fixture = managed_worktree_fixture();
    let audit_db = fixture.registry_root.join("orbit.db");
    let blocker_store = orbit_store::Store::open(&audit_db).expect("open audit blocker");
    let blocker_connection = blocker_store.connection();
    let blocker = blocker_connection.lock().expect("lock audit blocker");
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold audit WAL writer");

    let started = Instant::now();
    let runtime = RegisteredRuntimeFactory::initialize_pipeline_worker_with_overrides(
        Some(&fixture.registry_root),
        Some(&fixture.repo_root.to_string_lossy()),
    )
    .expect("current-schema worker bootstrap only observes the audit store");

    assert!(started.elapsed() < Duration::from_secs(4));
    let shown = run_tool(
        &runtime,
        "orbit.task.show",
        json!({ "id": fixture.task_id }),
    )
    .expect("recovered worker runtime retains authoritative task ownership");
    assert_eq!(shown["id"], fixture.task_id);
    blocker
        .execute_batch("ROLLBACK")
        .expect("release audit writer");
}

pub(super) fn workspace(id: &str, ship_mode: &str) -> Workspace {
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
pub(super) fn binding_preserves_logical_and_runtime_ids_and_ship_mode() {
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
pub(super) fn registered_checkout_opens_a_bound_runtime() {
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

    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_local\"\nname = \"local\"\ntask_prefix = \"ORB\"\n",
    )
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
pub(super) fn explicit_shared_root_selects_checkout_by_cwd_and_does_not_fall_back() {
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

    let selected = select_for_cwd_and_roots(&beta_repo, &roots)
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
        select_for_cwd_and_roots(&unregistered_repo, &roots)
            .expect("unregistered selection")
            .is_none(),
        "a shared orbit_dir must not select the first registered checkout"
    );
}

#[test]
pub(super) fn registered_checkout_task_creation_uses_host_task_prefix() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_runtime_test\"\nname = \"runtime-test\"\ntask_prefix = \"DE\"\n",
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

/// A half-written `[machine]` table names what is missing rather than
/// projecting a namespace from an identity that does not exist.
#[test]
pub(super) fn sync_task_prefix_rejects_a_partial_machine_identity() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(
        root.path().join("config.toml"),
        "[machine]\nname = \"partial\"\n",
    )
    .expect("partial machine identity");

    let error = sync_task_prefix(root.path()).expect_err("a partial identity must require repair");

    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::InvalidInput(_)) && message.contains("machine.id"),
        "unexpected: {message}"
    );
}

/// An uninitialized root has no namespace to project, and says so by leaving
/// the allocator alone rather than failing every command.
#[test]
pub(super) fn sync_task_prefix_leaves_an_uninitialized_root_alone() {
    let root = tempfile::tempdir().expect("root");
    sync_task_prefix(root.path()).expect("an absent identity projects nothing");
}

#[test]
pub(super) fn replica_runtime_refuses_task_writes_and_hides_coordination_reads() {
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

pub(super) struct DualWorkspaceFixture {
    pub(super) _root: tempfile::TempDir,
    pub(super) alpha: OrbitRuntime,
    pub(super) beta_repo: PathBuf,
    pub(super) beta_task_id: String,
}

pub(super) fn execute_cli_tool(
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

pub(super) fn dual_workspace_fixture() -> DualWorkspaceFixture {
    use orbit_types::workspace::WorkspaceRegistry;

    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_cli_bind\"\nname = \"cli-bind\"\ntask_prefix = \"ORB\"\n",
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

pub(super) fn registered_workspace(
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
pub(super) fn workspace_registered_during_selector_resolution_survives_validation_save() {
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

/// Managed MCP calls and registered CLI tools both enter through these two
/// resolver seams. Run them as an unprivileged child so directory modes enforce
/// the same no-lock-file boundary as a read-only registry mount; the parent test
/// process may be root and would otherwise bypass ordinary permission bits.
#[cfg(unix)]
#[test]
pub(super) fn read_only_global_registry_supports_mcp_and_cli_workspace_bindings() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_READ_ONLY_SELECTOR_CHILD";
    const GLOBAL_ROOT: &str = "ORBIT_TEST_READ_ONLY_SELECTOR_GLOBAL";
    const ROOT: &str = "ORBIT_TEST_READ_ONLY_SELECTOR_ROOT";
    const TASK_ID: &str = "ORBIT_TEST_READ_ONLY_SELECTOR_TASK";

    if std::env::var_os(CHILD_MARKER).is_some() {
        let root = PathBuf::from(std::env::var(ROOT).expect("fixture root"));
        let global = PathBuf::from(std::env::var(GLOBAL_ROOT).expect("global root"));
        let task_id = std::env::var(TASK_ID).expect("task id");
        let lock_path = global.join(".workspaces.json.lock");
        let lock_error = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .expect_err("read-only registry root must reject the original lock open");
        assert!(
            matches!(
                lock_error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ),
            "unexpected lock error: {lock_error}"
        );

        let selected = RegisteredRuntimeFactory::resolve_workspace_selector(&global, "ws_beta")
            .expect("explicit MCP selector must be observational");
        let beta = RegisteredRuntimeFactory::open_registered_checkout(
            &global,
            &selected.workspace,
            &selected.checkout,
        )
        .expect("open MCP-selected workspace");
        run_tool(
            &beta,
            "orbit.task.update",
            json!({
                "id": task_id,
                "workspace": selected.checkout.repo_root,
                "plan": "Updated after explicit MCP workspace resolution."
            }),
        )
        .expect("MCP-selected runtime must reach writable assigned task state");

        let alpha =
            RegisteredRuntimeFactory::initialize_with_overrides(Some(&global), Some("ws_alpha"))
                .expect("open initial CLI runtime");
        execute_cli_tool(
            &alpha,
            "orbit.task.update",
            json!({
                "id": task_id,
                "workspace": "ws_beta",
                "plan": "Updated after registered CLI workspace binding."
            }),
        )
        .expect("CLI-bound runtime must reach writable assigned task state");

        for selector in ["ws_inactive", "ws_unknown"] {
            let error = RegisteredRuntimeFactory::resolve_workspace_selector(&global, selector)
                .expect_err("inactive and unknown MCP selectors must fail closed");
            assert!(
                error.to_string().contains(selector),
                "selector must be named: {error}"
            );

            let mut input = json!({"workspace": selector});
            let error = match RegisteredRuntimeFactory::bind_cli_tool_workspace(&alpha, &mut input)
            {
                Ok(_) => panic!("inactive and unknown CLI selectors must fail closed"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains(selector),
                "selector must be named: {error}"
            );
        }

        assert!(
            !lock_path.exists(),
            "unchanged-registry resolution must not create the global lock file"
        );
        assert!(root.join("beta/.orbit").is_dir());
        return;
    }

    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_read_only\"\nname = \"read-only\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (alpha_workspace, alpha_checkout) =
        registered_workspace(root.path(), "ws_alpha", "alpha", "hm_read_only");
    let (beta_workspace, beta_checkout) =
        registered_workspace(root.path(), "ws_beta", "beta", "hm_read_only");
    let (mut inactive_workspace, inactive_checkout) =
        registered_workspace(root.path(), "ws_inactive", "inactive", "hm_read_only");
    inactive_workspace.status = WorkspaceStatus::Invalid;
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![
                alpha_workspace.clone(),
                beta_workspace.clone(),
                inactive_workspace,
            ],
            checkouts: vec![
                alpha_checkout.clone(),
                beta_checkout.clone(),
                inactive_checkout,
            ],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let beta = RegisteredRuntimeFactory::open_registered_checkout(
        &global,
        &beta_workspace,
        &beta_checkout,
    )
    .expect("seed beta runtime");
    let created = run_tool(
        &beta,
        "orbit.task.add",
        json!({
            "title": "Read-only registry routing task",
            "description": "The assigned task state remains writable.",
            "complexity": "low",
            "workspace": beta_checkout.repo_root
        }),
    )
    .expect("seed assigned task");
    let task_id = created["id"].as_str().expect("created task id");

    fn make_writable(path: &Path) {
        let metadata = std::fs::metadata(path).expect("fixture metadata");
        let mode = if metadata.is_dir() { 0o777 } else { 0o666 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .expect("make assigned state writable");
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).expect("fixture directory") {
                make_writable(&entry.expect("fixture entry").path());
            }
        }
    }

    make_writable(&global.join("tasks"));
    for checkout in [&alpha_checkout, &beta_checkout] {
        make_writable(&checkout.repo_root);
    }
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make fixture root traversable");
    std::fs::set_permissions(
        global.join("workspaces.json"),
        std::fs::Permissions::from_mode(0o444),
    )
    .expect("make registry file read-only");
    std::fs::set_permissions(
        global.join("config.toml"),
        std::fs::Permissions::from_mode(0o444),
    )
    .expect("make host identity read-only");
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o555))
        .expect("make registry root read-only");

    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .arg("read_only_global_registry_supports_mcp_and_cli_workspace_bindings")
        .arg("--exact")
        .env(CHILD_MARKER, "1")
        .env(ROOT, root.path())
        .env(GLOBAL_ROOT, &global)
        .env(TASK_ID, task_id);
    // SAFETY: `geteuid` reads process credentials and has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        command.gid(65_534).uid(65_534);
    }
    let output = command.output().expect("run unprivileged selector test");
    assert!(
        output.status.success(),
        "read-only selector child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
