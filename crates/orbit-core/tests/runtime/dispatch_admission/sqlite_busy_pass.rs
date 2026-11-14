//! A drain admission pass that loses only to another connection's SQLite
//! writer lock on the host store.
//!
//! The admission section publishes its wave in one SQLite write transaction.
//! When another writer holds the database past the busy timeout, SQLite's
//! busy result keeps its typed lock-busy class through the store's error
//! translation, so the pass is deferred like one held out by the task-commit
//! file lock, not failed.

use std::time::Duration;

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::json;

use super::{Seed, as_operator, isolated, running_run, runtime, seed};

/// Exercise the task-update tool used by CLI callers, and run dispatch, at
/// the precise point where GC used to rank history under BEGIN IMMEDIATE.
#[test]
fn store_gc_allows_task_updates_and_dispatch_during_history_selection() {
    const TEST: &str = "dispatch_admission::sqlite_busy_pass::store_gc_allows_task_updates_and_dispatch_during_history_selection";
    if !isolated(TEST) {
        return;
    }
    use orbit_core::OrbitRuntime;
    use orbit_store::contracts::RunResourceMinute;
    use rusqlite::functions::FunctionFlags;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let (root, runtime, _repo) = runtime();
    let task = seed(&runtime, Seed::default());
    let writer = OrbitRuntime::from_roots(
        &root.path().join("home/.orbit"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    writer
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .busy_timeout(Duration::ZERO)
        .unwrap();
    let store = runtime.sqlite_store().unwrap();
    assert!(!Arc::ptr_eq(
        &store.connection(),
        &writer.sqlite_store().unwrap().connection()
    ));
    let jobs = orbit_store::compose::workspace_job_run_store(
        store.clone(),
        runtime.workspace_id().unwrap(),
    );
    let at = chrono::Utc::now() - chrono::Duration::days(40);
    let run = jobs
        .insert_job_run("task_auto_pipeline", 1, at, None, None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, at, std::process::id())
        .unwrap();
    jobs.finalize_job_run(
        &run.run_id,
        orbit_types::workflow::JobRunState::Success,
        at,
        Some(1),
    )
    .unwrap();
    // One delivery outside the retained cohort makes GC exercise both its
    // planning read and its deletion path.
    use orbit_store::contracts::ResourceUsageStoreBackend;
    store
        .record_run_resource_minute(&RunResourceMinute {
            workspace_id: runtime.workspace_id().unwrap(),
            run_id: run.run_id.clone(),
            minute_index: 0,
            minute_started_at: at,
            task_id: task.id.clone(),
            work_class: "change".into(),
            complexity: Some("medium".into()),
            resumed: false,
            delivery_admitted_at: Some(at),
            cpu_cores: 1.0,
            cpu_accounting: Some(orbit_store::contracts::CPU_ACCOUNTING_LINUX.into()),
            memory_bytes: 1024,
            samples: 1,
            crew_model: Some("fixture-model".into()),
            orbit_version: None,
        })
        .unwrap();
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<?1)
         INSERT INTO job_runs(workspace_id, run_id, job_id, attempt, state, scheduled_at,
                              created_at, finished_at)
         SELECT workspace_id, printf('gc-history-%04d', n), job_id, 1, state, scheduled_at,
                created_at, finished_at FROM job_runs, ids WHERE run_id = ?2",
            rusqlite::params![
                usize::try_from(orbit_config::AUTO_CONCURRENCY_PROFILE_WINDOW).unwrap().max(
                    orbit_core::runtime::host_resource::profile::RESOURCE_HISTORY_DELIVERIES_PER_KEY
                ),
                run.run_id
            ],
        )
        .unwrap();
    store.connection().lock().unwrap().execute(
        "INSERT INTO run_resource_minutes(workspace_id, run_id, minute_index, minute_started_at,
             task_id, work_class, complexity, delivery_admitted_at, cpu_cores, memory_bytes,
             samples, cpu_accounting, crew_model)
         SELECT j.workspace_id, j.run_id, 0, m.minute_started_at, 'zz-' || j.run_id, 'change',
                'medium', m.delivery_admitted_at, 1, 1024, 1, m.cpu_accounting, m.crew_model
         FROM job_runs j JOIN run_resource_minutes m ON m.workspace_id = j.workspace_id
         WHERE j.run_id LIKE 'gc-history-%' AND m.run_id = ?1",
        [&run.run_id],
    ).unwrap();
    let writer_jobs = orbit_store::compose::workspace_job_run_store(
        writer.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let attempted = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::new(writer);
    let install = |connection: &rusqlite::Connection| {
        let attempted = attempted.clone();
        let failures = failures.clone();
        let writer = writer.clone();
        let writer_jobs = writer_jobs.clone();
        let task_id = task.id.clone();
        let called_on_connection = AtomicBool::new(false);
        connection
            .create_scalar_function("julianday", 1, FunctionFlags::SQLITE_UTF8, move |_| {
                if !called_on_connection.swap(true, Ordering::SeqCst) {
                    let attempt = attempted.fetch_add(1, Ordering::SeqCst) + 1;
                    let update = writer.run_tool_with_context_and_role(
                        "orbit.task.update",
                        json!({"id": task_id, "title": format!("updated during GC {attempt}")}),
                        orbit_types::policy::Role::Admin,
                        ToolContext {
                            session_context: orbit_types::tool::ToolSessionContext {
                                effective_capabilities: std::collections::BTreeSet::from([
                                    orbit_types::tool::McpCapability::Operator,
                                ]),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    );
                    if let Err(error) = update {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("task update: {error}"));
                    }
                    if let Err(error) = writer_jobs.insert_job_run(
                        "dispatch-during-gc",
                        1,
                        chrono::Utc::now(),
                        None,
                        None,
                    ) {
                        failures.lock().unwrap().push(format!("dispatch: {error}"));
                    }
                }
                Ok(2_461_042.5_f64)
            })
            .unwrap();
    };
    install(&store.connection().lock().unwrap());
    store
        .with_read_connection(|connection| {
            install(connection);
            Ok(())
        })
        .unwrap();
    let report = runtime.gc_runs(true, None).unwrap();
    assert!(
        report.resource_minutes[0].rows_removed > 0,
        "GC must exercise its deletion path"
    );
    assert!(
        attempted.load(Ordering::SeqCst) > 0,
        "selection must reach the deterministic write probe"
    );
    let failures = failures.lock().unwrap();
    assert!(
        failures.is_empty(),
        "GC selection must allow task writes and dispatch (ORB-24645): {:?}",
        *failures
    );
    assert_eq!(
        writer.get_task(&task.id).unwrap().title,
        format!("updated during GC {}", attempted.load(Ordering::SeqCst))
    );
}

const TEST: &str = "dispatch_admission::sqlite_busy_pass::a_drain_defers_an_admission_pass_held_out_by_a_sqlite_writer";

/// A diagnostic checkpoint is advisory; contention there must not end the
/// auto drain's loop before the next admission pass (ORB-24645).
#[test]
fn a_drain_retries_after_its_adoption_checkpoint_hits_a_sqlite_writer() {
    const TEST: &str = "dispatch_admission::sqlite_busy_pass::a_drain_retries_after_its_adoption_checkpoint_hits_a_sqlite_writer";
    if !isolated(TEST) {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    seed(&runtime, Seed::default());
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let store = runtime.sqlite_store().unwrap();
    store
        .connection()
        .lock()
        .unwrap()
        .busy_timeout(Duration::ZERO)
        .unwrap();
    let database = store
        .with_read_connection(|connection| {
            connection
                .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
                .map_err(orbit_common::storage::sqlite::sqlite_store_error)
        })
        .unwrap();
    let holder = rusqlite::Connection::open(database).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let input = json!({"run_id": drain, "max_active_leaf_runs": 2, "poll_sleep_seconds": 0});
    let adoption = runtime.run_deterministic(
        "adopt_orphaned_deliveries",
        &json!({}),
        &input,
        ToolContext::default(),
    );
    let admission = runtime.run_deterministic(
        "classify_workspace_auto_tasks",
        &json!({}),
        &input,
        ToolContext::default(),
    );
    holder.execute_batch("ROLLBACK").unwrap();
    adoption.expect("a busy diagnostic checkpoint must leave the loop alive");
    let deferred = admission.expect("a busy deferral checkpoint must leave the loop alive");
    assert_eq!(deferred["pass_deferred"]["code"], "lock_busy", "{deferred}");
    assert_eq!(
        runtime.show_job_run(&drain).unwrap().state.to_string(),
        "running"
    );
    let next = runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("the next pass succeeds after the writer releases its lock");
    assert!(next.get("pass_deferred").is_none(), "{next}");
    assert!(
        !next["loose_task_dispatches"].as_array().unwrap().is_empty(),
        "{next}"
    );
}

/// The host store holds a writer lock for one busy timeout plus a margin: the
/// pass's admission write gives up while it is held, and the deferral it then
/// records outlasts it.
#[test]
fn a_drain_defers_an_admission_pass_held_out_by_a_sqlite_writer() {
    if !isolated(TEST) {
        return;
    }
    let (root, runtime, _repo) = runtime();
    seed(&runtime, Seed::default());
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let database = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
        root.path().join("home/.orbit"),
        root.path().join("repo/.orbit"),
    ))
    .expect("host database path");
    let busy_timeout = Duration::from_millis(u64::from(
        orbit_common::storage::sqlite::DEFAULT_BUSY_TIMEOUT_MS,
    ));
    let (held, held_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(&database).expect("open host database");
        connection
            .execute_batch("BEGIN IMMEDIATE;")
            .expect("hold the host writer lock");
        held.send(()).expect("report the held lock");
        std::thread::sleep(busy_timeout + Duration::from_millis(2_500));
        connection
            .execute_batch("ROLLBACK;")
            .expect("release the host writer lock");
    });
    held_rx.recv().expect("the holder takes the lock");

    let deferred = runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 2, "poll_sleep_seconds": 7}),
            ToolContext::default(),
        )
        .expect("a pass held out by a SQLite writer is deferred, not failed");
    holder.join().expect("the holder releases the lock");

    assert_eq!(deferred["pass_deferred"]["code"], "lock_busy", "{deferred}");
    assert_eq!(deferred["loose_task_dispatches"], json!([]), "{deferred}");
    let shown = as_operator(&runtime, "orbit.workflow.run.show", json!({"id": drain}));
    let pass = &shown["drain_last_pass"];
    assert_eq!(pass["last_pass_error_code"], "lock_busy", "{pass}");
    assert_eq!(pass["consecutive_pass_failures"], 1, "{pass}");
    assert!(
        pass["last_pass_error"].as_str().is_some_and(
            |error| error.starts_with("admit the wave through the admission section: ")
        ),
        "the SQLite writer held out the admission section's write: {pass}"
    );
    assert_eq!(shown["state"], "running", "{shown}");
}
