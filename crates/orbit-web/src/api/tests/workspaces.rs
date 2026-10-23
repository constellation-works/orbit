use super::test_support::{assert_isolated_child, enter_isolated_child};
use crate::state::{DashboardState, RegistrySource};
use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, OrbitRuntime, TaskStatus};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

/// Create an on-disk workspace under `base/<name>`, seed one in-progress task,
/// and return `(orbit_dir, repo_root)`. The workspace persists after the
/// runtime is dropped, so global mode can reopen it via `from_roots`.
fn seed_workspace(global_root: &Path, base: &Path, name: &str) -> (PathBuf, PathBuf) {
    assert_isolated_child();
    let repo_root = base.join(name);
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create .orbit");
    std::fs::write(orbit_dir.join("config.toml"), "").expect("write config");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: ws_{name}\n"),
    )
    .expect("write workspace identity");
    let runtime = OrbitRuntime::from_roots(global_root, &orbit_dir)
        .expect("build runtime")
        .with_actor(ActorIdentity::human("human"));
    runtime
        .add_task(TaskAddParams {
            title: format!("{name} task"),
            description: "seed".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("add task");
    (orbit_dir, repo_root)
}

/// Write a registry file at `<global_root>/workspaces.json` binding each
/// `(id, repo_root)` as an active, owner-role workspace.
fn write_registry(global_root: &Path, workspaces: &[(&str, &Path)]) {
    let workspaces: Vec<_> = workspaces
        .iter()
        .map(|(id, repo_root)| (*id, *repo_root, None))
        .collect();
    write_registry_with_ship_modes(global_root, &workspaces);
}

fn write_registry_with_ship_modes(global_root: &Path, workspaces: &[(&str, &Path, Option<&str>)]) {
    let now = Utc::now();
    let mut registry = WorkspaceRegistry::default();
    for (id, repo_root, ship_mode) in workspaces {
        registry.workspaces.push(Workspace {
            id: (*id).to_string(),
            name: (*id).to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: ship_mode.map(str::to_string),
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        });
        registry.checkouts.push(WorkspaceCheckout::owner(
            (*id).to_string(),
            repo_root.to_path_buf(),
            repo_root.join(".orbit"),
        ));
    }
    orbit_registry::workspace_registry::save_registry_to(
        &registry,
        &global_root.join("workspaces.json"),
    )
    .expect("save registry");
}

/// A registry-backed state reloading from `<global_root>/workspaces.json`.
fn registry_state(global_root: &Path) -> DashboardState {
    assert_isolated_child();
    let source = RegistrySource::new(global_root.join("workspaces.json"), None, None);
    DashboardState::from_registry(global_root.to_path_buf(), source).expect("from_registry")
}

/// Metadata selection used by the aggregate exposes a runtime's binding.
fn runtime_task_titles(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .task_candidates(&Default::default(), orbit_core::DEFAULT_TASK_LIST_LIMIT)
        .expect("list candidates")
        .items
        .into_iter()
        .map(|task| task.title)
        .collect()
}

/// Barrier-controlled rebind-during-build: a runtime built against the old
/// binding, paused mid-flight while the registry is rebound and refreshed, must
/// NOT republish as current when it finally reaches the cache. The new-binding
/// runtime stays authoritative, the old build is returned only to its own
/// request, and `open_runtimes` (which every aggregate/health response and
/// `/api/tasks/all` derives from) surfaces exactly the new checkout — never the
/// stale one. Covers finding P1 (stale runtime publication) deterministically.
#[test]
fn stale_build_during_rebind_never_republishes_as_current() {
    if !enter_isolated_child(
        module_path!(),
        "stale_build_during_rebind_never_republishes_as_current",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (_alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    write_registry(&global_root, &[("alpha", &alpha_repo)]);
    let state = registry_state(&global_root);

    // The racing thread performs the *first* build, so leave the cache cold.
    let paused = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let fired = Arc::new(AtomicBool::new(false));
    {
        let paused = paused.clone();
        let release = release.clone();
        let fired = fired.clone();
        state.set_pre_publish_hook(Arc::new(move |_id: &str| {
            // Only the first build (the racing old-binding build) pauses; the
            // main thread's later new-binding build passes straight through.
            if !fired.swap(true, Ordering::SeqCst) {
                paused.wait();
                release.wait();
            }
        }));
    }

    let new_runtime = std::thread::scope(|scope| {
        let racer = {
            let state = &state;
            scope.spawn(move || state.runtime_for("alpha").expect("old-binding build"))
        };

        // Wait until the racer has built the old-binding runtime and parked
        // itself just before publication.
        paused.wait();

        // Rebind alpha to a fresh checkout and refresh: a new generation, and
        // the cold cache means nothing is evicted.
        let (_v2_orbit, v2_repo) = seed_workspace(&global_root, tmp.path(), "alpha_v2");
        write_registry(&global_root, &[("alpha", &v2_repo)]);
        state.refresh();

        // Resolve the new binding — builds and publishes the new-generation
        // runtime while the racer is still parked.
        let new_runtime = state.runtime_for("alpha").expect("new-binding build");

        // Release the racer; it now attempts to publish its stale old build.
        release.wait();
        let old_runtime = racer.join().expect("join racer");

        assert!(
            !Arc::ptr_eq(&old_runtime, &new_runtime),
            "old build is a distinct runtime, returned only to its own request"
        );
        // The stale build must not have overwritten the new cache entry.
        let current = state.runtime_for("alpha").expect("current");
        assert!(
            Arc::ptr_eq(&current, &new_runtime),
            "stale old-generation build must never republish as current"
        );
        // The old build serves the old checkout; current serves the new one.
        assert!(
            runtime_task_titles(&old_runtime).contains(&"alpha task".to_string()),
            "old build binds the original checkout"
        );
        assert!(
            runtime_task_titles(&current).contains(&"alpha_v2 task".to_string()),
            "current binds the rebound checkout"
        );
        new_runtime
    });

    // open_runtimes joins by exact binding, so the stale runtime is filtered
    // out entirely: exactly one alpha runtime, and it is the new checkout's.
    let open = state.open_runtimes();
    let alpha_open: Vec<_> = open.iter().filter(|(id, _)| id == "alpha").collect();
    assert_eq!(alpha_open.len(), 1, "one coherent alpha runtime open");
    assert!(
        Arc::ptr_eq(&alpha_open[0].1, &new_runtime),
        "open_runtimes surfaces only the current-binding runtime"
    );
    assert!(
        runtime_task_titles(&alpha_open[0].1).contains(&"alpha_v2 task".to_string()),
        "the open runtime is never tagged as the wrong (old) checkout"
    );
}
