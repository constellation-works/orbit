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

/// The aggregate run list reads one bounded, index-ordered page per
/// workspace and never writes. 20k runs across five workspaces used to cost a
/// full per-workspace sort on every request, plus the stale-run reconciliation
/// that operator reads perform [ORB-14595].
#[test]
fn aggregate_job_runs_page_is_bounded_and_observational() {
    if !enter_isolated_child(
        module_path!(),
        "aggregate_job_runs_page_is_bounded_and_observational",
    ) {
        return;
    }
    use super::super::jobs::JobRunScope;
    use super::super::workspaces::{AllJobRunsState, all_job_runs_json};

    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let names = ["alpha", "bravo", "charlie", "delta", "echo"];
    let repos = names
        .iter()
        .map(|name| seed_workspace(&global_root, tmp.path(), name).1)
        .collect::<Vec<_>>();
    let bindings = names
        .iter()
        .zip(&repos)
        .map(|(name, repo)| (*name, repo.as_path()))
        .collect::<Vec<_>>();
    write_registry(&global_root, &bindings);
    let state = registry_state(&global_root);
    let store = state
        .runtime_for("alpha")
        .expect("alpha runtime")
        .sqlite_store()
        .expect("store");
    {
        let connection = store.connection();
        let conn = connection.lock().expect("store connection");
        for name in names {
            conn.execute(
                "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 3999)
                 INSERT INTO job_runs(run_id, workspace_id, job_id, attempt, state,
                     scheduled_at, started_at, finished_at, duration_ms, created_at)
                 SELECT printf('jrun-%05d', i), ?1, 'bulk', 1, 'success', ts, ts, ts, 0, ts
                 FROM (SELECT i, strftime('%Y-%m-%dT%H:%M:%S+00:00', '2026-01-01',
                     '+' || i || ' minutes') AS ts FROM n)",
                [format!("ws_{name}")],
            )
            .expect("seed runs");
        }
    }

    let page = |state_filter| all_job_runs_json(&state, 50, state_filter, &JobRunScope::default());
    // Warm: every workspace runtime is open and the store pages are cached.
    let warm = page(AllJobRunsState::All);
    assert_eq!(warm["items"].as_array().expect("items").len(), 50);
    assert_eq!(warm["truncated"], true);
    assert_eq!(warm["unavailable"], serde_json::json!([]));
    assert_eq!(warm["items"][0]["run_id"], "jrun-03999");

    // A terminal run whose timing is incomplete is exactly what reconciling
    // reads repair. The dashboard must show it as stored.
    let alpha = state.runtime_for("alpha").expect("alpha runtime");
    let mut incomplete = super::test_support::seed_run(
        &alpha,
        "jrun-incomplete",
        "bulk",
        orbit_core::JobRunState::Failed,
    );
    incomplete.finished_at = None;
    incomplete.duration_ms = None;
    store
        .upsert_job_run_for_workspace("ws_alpha", &incomplete, None)
        .expect("store incomplete run");

    let started = std::time::Instant::now();
    let all = page(AllJobRunsState::All);
    let failed = page(AllJobRunsState::Failed);
    let elapsed = started.elapsed();
    assert_eq!(all["items"].as_array().expect("items").len(), 50);
    assert_eq!(failed["items"][0]["run_id"], "jrun-incomplete");
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "two warm aggregate pages over 20k runs took {elapsed:?}"
    );
    let stored = alpha
        .show_job_run_observed("jrun-incomplete")
        .expect("incomplete run");
    assert_eq!(
        (stored.state, stored.finished_at),
        (orbit_core::JobRunState::Failed, None),
        "a dashboard GET must not reconcile runs"
    );
}

/// The aggregate task list chooses its page from the task index and opens
/// only the page's bundles. A cold dashboard used to parse every registered
/// `task.yaml` first [ORB-14595]. The off-page envelopes here are overwritten
/// in place with unparseable bytes that keep their file's stamp, so any read
/// of them would fail the workspace out of the list.
#[test]
fn aggregate_task_page_opens_only_the_returned_bundles() {
    if !enter_isolated_child(
        module_path!(),
        "aggregate_task_page_opens_only_the_returned_bundles",
    ) {
        return;
    }
    use axum::extract::{RawQuery, State};

    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
    let (_bravo_orbit, bravo_repo) = seed_workspace(&global_root, tmp.path(), "bravo");
    let alpha = OrbitRuntime::from_roots(&global_root, &alpha_orbit)
        .expect("alpha runtime")
        .with_actor(ActorIdentity::human("human"));
    for title in ["alpha newer", "alpha newest"] {
        alpha
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: "page".to_string(),
                ..Default::default()
            })
            .expect("add task");
    }
    let bravo =
        OrbitRuntime::from_roots(&global_root, &bravo_repo.join(".orbit")).expect("bravo runtime");
    let off_page = [&alpha, &bravo].map(|runtime| {
        let tasks = runtime.list_tasks().expect("list tasks");
        let seeded = tasks.iter().find(|task| task.description == "seed");
        seeded.expect("seeded task").id.clone()
    });
    drop(bravo);
    drop(alpha);
    write_registry(
        &global_root,
        &[("alpha", &alpha_repo), ("bravo", &bravo_repo)],
    );

    let list = |state: DashboardState| {
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(async {
                let response = super::super::workspaces::list_all_tasks(
                    State(state),
                    RawQuery(Some("limit=2".to_string())),
                )
                .await;
                super::test_support::body_json(response).await
            });
        let titles = response["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|task| task["title"].as_str().expect("title").to_string())
            .collect::<Vec<_>>();
        (
            titles,
            response["total"].clone(),
            response["truncated"].clone(),
        )
    };
    let expected = (
        vec!["alpha newest".to_string(), "alpha newer".to_string()],
        serde_json::json!(4),
        serde_json::json!(true),
    );
    // A first listing proves every envelope against the index, in any
    // process; the proofs outlive its in-memory cache.
    assert_eq!(list(registry_state(&global_root)), expected);

    for id in off_page {
        let envelope = find_envelope(&global_root.join("tasks"), &id).expect("task envelope");
        let modified = std::fs::metadata(&envelope)
            .and_then(|metadata| metadata.modified())
            .expect("envelope mtime");
        let len = std::fs::metadata(&envelope).expect("envelope").len();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&envelope)
            .expect("open envelope in place");
        std::io::Write::write_all(&mut file, &vec![b'{'; len as usize]).expect("overwrite");
        file.set_modified(modified).expect("keep mtime");
    }

    // A cold dashboard: new runtimes, empty caches.
    assert_eq!(list(registry_state(&global_root)), expected);
}

/// The `task.yaml` of `task_id`'s bundle somewhere under `dir`.
fn find_envelope(dir: &Path, task_id: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .find_map(|path| {
            if path.file_name().is_some_and(|name| name == task_id) {
                Some(path.join("task.yaml"))
            } else {
                find_envelope(&path, task_id)
            }
        })
}
