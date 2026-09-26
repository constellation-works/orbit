//! Sibling tests for `resolve.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use std::fs;
use std::path::Path;

use tempfile::{tempdir, tempdir_in};

use orbit_common::{OrbitError, test_env};

use super::super::resolve::{
    ResolvedOrbitRoots, WorkspaceRootHint, resolve_bootstrap_roots, resolve_generation_root,
    resolve_initialize_roots, resolve_initialize_roots_with_hint, try_resolve_initialized_roots,
    try_resolve_initialized_roots_with_hint,
};

#[test]
fn caller_supplied_workspace_hint_keeps_registry_out_of_core_resolution() {
    let home = tempdir().expect("home tempdir");
    let cwd = tempdir().expect("cwd tempdir");
    let hinted = tempdir().expect("hinted tempdir");
    let hinted_orbit = hinted.path().join(".orbit");
    seed_initialized_workspace_root(&hinted_orbit);
    let home_var = home.path().to_string_lossy().into_owned();
    // `ORBIT_ROOT` outranks the caller hint, and a managed agent run exports
    // one, so this case only tests hint precedence with the env root cleared.
    let _env = test_env::scoped([("ORBIT_ROOT", None), ("HOME", Some(home_var.as_str()))]);

    let roots = resolve_initialize_roots_with_hint(
        cwd.path(),
        None,
        Some(&WorkspaceRootHint {
            orbit_dir: hinted_orbit.clone(),
        }),
    )
    .expect("resolve caller hint");

    assert_pinned_roots(&roots, &hinted_orbit);
}

#[test]
fn explicit_root_with_initialized_child_orbit_resolves_to_child() {
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&orbit_root);

    let resolved = resolve_initialize_roots(repo.path(), Some(repo.path())).expect("resolve root");

    assert_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn explicit_root_prefers_initialized_child_orbit_over_polluted_repo_root() {
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&orbit_root);
    fs::write(repo.path().join("config.toml"), "polluted = true\n").expect("write root pollution");

    let resolved = resolve_initialize_roots(repo.path(), Some(repo.path())).expect("resolve root");

    assert_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn explicit_root_with_uninitialized_directory_returns_invalid_input_without_layout() {
    let parent = tempdir().expect("parent tempdir");
    let root = parent.path().join("not-an-orbit-root");
    fs::create_dir_all(&root).expect("create uninitialized root");

    let err = resolve_initialize_roots(parent.path(), Some(&root))
        .expect_err("uninitialized root should fail");

    assert!(matches!(
        err,
        OrbitError::InvalidInput(message) if message.contains("not an Orbit workspace")
    ));
    assert!(!root.join(".orbit").exists());
    assert!(!root.join("resources").exists());
    assert!(!root.join("tasks").exists());
    assert!(!root.join("state").exists());
}

#[test]
fn explicit_root_with_initialized_orbit_root_resolves_as_is() {
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&orbit_root);

    let resolved = resolve_initialize_roots(repo.path(), Some(&orbit_root)).expect("resolve root");

    assert_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn bootstrap_root_allows_uninitialized_path_without_creating_it() {
    let parent = tempdir().expect("parent tempdir");
    let root = parent.path().join("new-orbit-root");

    let resolved = resolve_bootstrap_roots(parent.path(), Some(&root)).expect("resolve root");

    assert_pinned_roots(&resolved, &root);
    assert!(!root.exists());
}

#[test]
fn explicit_root_precedes_env_and_worktree_resolution() {
    let main_repo = tempdir().expect("main repo tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let explicit_repo = tempdir().expect("explicit repo tempdir");
    let env_repo = tempdir().expect("env repo tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());
    seed_initialized_workspace_root(&main_repo.path().join(".orbit"));
    seed_initialized_workspace_root(&explicit_repo.path().join(".orbit"));
    seed_initialized_workspace_root(&env_repo.path().join(".orbit"));
    let env_var = env_repo.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("ORBIT_ROOT", Some(env_var.as_str()))]);

    let resolved = resolve_initialize_roots(worktree.path(), Some(explicit_repo.path()))
        .expect("resolve explicit root");

    assert_pinned_roots(&resolved, &explicit_repo.path().join(".orbit"));
}

