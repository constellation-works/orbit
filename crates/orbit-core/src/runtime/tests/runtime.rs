//! Sibling tests for `mod.rs` (i.e. the runtime module root; migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use std::path::{Path, PathBuf};

use crate::OrbitRuntime;
use orbit_types::workflow::JobRunState;

use orbit_common::test_env;
use tempfile::tempdir;

use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, global_root, workspace_root)
}

#[test]
fn orbit_root_env_pins_global_registry_root() {
    let home = tempdir().expect("home tempdir");
    let repo = tempdir().expect("repo tempdir");
    let workspace_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&workspace_root);
    let home_var = home.path().to_string_lossy().into_owned();
    let root_var = workspace_root.to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", Some(root_var.as_str())),
    ]);

    let resolved_roots =
        OrbitRuntime::resolve_roots_for_cwd(repo.path(), None).expect("resolve roots");

    // `ORBIT_ROOT` is documented as equivalent to `--root`: both pin the
    // global registry root alongside the shared/local roots, so
    // `ORBIT_ROOT=<dir> orbit workspace list` reads and writes
    // `<dir>/workspaces.json` rather than `$HOME/.orbit` [ORB-10928].
    assert_eq!(resolved_roots.global_root, workspace_root);
    assert_eq!(resolved_roots.shared_root, workspace_root);
    assert_eq!(resolved_roots.local_root, workspace_root);
    assert_ne!(resolved_roots.global_root, home.path().join(".orbit"));
}

#[test]
fn explicit_root_flag_pins_global_registry_root() {
    let home = tempdir().expect("home tempdir");
    let repo = tempdir().expect("repo tempdir");
    let custom_root_parent = tempdir().expect("custom root parent");
    let custom_root = custom_root_parent.path().join("custom-orbit");
    seed_initialized_workspace_root(&custom_root);
    let home_var = home.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([("HOME", Some(home_var.as_str()))]);

    let resolved_roots =
        OrbitRuntime::resolve_roots_for_cwd(repo.path(), Some(custom_root.as_path()))
            .expect("resolve roots with explicit --root");

    // The `--root` flag pins both the shared and global roots to the isolated
    // custom root, so `workspace list`/`show --root <custom>` read
    // `<custom>/workspaces.json` rather than `$HOME/.orbit` [ORB-10218].
    assert_eq!(resolved_roots.global_root, custom_root);
    assert_eq!(resolved_roots.shared_root, custom_root);
    assert_ne!(resolved_roots.global_root, home.path().join(".orbit"));
}

/// [ORB-11066] A managed child needs the host registry even when its HOME is
/// provider-specific, but that locator must not turn the registry into the
/// workspace bootstrap target.
#[test]
fn managed_registry_locator_keeps_workspace_bootstrap_on_the_checkout() {
    let home = tempdir().expect("home tempdir");
    let root = tempdir().expect("fixture root");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    seed_initialized_workspace_root(&workspace_root);
    let home_var = home.path().to_string_lossy().into_owned();
    let global_var = global_root.to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", Some(global_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-managed-registry")),
    ]);

    let resolved =
        OrbitRuntime::resolve_roots_for_cwd(&repo_root, None).expect("resolve managed roots");
    assert_eq!(resolved.global_root, global_root);
    assert_eq!(resolved.shared_root, workspace_root);
    assert_eq!(resolved.local_root, workspace_root);

    let runtime = OrbitRuntime::initialize_from_resolved_roots(resolved, None)
        .expect("initialize managed runtime");
    assert_eq!(runtime.global_root(), global_root);
    assert_eq!(runtime.shared_root(), workspace_root);
    drop(runtime);

    for workspace_only in [
        "state/job-runs",
        "state/diagnostics",
        "state/scoreboard",
        "state/worktrees",
        "knowledge",
    ] {
        assert!(
            !global_root.join(workspace_only).exists(),
            "managed registry bootstrap must not create global workspace-only path {workspace_only}"
        );
    }
}

#[test]
fn unmanaged_registry_locator_is_not_an_operator_root_override() {
    let home = tempdir().expect("home tempdir");
    let repo = tempdir().expect("repo tempdir");
    let workspace_root = repo.path().join(".orbit");
    seed_initialized_workspace_root(&workspace_root);
    let locator = tempdir().expect("untrusted locator");
    let home_var = home.path().to_string_lossy().into_owned();
    let locator_var = locator.path().to_string_lossy().into_owned();
    let _env = test_env::scoped([
        ("HOME", Some(home_var.as_str())),
        ("ORBIT_ROOT", None),
        ("ORBIT_REGISTRY_ROOT", Some(locator_var.as_str())),
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_RUN_ID", None),
    ]);

    let resolved = OrbitRuntime::resolve_roots_for_cwd(repo.path(), None).expect("resolve roots");

    assert_eq!(resolved.global_root, home.path().join(".orbit"));
    assert_eq!(resolved.shared_root, workspace_root);
}

fn seed_initialized_workspace_root(path: &Path) {
    std::fs::create_dir_all(path.join("resources")).expect("create resources dir");
    std::fs::create_dir_all(path.join("tasks")).expect("create tasks dir");
    std::fs::create_dir_all(path.join("state")).expect("create state dir");
}

fn reopened_stale_run_state(managed_context: Option<&str>, run_id: Option<&str>) -> JobRunState {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("managed_context_probe", 1, chrono::Utc::now(), None, None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, chrono::Utc::now(), 999_999)
        .expect("mark run with host-invisible owner");
    drop(runtime);

    let _env = test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", managed_context),
        ("ORBIT_RUN_ID", run_id),
    ]);
    let reopened = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("reopen runtime");
    reopened
        .get_job_run_backend(&run.run_id)
        .expect("read run")
        .expect("run exists")
        .state
}

