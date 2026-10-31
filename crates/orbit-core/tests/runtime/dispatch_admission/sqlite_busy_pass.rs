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

const TEST: &str = "dispatch_admission::sqlite_busy_pass::a_drain_defers_an_admission_pass_held_out_by_a_sqlite_writer";

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