#[test]
fn env_root_precedes_worktree_resolution() {
    let main_repo = tempdir().expect("main repo tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let env_repo = tempdir().expect("env repo tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());
    seed_initialized_workspace_root(&main_repo.path().join(".orbit"));
    seed_initialized_workspace_root(&env_repo.path().join(".orbit"));
    let env_var = env_repo.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("ORBIT_ROOT", Some(env_var.as_str()))]);

    let resolved = resolve_initialize_roots(worktree.path(), None).expect("resolve env root");

    assert_pinned_roots(&resolved, &env_repo.path().join(".orbit"));
}

#[test]
fn worktree_main_orbit_precedes_worktree_local_orbit() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let main_repo = tempdir().expect("main repo tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());
    let main_orbit = main_repo.path().join(".orbit");
    let worktree_orbit = worktree.path().join(".orbit");
    seed_initialized_workspace_root(&main_orbit);
    seed_initialized_workspace_root(&worktree_orbit);

    let resolved = resolve_initialize_roots(worktree.path(), None).expect("resolve worktree root");

    assert_roots(&resolved, &main_orbit, &worktree_orbit);
}

#[test]
fn worktree_without_orbit_uses_main_repo_legacy_orbit_path() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let main_repo = tempdir().expect("main repo tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());

    let resolved = resolve_bootstrap_roots(worktree.path(), None).expect("resolve worktree root");

    assert_roots(
        &resolved,
        &main_repo.path().join(".orbit"),
        &worktree.path().join(".orbit"),
    );
    assert!(!resolved.shared_root.exists());
    assert!(!worktree.path().join(".orbit").exists());
}

#[test]
fn non_worktree_walk_up_behavior_is_preserved() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let repo = tempdir().expect("repo tempdir");
    let nested = repo.path().join("a").join("b");
    fs::create_dir_all(&nested).expect("create nested dir");
    let orbit_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&orbit_root);

    let resolved = resolve_initialize_roots(&nested, None).expect("resolve walk-up root");

    assert_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn bootstrap_rejects_home_when_cwd_is_home_with_global_orbit_and_no_git() {
    let home = tempdir().expect("home tempdir");
    let global_orbit = home.path().join(".orbit");
    seed_initialized_workspace_root(&global_orbit);
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("HOME", Some(home_var.as_str())), ("ORBIT_ROOT", None)]);

    let err = resolve_bootstrap_roots(home.path(), None)
        .expect_err("bootstrap should refuse to adopt the global root as a workspace");

    assert!(matches!(
        err,
        OrbitError::InvalidInput(message) if message.contains("global Orbit root")
    ));
}

#[test]
fn bootstrap_rejects_home_when_home_itself_is_a_git_repo() {
    let home = tempdir().expect("home tempdir");
    fs::create_dir_all(home.path().join(".git")).expect("seed home as git repo");
    let global_orbit = home.path().join(".orbit");
    seed_initialized_workspace_root(&global_orbit);
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("HOME", Some(home_var.as_str())), ("ORBIT_ROOT", None)]);

    let err = resolve_bootstrap_roots(home.path(), None)
        .expect_err("bootstrap should refuse $HOME/.orbit via git_repo_root + cwd_fallback");

    assert!(matches!(
        err,
        OrbitError::InvalidInput(message) if message.contains("global Orbit root")
    ));
}

#[test]
fn bootstrap_ignores_home_global_orbit_when_repo_has_no_workspace_orbit() {
    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("work").join("repo");
    fs::create_dir_all(repo.join(".git")).expect("create repo git dir");
    let global_orbit = home.path().join(".orbit");
    seed_initialized_workspace_root(&global_orbit);
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("HOME", Some(home_var.as_str())), ("ORBIT_ROOT", None)]);

    let resolved = resolve_bootstrap_roots(&repo, None).expect("resolve bootstrap root");

    assert_pinned_roots(&resolved, &repo.join(".orbit"));
    assert_ne!(resolved.shared_root, global_orbit);
}

