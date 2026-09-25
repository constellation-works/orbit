use super::*;

fn colliding_id_name_fixture() -> CollidingIdNameFixture {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_collision\"\nname = \"collision\"\ntask_prefix = \"ORB\"\n",
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

    let from_cwd = select_for_cwd_and_roots(&fixture.alpha_repo, &roots)
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
        global.join("config.toml"),
        "[machine]\nid = \"hm_deleted_test\"\nname = \"deleted-test\"\ntask_prefix = \"ORB\"\n",
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
        global.join("config.toml"),
        "[machine]\nid = \"hm_readable_test\"\nname = \"readable-test\"\ntask_prefix = \"ORB\"\n",
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

struct PathSelectorFixture {
    _root: tempfile::TempDir,
    global: PathBuf,
    primary_repo: PathBuf,
    primary_orbit: PathBuf,
    linked: PathBuf,
    primary_subdir: PathBuf,
    linked_subdir: PathBuf,
}

fn path_selector_fixture() -> PathSelectorFixture {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_path_sel\"\nname = \"path-sel\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (ws_primary, checkout_primary) =
        registered_workspace(root.path(), "ws_primary", "primary", "hm_path_sel");
    init_git_repo(&checkout_primary.repo_root);
    let primary_repo = checkout_primary.repo_root.clone();
    let primary_orbit = checkout_primary.orbit_dir.clone();

    let mut workspaces = vec![ws_primary];
    let mut checkouts = vec![checkout_primary];
    for (id, name) in [
        ("ws_decoy_a", "decoy-a"),
        ("ws_decoy_b", "decoy-b"),
        ("ws_decoy_c", "decoy-c"),
    ] {
        let (workspace, checkout) = registered_workspace(root.path(), id, name, "hm_path_sel");
        init_git_repo(&checkout.repo_root);
        workspaces.push(workspace);
        checkouts.push(checkout);
    }
    save_registry_to(
        &WorkspaceRegistry {
            workspaces,
            checkouts,
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let linked = root.path().join("linked");
    add_linked_worktree(&primary_repo, &linked);
    let primary_subdir = primary_repo.join("packages/demo");
    std::fs::create_dir_all(&primary_subdir).expect("primary subdirectory");
    let linked_subdir = linked.join("src");
    std::fs::create_dir_all(&linked_subdir).expect("linked subdirectory");

    PathSelectorFixture {
        _root: root,
        global,
        primary_repo,
        primary_orbit,
        linked,
        primary_subdir,
        linked_subdir,
    }
}

fn resolve_path_selector(global: &Path, path: &Path) -> ResolvedWorkspaceSelection {
    let selector = path
        .to_str()
        .unwrap_or_else(|| panic!("utf8 path {}", path.display()));
    RegisteredRuntimeFactory::resolve_workspace_selector(global, selector)
        .unwrap_or_else(|error| panic!("resolve {}: {error}", path.display()))
}

fn assert_primary_selection(
    selected: &ResolvedWorkspaceSelection,
    fixture: &PathSelectorFixture,
    expected_local_root: &Path,
) {
    assert_eq!(selected.workspace.id, "ws_primary");
    assert_eq!(
        canonical_test_path(&selected.checkout.repo_root),
        canonical_test_path(&fixture.primary_repo)
    );
    assert_eq!(
        canonical_test_path(&selected.local_root),
        canonical_test_path(expected_local_root)
    );
}

fn canonical_test_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[test]
fn workspace_selector_path_spawns_at_most_one_git_process_for_linked_worktree() {
    let fixture = path_selector_fixture();
    let probes = GitProcessProbes::capture();
    let selected = resolve_path_selector(&fixture.global, &fixture.linked);
    assert_primary_selection(
        &selected,
        &fixture,
        &canonical_test_path(&fixture.linked).join(".orbit"),
    );
    assert!(
        probes.git_process_spawns() <= 1,
        "linked worktree path must not spawn git per registered checkout, got {}",
        probes.git_process_spawns()
    );
}

#[test]
fn workspace_selector_path_spawns_at_most_one_git_process_for_subdirectory() {
    let fixture = path_selector_fixture();
    let probes = GitProcessProbes::capture();
    let selected = resolve_path_selector(&fixture.global, &fixture.primary_subdir);
    assert_primary_selection(&selected, &fixture, &fixture.primary_orbit);
    assert!(
        probes.git_process_spawns() <= 1,
        "subdirectory path must not spawn git per registered checkout, got {}",
        probes.git_process_spawns()
    );
}

#[test]
fn workspace_selector_path_resolves_linked_worktree_subdirectory_with_one_git_process() {
    let fixture = path_selector_fixture();
    let probes = GitProcessProbes::capture();
    let selected = resolve_path_selector(&fixture.global, &fixture.linked_subdir);
    assert_primary_selection(
        &selected,
        &fixture,
        &canonical_test_path(&fixture.linked).join(".orbit"),
    );
    assert!(
        probes.git_process_spawns() <= 1,
        "linked-worktree subdirectory must not spawn git per registered checkout, got {}",
        probes.git_process_spawns()
    );
}

#[test]
fn exact_checkout_path_selector_does_not_spawn_git() {
    let fixture = path_selector_fixture();
    let probes = GitProcessProbes::capture();
    let selected = resolve_path_selector(&fixture.global, &fixture.primary_repo);
    assert_primary_selection(&selected, &fixture, &fixture.primary_orbit);
    assert_eq!(
        probes.git_process_spawns(),
        0,
        "an exact registered repo_root match must not spawn git"
    );
}

#[test]
fn path_selector_falls_back_to_git_spawn_when_recorded_git_dir_is_missing() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_path_fallback\"\nname = \"path-fallback\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let (workspace, mut checkout) =
        registered_workspace(root.path(), "ws_fallback", "fallback", "hm_path_fallback");
    init_git_repo(&checkout.repo_root);
    checkout.orbit_dir = global.join("external-orbit");
    std::fs::create_dir_all(&checkout.orbit_dir).expect("external orbit dir");
    save_registry_to(
        &WorkspaceRegistry {
            workspaces: vec![workspace],
            checkouts: vec![checkout.clone()],
            ..Default::default()
        },
        &registry_path_for(&global),
    )
    .expect("workspace registry");

    let subdir = checkout.repo_root.join("src");
    std::fs::create_dir_all(&subdir).expect("subdirectory");
    let probes = GitProcessProbes::capture();
    let selected = resolve_path_selector(&global, &subdir);
    assert_eq!(selected.workspace.id, "ws_fallback");
    assert!(
        probes.git_process_spawns() >= 2,
        "zero recorded .git hits must fall back to spawning git for the selected path and checkout, got {}",
        probes.git_process_spawns()
    );
}

fn init_git_repo(repo: &Path) {
    std::fs::create_dir_all(repo).expect("repo dir");
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "# repo\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "initial"]);
}

fn add_linked_worktree(primary: &Path, linked: &Path) {
    run_git(
        primary,
        &[
            "worktree",
            "add",
            linked.to_str().expect("utf8 linked worktree path"),
            "HEAD",
        ],
    );
}

struct CurrentDirGuard {
    _lock: MutexGuard<'static, ()>,
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::current_dir().expect("capture cwd");
        std::env::set_current_dir(path)
            .unwrap_or_else(|error| panic!("enter {}: {error}", path.display()));
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

fn add_probe_definition(runtime: &OrbitRuntime, name: &str, description: &str) {
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.to_string(),
            description: description.to_string(),
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
            template: AutoTaskTemplate {
                title: description.to_string(),
                description: String::new(),
                acceptance_criteria: vec![],
                task_type: TaskType::Chore,
                tags: vec![],
                required_tools: vec![],
                priority: TaskPriority::Medium,
                complexity: None,
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap_or_else(|error| panic!("auto-task add {name}: {error}"));
}

#[test]
fn logical_selector_from_linked_worktree_writes_auto_task_yaml_to_worktree_local_root() {
    let fixture = path_selector_fixture();
    let linked = canonical_test_path(&fixture.linked);
    let linked_orbit = linked.join(".orbit");
    let primary_definition = fixture.primary_orbit.join("auto_tasks/worktree-write.yaml");
    let worktree_definition = linked_orbit.join("auto_tasks/worktree-write.yaml");

    let _cwd = CurrentDirGuard::enter(&linked);
    let runtime =
        RegisteredRuntimeFactory::initialize_with_overrides(Some(&fixture.global), Some("primary"))
            .expect("logical selector from a linked worktree must open");
    assert_eq!(
        canonical_test_path(&runtime.shared_root()),
        canonical_test_path(&fixture.primary_orbit),
        "shared_root stays the registered primary store"
    );
    assert_eq!(
        canonical_test_path(&runtime.local_root()),
        linked_orbit,
        "local_root is the linked worktree .orbit"
    );

    add_probe_definition(
        &runtime,
        "worktree-write",
        "Written from the linked worktree",
    );
    assert!(
        worktree_definition.is_file(),
        "definition YAML must land under the worktree local_root: {}",
        worktree_definition.display()
    );
    assert!(
        !primary_definition.exists(),
        "definition YAML must not land in the registered primary checkout"
    );
    assert!(
        !linked_orbit.join("tasks").exists(),
        "must not invent a worktree-local task store"
    );

    let shown = runtime
        .auto_task_show("worktree-write")
        .expect("show worktree definition")
        .expect("definition exists on the worktree local_root");
    assert_eq!(shown.description, "Written from the linked worktree");

    runtime
        .auto_task_toggle("worktree-write", false)
        .expect("toggle on the worktree local_root");
    let yaml = std::fs::read_to_string(&worktree_definition).expect("read worktree yaml");
    assert!(
        yaml.contains("enabled: false"),
        "toggle must rewrite the worktree YAML: {yaml}"
    );
    assert!(
        !primary_definition.exists(),
        "toggle must not create primary YAML"
    );
}

#[test]
fn logical_selector_from_unrelated_cwd_keeps_registered_primary_local_root() {
    let fixture = path_selector_fixture();
    let elsewhere = fixture._root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("unrelated cwd");

    let _cwd = CurrentDirGuard::enter(&elsewhere);
    let runtime = RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.global),
        Some("ws_primary"),
    )
    .expect("logical selector from an unrelated cwd must open the primary");
    assert_eq!(
        canonical_test_path(&runtime.shared_root()),
        canonical_test_path(&fixture.primary_orbit)
    );
    assert_eq!(
        canonical_test_path(&runtime.local_root()),
        canonical_test_path(&fixture.primary_orbit),
        "a logical selector from a cwd that is not a linked worktree of that workspace opens the primary"
    );

    add_probe_definition(&runtime, "primary-write", "Written from an unrelated cwd");
    assert!(
        fixture
            .primary_orbit
            .join("auto_tasks/primary-write.yaml")
            .is_file()
    );
    assert!(
        !canonical_test_path(&fixture.linked)
            .join(".orbit/auto_tasks/primary-write.yaml")
            .exists(),
        "unrelated cwd must not write worktree YAML"
    );
}

#[test]
fn linked_worktree_root_override_is_not_a_store_and_invalid_selectors_fail_closed() {
    let fixture = path_selector_fixture();
    let linked = canonical_test_path(&fixture.linked);
    let worktree_orbit = linked.join(".orbit");
    std::fs::create_dir_all(&worktree_orbit).expect("worktree local orbit");

    let unknown = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.global),
        Some("no-such-workspace"),
    ) {
        Ok(_) => panic!("unknown selector must fail closed"),
        Err(error) => error,
    };
    unsupported_workspace_message(unknown, "no-such-workspace");

    let outside = fixture._root.path().to_string_lossy().into_owned();
    let unrelated = match RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.global),
        Some(&outside),
    ) {
        Ok(_) => panic!("unrelated path selector must fail closed"),
        Err(error) => error,
    };
    unsupported_workspace_message(unrelated, &outside);

    let shadowed =
        RegisteredRuntimeFactory::initialize_with_overrides(Some(&worktree_orbit), Some("primary"));
    match shadowed {
        Ok(_) => panic!("--root pointed at a worktree .orbit must not open as a store"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("unknown workspace selector")
                    || message.contains("not an Orbit workspace")
                    || message.contains("workspaces.json")
                    || message.contains("[machine]"),
                "worktree .orbit as --root must be refused as a store shadow: {message}"
            );
        }
    }
}

#[test]
fn read_only_linked_path_selector_from_primary_keeps_candidate_local_root() {
    let fixture = path_selector_fixture();
    let linked = canonical_test_path(&fixture.linked);
    let linked_orbit = linked.join(".orbit");
    let linked_selector = linked.to_string_lossy().into_owned();

    let _cwd = CurrentDirGuard::enter(&fixture.primary_repo);
    let read_only = RegisteredRuntimeFactory::initialize_read_only_with_overrides(
        Some(&fixture.global),
        Some(&linked_selector),
    )
    .expect("read-only linked path selector");
    assert_eq!(
        canonical_test_path(&read_only.shared_root()),
        canonical_test_path(&fixture.primary_orbit)
    );
    assert_eq!(
        canonical_test_path(&read_only.local_root()),
        linked_orbit,
        "read-only show/list may open a linked candidate root"
    );

    let writable = RegisteredRuntimeFactory::initialize_with_overrides(
        Some(&fixture.global),
        Some(&linked_selector),
    )
    .expect("writable linked path selector from the primary");
    assert_eq!(
        canonical_test_path(&writable.local_root()),
        canonical_test_path(&fixture.primary_orbit),
        "writable explicit-linked-selector from another cwd must not retarget mutations"
    );
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
