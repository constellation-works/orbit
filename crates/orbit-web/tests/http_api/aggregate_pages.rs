use super::support::{isolated, json_ok, spawn_server};
use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, JobRun, JobRunState, OrbitRuntime, TaskStatus};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use std::path::{Path, PathBuf};
fn seed_workspace(global_root: &Path, base: &Path, name: &str) -> (PathBuf, PathBuf) {
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
            name: if *id == "alpha" {
                "fixture".into()
            } else {
                (*id).to_string()
            },
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

#[test]
fn fleet_drains_exposes_idle_ready_work_beside_a_live_workspace() {
    isolated(
        "aggregate_pages::fleet_drains_exposes_idle_ready_work_beside_a_live_workspace",
        || {
            let tmp = tempfile::tempdir().expect("tempdir");
            let global = tmp.path().join("global");
            std::fs::create_dir_all(&global).unwrap();
            std::fs::write(global.join("config.toml"),
                "[workflow]\ndefault_crew = \"sol\"\n[crews.sol]\nprovider = \"codex\"\nmodel = \"fixture\"\n").unwrap();
            let (alpha_orbit, alpha_repo) = seed_workspace(&global, tmp.path(), "alpha");
            let (bravo_orbit, bravo_repo) = seed_workspace(&global, tmp.path(), "bravo");
            write_registry(&global, &[("alpha", &alpha_repo), ("bravo", &bravo_repo)]);
            let alpha = OrbitRuntime::from_roots(&global, &alpha_orbit).unwrap();
            let bravo = OrbitRuntime::from_roots(&global, &bravo_orbit)
                .unwrap()
                .with_actor(ActorIdentity::human("fixture"));
            std::fs::write(bravo_repo.join("ready.rs"), "fixture").unwrap();
            bravo
                .add_task(TaskAddParams {
                    title: "Idle ready work".into(),
                    description: "Ready work must remain visible without a drain".into(),
                    acceptance_criteria: vec!["The work is visible".into()],
                    plan: "Fixture plan".into(),
                    context_files: vec!["file:ready.rs".into()],
                    complexity: orbit_core::TaskComplexity::Medium,
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            let mut drain = seed_run(
                &alpha,
                "jrun-fleet",
                "workspace_auto_pipeline",
                JobRunState::Running,
            );
            drain.input = Some(serde_json::json!({"max_active_leaf_runs": 4}));
            write_seeded_run(&alpha, &drain);
            // Its latest pass found no task pilot enabled on this machine.
            let mut state = orbit_types::workflow::PipelineState::new(
                drain.run_id.clone(),
                drain.job_id.clone(),
                serde_json::json!({}),
            );
            state.pull_drain.drain_last_pass = Some(
                serde_json::from_value(serde_json::json!({
                    "recorded_at": Utc::now(),
                    "queued": 0,
                    "task_pilot_warning": "Task pilot is not enabled for workspace 'alpha'",
                }))
                .unwrap(),
            );
            alpha.write_run_state(&drain.run_id, &state).unwrap();
            // A second window started while that one runs waits behind it.
            let mut queued = seed_run(
                &alpha,
                "jrun-fleet-queued",
                "workspace_auto_pipeline",
                JobRunState::Pending,
            );
            queued.input =
                Some(serde_json::json!({"for_seconds": 28800, "allowed_crews": ["sol"]}));
            write_seeded_run(&alpha, &queued);
            let server = spawn_server(
                tmp.path(),
                true,
                false,
                false,
                &tmp.path().join("process.log"),
                None,
                &[],
            );
            let payload = json_ok(server.get("/api/fleet/drains"));
            let rows = payload["items"].as_array().unwrap();
            assert_eq!(rows.len(), 2, "every registered workspace is visible");
            let row = |name: &str| rows.iter().find(|row| row["workspace_id"] == name).unwrap();
            assert_eq!(row("alpha")["phase"], "draining", "{payload:#}");
            assert_eq!(row("alpha")["slots_used"], 0);
            assert_eq!(row("alpha")["slots_ceiling"], 4);
            let queued = &row("alpha")["queued_drains"][0];
            assert_eq!(queued["run_id"], "jrun-fleet-queued", "{payload:#}");
            assert_eq!(queued["allowed_crews"], serde_json::json!(["sol"]));
            assert_eq!(queued["for_seconds"], 28800);
            assert_eq!(
                row("alpha")["task_pilot_warning"],
                "Task pilot is not enabled for workspace 'alpha'",
                "the live drain's pilot warning reaches its row: {payload:#}"
            );
            assert_eq!(row("bravo")["phase"], "idle");
            assert_eq!(row("bravo")["task_pilot_warning"], serde_json::Value::Null);
            assert_eq!(
                row("bravo")["eligible_count"],
                1,
                "idle ready work is discoverable: {payload:#}"
            );
            for row in rows {
                assert!(row["holds"].is_array());
                assert!(row["queued_drains"].is_array());
                assert!(row.get("top_wait_reason").is_some());
                assert_eq!(row["counts_truncated"], false);
            }
            // A waiting task gets a reason in the fleet, rather than looking
            // identical to an empty workspace.
            bravo
                .add_task(TaskAddParams {
                    title: "Needs preparation".into(),
                    description: "Incomplete task waits for preparation".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            let waiting = json_ok(server.get("/api/fleet/drains"));
            let row = waiting["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["workspace_id"] == "bravo")
                .unwrap();
            assert!(
                row["top_wait_reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "{waiting:#}"
            );
            assert_eq!(row["eligible_count"], 1);
        },
    );
}

#[test]
fn aggregate_job_runs_page_is_bounded_and_observational() {
    isolated(
        "aggregate_pages::aggregate_job_runs_page_is_bounded_and_observational",
        || {
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
            let alpha = OrbitRuntime::from_roots(&global_root, &repos[0].join(".orbit"))
                .expect("alpha runtime");
            let store = alpha.sqlite_store().expect("store");
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

            let server = spawn_server(
                tmp.path(),
                true,
                false,
                false,
                &tmp.path().join("process.log"),
                None,
                &[],
            );
            let page = |state_filter: &str| {
                json_ok(server.get(&format!("/api/job-runs/all?limit=50&state={state_filter}")))
            };
            // Warm: every workspace runtime is open and the store pages are cached.
            let warm = page("all");
            assert_eq!(warm["items"].as_array().expect("items").len(), 50);
            assert_eq!(warm["total"], 20_000);
            assert_eq!(warm["truncated"], true);
            assert_eq!(warm["unavailable"], serde_json::json!([]));
            assert_eq!(warm["items"][0]["run_id"], "jrun-03999");

            // A terminal run whose timing is incomplete is exactly what reconciling
            // reads repair. The dashboard must show it as stored.
            let mut incomplete = seed_run(
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
            let all = page("all");
            let failed = page("failed");
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
        },
    );
}

#[test]
fn aggregate_job_runs_total_sums_every_workspace_independent_of_page_size() {
    isolated(
        "aggregate_pages::aggregate_job_runs_total_sums_every_workspace_independent_of_page_size",
        || {
            let tmp = tempfile::tempdir().expect("tempdir");
            let global_root = tmp.path().join("global");
            std::fs::create_dir_all(&global_root).expect("create global root");
            let (alpha_orbit, alpha_repo) = seed_workspace(&global_root, tmp.path(), "alpha");
            let (bravo_orbit, bravo_repo) = seed_workspace(&global_root, tmp.path(), "bravo");
            write_registry(
                &global_root,
                &[("alpha", &alpha_repo), ("bravo", &bravo_repo)],
            );
            let server = spawn_server(
                tmp.path(),
                true,
                false,
                false,
                &tmp.path().join("process.log"),
                None,
                &[],
            );
            let page = |query: &str| json_ok(server.get(&format!("/api/job-runs/all?{query}")));
            // Open both workspace runtimes before seeding, so the in-flight
            // runs below are read as stored rather than reconciled on open.
            assert_eq!(page("state=active&limit=1")["total"], 0);

            let alpha = OrbitRuntime::from_roots(&global_root, &alpha_orbit).expect("alpha");
            let bravo = OrbitRuntime::from_roots(&global_root, &bravo_orbit).expect("bravo");
            for (runtime, prefix, active) in [(&alpha, "a", 3), (&bravo, "b", 2)] {
                for index in 0..active {
                    let state = if index == 0 {
                        JobRunState::Pending
                    } else {
                        JobRunState::Running
                    };
                    seed_run(
                        runtime,
                        &format!("jrun-{prefix}-active-{index}"),
                        "ship",
                        state,
                    );
                }
                seed_run(
                    runtime,
                    &format!("jrun-{prefix}-done"),
                    "ship",
                    JobRunState::Success,
                );
            }

            // The Jobs rail asks for a one-item page; its total is still the
            // per-workspace sum (3 + 2), not the page length.
            for limit in [1, 100] {
                let active = page(&format!("state=active&limit={limit}"));
                let items = active["items"].as_array().expect("items");
                assert_eq!(active["total"], 5, "active total at limit {limit}");
                assert_eq!(items.len(), limit.min(5));
                assert_eq!(active["truncated"], limit < 5);
            }
            assert_eq!(page("state=all&limit=1")["total"], 7);
        },
    );
}

#[test]
fn aggregate_task_page_opens_only_the_returned_bundles() {
    isolated(
        "aggregate_pages::aggregate_task_page_opens_only_the_returned_bundles",
        || {
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
            let bravo = OrbitRuntime::from_roots(&global_root, &bravo_repo.join(".orbit"))
                .expect("bravo runtime");
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

            let list = || {
                let server = spawn_server(
                    tmp.path(),
                    true,
                    false,
                    false,
                    &tmp.path().join("process.log"),
                    None,
                    &[],
                );
                let response = json_ok(server.get("/api/tasks/all?limit=2"));
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
            assert_eq!(list(), expected);

            for id in off_page {
                let envelope =
                    find_envelope(&global_root.join("tasks"), &id).expect("task envelope");
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
            assert_eq!(list(), expected);
        },
    );
}
fn seed_run(runtime: &OrbitRuntime, run_id: &str, job_id: &str, state: JobRunState) -> JobRun {
    let now = Utc::now();
    let run = JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: matches!(
            state,
            JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
        )
        .then_some(now),
        finished_at: state.is_terminal().then_some(now),
        duration_ms: state.is_terminal().then_some(0),
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    write_seeded_run(runtime, &run);
    run
}
fn write_seeded_run(runtime: &OrbitRuntime, run: &JobRun) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    runtime
        .sqlite_store()
        .expect("sqlite store")
        .upsert_job_run_for_workspace(&workspace_id, run, None)
        .expect("insert job run");
}