#[test]
fn try_resolve_returns_none_outside_orbit_workspace() {
    let home = tempdir().expect("home tempdir");
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("ORBIT_ROOT", None), ("HOME", Some(home_var.as_str()))]);
    let nowhere = tempdir_in(home.path()).expect("nowhere tempdir");

    let resolved = try_resolve_initialized_roots(nowhere.path(), None)
        .expect("try_resolve completes without error");

    assert!(resolved.is_none(), "unexpected roots: {resolved:?}");
    assert!(!nowhere.path().join(".orbit").exists());
}

#[test]
fn try_resolve_finds_initialized_workspace_via_walk_up() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let repo = tempdir().expect("repo tempdir");
    let nested = repo.path().join("a").join("b");
    fs::create_dir_all(&nested).expect("create nested dir");
    let orbit_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&orbit_root);

    let resolved =
        try_resolve_initialized_roots(&nested, None).expect("try_resolve completes without error");

    assert_optional_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn try_resolve_finds_main_worktree_orbit_for_linked_worktree() {
    let home = tempdir().expect("home tempdir");
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("ORBIT_ROOT", None), ("HOME", Some(home_var.as_str()))]);
    let main_repo = tempdir_in(home.path()).expect("main repo tempdir");
    let worktree = tempdir_in(home.path()).expect("worktree tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());
    let main_orbit = main_repo.path().join(".orbit");
    seed_initialized_workspace_root(&main_orbit);

    let resolved = try_resolve_initialized_roots(worktree.path(), None)
        .expect("try_resolve completes without error");

    assert_optional_roots(&resolved, &main_orbit, &worktree.path().join(".orbit"));
}

#[test]
fn try_resolve_returns_none_when_main_worktree_orbit_is_uninitialized() {
    let home = tempdir().expect("home tempdir");
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("ORBIT_ROOT", None), ("HOME", Some(home_var.as_str()))]);
    let main_repo = tempdir_in(home.path()).expect("main repo tempdir");
    let worktree = tempdir_in(home.path()).expect("worktree tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());
    // No `.orbit/` exists at all — main worktree resolution finds the
    // path but it's uninitialized, so try_resolve falls through.

    let resolved = try_resolve_initialized_roots(worktree.path(), None)
        .expect("try_resolve completes without error");

    assert!(resolved.is_none(), "unexpected roots: {resolved:?}");
    assert!(!main_repo.path().join(".orbit").exists());
    assert!(!worktree.path().join(".orbit").exists());
}

#[test]
fn try_resolve_honors_initialized_root_override() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&orbit_root);
    let elsewhere = tempdir().expect("elsewhere tempdir");

    let resolved = try_resolve_initialized_roots(elsewhere.path(), Some(repo.path()))
        .expect("try_resolve completes without error");

    assert_optional_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn generation_root_prefers_explicit_root_over_orbit_root_and_home() {
    let home = tempdir().expect("home tempdir");
    let env_root = tempdir().expect("env root");
    let explicit = tempdir().expect("explicit root");
    let home_var = home.path().to_string_lossy().into_owned();
    let env_var = env_root.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", Some(env_var.as_str())),
        ("ORBIT_REGISTRY_ROOT", None),
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_RUN_ID", None),
    ]);

    let resolved =
        resolve_generation_root(Some(explicit.path())).expect("explicit generation root");
    assert_eq!(resolved, explicit.path());
}

#[test]
fn generation_root_uses_orbit_root_when_no_flag_is_supplied() {
    let home = tempdir().expect("home tempdir");
    let env_root = tempdir().expect("env root");
    let home_var = home.path().to_string_lossy().into_owned();
    let env_var = env_root.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", Some(env_var.as_str())),
        ("ORBIT_REGISTRY_ROOT", None),
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_RUN_ID", None),
    ]);

    let resolved = resolve_generation_root(None).expect("env generation root");
    assert_eq!(resolved, env_root.path());
}