/// [ORB-10557] A managed sandbox child may not see the host worker PID. The
/// impossible PID below models that private-namespace `process_not_found`
/// shape; workspace open must leave the host-owned run alone, while explicit
/// recovery remains unchanged.
#[test]
fn managed_run_context_skips_workspace_open_reconciliation_but_not_explicit_recovery() {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("managed_context_probe", 1, chrono::Utc::now(), None, None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, chrono::Utc::now(), 999_999)
        .expect("mark run with host-invisible owner");
    drop(runtime);

    let _env = test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("true")),
        ("ORBIT_RUN_ID", Some("jrun-managed-child")),
    ]);
    let reopened = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("reopen runtime");
    let stored = reopened
        .get_job_run_backend(&run.run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(stored.state, JobRunState::Running);

    assert_eq!(
        reopened
            .reconcile_stale_job_runs(None)
            .expect("explicit reconciliation"),
        1
    );
    let reconciled = reopened
        .get_job_run_backend(&run.run_id)
        .expect("read reconciled run")
        .expect("run exists");
    assert_eq!(reconciled.state, JobRunState::Interrupted);
}

#[test]
fn workspace_open_reconciles_without_a_complete_managed_run_context() {
    // No lock here: `reopened_stale_run_state` takes the shared `test_env`
    // guard per iteration, and that mutex is not reentrant.
    for (managed_context, run_id) in [
        (None, Some("jrun-unmanaged")),
        (Some("false"), Some("jrun-false")),
        (Some("not-a-boolean"), Some("jrun-malformed")),
        (Some("true"), None),
        (Some("1"), Some("   ")),
    ] {
        assert_eq!(
            reopened_stale_run_state(managed_context, run_id),
            JobRunState::Interrupted,
            "managed context={managed_context:?}, run id={run_id:?}",
        );
    }
}

fn write_activity(path: &Path, name: &str, description: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: deterministic
  description: {description}
  action: test_action
  config: {{}}
"#
    );
    std::fs::create_dir_all(path.parent().expect("activity path has parent"))
        .expect("create activity dir");
    std::fs::write(path, yaml).expect("write activity yaml");
}

fn write_agent_loop_activity(path: &Path, name: &str, tools: &[&str]) {
    let tools_yaml = tools
        .iter()
        .map(|tool| format!("    - {tool}\n"))
        .collect::<String>();
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  description: Test agent loop.
  instruction: Test.
  tools:
{tools_yaml}"#
    );
    std::fs::create_dir_all(path.parent().expect("activity path has parent"))
        .expect("create activity dir");
    std::fs::write(path, yaml).expect("write activity yaml");
}

#[test]
fn global_default_activity_wins_over_workspace_shadow_in_execution_catalog() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    write_activity(
        &global_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "global description",
    );
    write_activity(
        &workspace_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "workspace description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");
    let activity = catalog.get("pr_open").expect("pr_open activity");
    assert_eq!(activity.description, "global description");
}

#[test]
fn workspace_default_activity_cannot_claim_missing_global_default_name() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_activity(
        &workspace_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "workspace description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(
        catalog.get("pr_open").is_none(),
        "workspace assets must never claim shipped default activity names"
    );
}

#[test]
fn activity_catalog_still_skips_retired_assets() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let activities_dir = workspace_root.join("resources/activities");
    std::fs::create_dir_all(&activities_dir).expect("create activities dir");
    std::fs::write(
        activities_dir.join("retired.yaml"),
        "schemaVersion: 1\nkind: Activity\nmetadata:\n  name: retired\nspec: {}\n",
    )
    .expect("write retired activity");
    write_activity(
        &activities_dir.join("current.yaml"),
        "current",
        "current description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("retired").is_none());
    assert!(catalog.get("current").is_some());
}

#[test]
fn duplicate_activities_within_one_catalog_directory_remain_invalid() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let activities_dir = workspace_root.join("resources/activities");
    write_activity(
        &activities_dir.join("first.yaml"),
        "duplicate_activity",
        "first description",
    );
    write_activity(
        &activities_dir.join("nested/second.yaml"),
        "duplicate_activity",
        "second description",
    );

    let err = runtime
        .v2_activity_catalog()
        .expect_err("duplicate activity name should fail");
    assert!(err.to_string().contains("duplicate activity name"), "{err}");
}

#[test]
fn activity_catalog_accepts_registered_task_wildcard() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/task_tools.yaml"),
        "task_tools",
        &["orbit.task.*"],
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("task_tools").is_some());
}

#[test]
fn activity_catalog_rejects_unknown_concrete_tool() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/unknown_tool.yaml"),
        "unknown_tool",
        &["orbit.task.nope"],
    );

    let err = runtime
        .v2_activity_catalog()
        .expect_err("unknown concrete tool should fail");
    let message = err.to_string();

    assert!(message.contains("unknown_tool"), "{message}");
    assert!(message.contains("orbit.task.nope"), "{message}");
    assert!(message.contains("unknown tool name"), "{message}");
}

#[test]
fn activity_catalog_accepts_intentionally_empty_audit_wildcard() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/audit_tools.yaml"),
        "audit_tools",
        &["orbit.audit.*"],
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("audit_tools").is_some());
}

#[test]
fn default_activity_catalog_allowlists_resolve_registered_tools() {
    let (_root, runtime, global_root, _workspace_root) = test_runtime();
    let activities_dir = global_root.join("resources/activities");
    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let path = activities_dir.join(format!("{name}.yaml"));
        std::fs::create_dir_all(path.parent().expect("activity path has parent"))
            .expect("create activity dir");
        std::fs::write(path, yaml).expect("write activity yaml");
    }

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert_eq!(catalog.len(), DEFAULT_ACTIVITY_FILES.len());
}