#[test]
fn generation_root_falls_back_to_home_orbit_without_overrides() {
    let home = tempdir().expect("home tempdir");
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", None),
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_RUN_ID", None),
    ]);

    let resolved = resolve_generation_root(None).expect("home generation root");
    assert_eq!(resolved, home.path().join(".orbit"));
}

#[test]
fn generation_root_uses_managed_registry_when_no_override_is_present() {
    let home = tempdir().expect("home tempdir");
    let registry = tempdir().expect("registry");
    let home_var = home.path().to_string_lossy().into_owned();
    let registry_var = registry.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", Some(registry_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-generation-root")),
    ]);

    let resolved = resolve_generation_root(None).expect("managed generation root");
    assert_eq!(resolved, registry.path());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_sandbox_managed_process_pin_uses_registry_even_with_orbit_root_env() {
    let home = tempdir().expect("home tempdir");
    let registry = tempdir().expect("registry");
    let workspace = tempdir().expect("workspace root");
    let explicit = tempdir().expect("explicit root");
    let home_var = home.path().to_string_lossy().into_owned();
    let registry_var = registry.path().to_string_lossy().into_owned();
    let workspace_var = workspace.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", Some(workspace_var.as_str())),
        ("ORBIT_REGISTRY_ROOT", Some(registry_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-generation-root")),
    ]);

    assert_eq!(
        super::super::resolve::resolve_process_generation_root(None)
            .expect("managed process generation root"),
        registry.path(),
        "managed child must join the host pin even when ORBIT_ROOT selects workspace data"
    );
    assert_eq!(
        resolve_generation_root(None).expect("update generation root"),
        workspace.path(),
        "update admission must still check the ORBIT_ROOT authority"
    );
    assert_eq!(
        super::super::resolve::resolve_process_generation_root(Some(explicit.path()))
            .expect("explicit process generation root"),
        explicit.path(),
        "an explicit CLI root must retain its generation authority"
    );
}

#[test]
fn inspection_invocation_uses_the_managed_registry_without_a_job_run() {
    let home = tempdir().expect("home tempdir");
    let registry = tempdir().expect("registry");
    let home_var = home.path().to_string_lossy().into_owned();
    let registry_var = registry.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", Some(registry_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", None),
        ("ORBIT_SESSION_ID", Some("inspection-invocation")),
    ]);

    assert_eq!(
        resolve_generation_root(None).expect("inspection registry"),
        registry.path()
    );
}

#[test]
fn try_resolve_rejects_uninitialized_root_override() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let parent = tempdir().expect("parent tempdir");
    let bogus = parent.path().join("not-an-orbit-root");
    fs::create_dir_all(&bogus).expect("create bogus dir");

    let err = try_resolve_initialized_roots(parent.path(), Some(&bogus))
        .expect_err("uninitialized override should error");

    assert!(matches!(
        err,
        OrbitError::InvalidInput(message) if message.contains("not an Orbit workspace")
    ));
    assert!(!bogus.join(".orbit").exists());
}

#[test]
fn config_yaml_only_workspace_resolves_via_walk_up() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let repo = tempdir().expect("repo tempdir");
    let nested = repo.path().join("nested");
    fs::create_dir(&nested).expect("create nested dir");
    let orbit_root = repo.path().join(".orbit");
    seed_identity_only_workspace_root(&orbit_root);

    let resolved = try_resolve_initialized_roots(&nested, None)
        .expect("config.yaml should initialize the workspace via walk-up");

    assert_optional_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn config_yaml_only_main_worktree_resolves_from_linked_worktree() {
    let home = tempdir().expect("home tempdir");
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("ORBIT_ROOT", None), ("HOME", Some(home_var.as_str()))]);
    let main_repo = tempdir_in(home.path()).expect("main repo tempdir");
    let worktree = tempdir_in(home.path()).expect("worktree tempdir");
    seed_fake_git_worktree(main_repo.path(), worktree.path());
    let main_orbit = main_repo.path().join(".orbit");
    seed_identity_only_workspace_root(&main_orbit);

    let resolved = try_resolve_initialized_roots(worktree.path(), None)
        .expect("config.yaml should initialize the main worktree");

    assert_optional_roots(&resolved, &main_orbit, &worktree.path().join(".orbit"));
}

#[test]
fn config_yaml_only_workspace_resolves_from_explicit_root_and_env() {
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    seed_identity_only_workspace_root(&orbit_root);
    let elsewhere = tempdir().expect("elsewhere tempdir");
    let _env = test_env::unset(["ORBIT_ROOT"]);

    let resolved = try_resolve_initialized_roots(elsewhere.path(), Some(repo.path()))
        .expect("repo root should resolve its config.yaml-only child");
    assert_optional_pinned_roots(&resolved, &orbit_root);

    let resolved = resolve_initialize_roots(elsewhere.path(), Some(&orbit_root))
        .expect("explicit .orbit root should resolve");
    assert_pinned_roots(&resolved, &orbit_root);

    drop(_env);
    let env_root = repo.path().to_string_lossy().into_owned();
    let _env_root = test_env::scoped([("ORBIT_ROOT", Some(env_root.as_str()))]);
    let resolved = try_resolve_initialized_roots(elsewhere.path(), None)
        .expect("ORBIT_ROOT should resolve the config.yaml-only workspace");
    assert_optional_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn config_yaml_only_workspace_resolves_from_registry_hint() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    seed_identity_only_workspace_root(&orbit_root);
    let elsewhere = tempdir().expect("elsewhere tempdir");

    let resolved = try_resolve_initialized_roots_with_hint(
        elsewhere.path(),
        None,
        Some(&WorkspaceRootHint {
            orbit_dir: orbit_root.clone(),
        }),
    )
    .expect("hint should resolve the config.yaml-only workspace");

    assert_optional_pinned_roots(&resolved, &orbit_root);
}

#[test]
fn bare_orbit_directory_is_not_an_initialized_workspace() {
    let _env = test_env::unset(["ORBIT_ROOT"]);
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    fs::create_dir(&orbit_root).expect("create bare .orbit directory");

    let resolved = try_resolve_initialized_roots(repo.path(), None)
        .expect("bare .orbit should not raise a resolution error");
    assert!(resolved.is_none(), "bare .orbit was accepted: {resolved:?}");

    let error = try_resolve_initialized_roots(repo.path(), Some(repo.path()))
        .expect_err("explicit bare .orbit should be rejected");
    assert!(matches!(
        error,
        OrbitError::InvalidInput(message) if message.contains("not an Orbit workspace")
    ));
}

fn seed_initialized_workspace_root(path: &Path) {
    fs::create_dir_all(path.join("resources")).expect("create resources");
    fs::create_dir_all(path.join("tasks")).expect("create tasks");
    fs::create_dir_all(path.join("state")).expect("create state");
}

fn seed_identity_only_workspace_root(path: &Path) {
    fs::create_dir_all(path.join("state")).expect("create state");
    fs::write(
        path.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_identity_only\n",
    )
    .expect("write workspace identity");
    assert!(!path.join("config.toml").exists());
    assert!(!path.join("resources").exists());
}

fn assert_pinned_roots(roots: &ResolvedOrbitRoots, root: &Path) {
    assert_roots(roots, root, root);
}

fn assert_roots(roots: &ResolvedOrbitRoots, shared_root: &Path, local_root: &Path) {
    assert_eq!(roots.shared_root, shared_root);
    assert_eq!(roots.local_root, local_root);
}

fn assert_optional_pinned_roots(roots: &Option<ResolvedOrbitRoots>, root: &Path) {
    assert_optional_roots(roots, root, root);
}

fn assert_optional_roots(
    roots: &Option<ResolvedOrbitRoots>,
    shared_root: &Path,
    local_root: &Path,
) {
    let roots = roots.as_ref().expect("expected resolved roots");
    assert_roots(roots, shared_root, local_root);
}

fn seed_fake_git_worktree(main_repo: &Path, worktree: &Path) {
    let worktree_git_dir = main_repo.join(".git").join("worktrees").join("orbit-test");
    fs::create_dir_all(&worktree_git_dir).expect("create fake worktree git dir");
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .expect("write worktree gitfile");
}
