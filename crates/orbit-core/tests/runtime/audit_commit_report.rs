//! A committed tool mutation stays a success when its audit insert fails
//! [ORB-16383].
//!
//! The audit row is written after the tool's own store commits, in the global
//! audit database. Reporting that insert as a call failure makes a caller
//! that retries errors create a second object. The failure here is a trigger,
//! so the test does not hold a database lock or wait out a busy timeout.
//!
//! An audit insert that loses only to another writer's SQLite lock is retried
//! once. The busy result is told apart by its typed lock-busy class, not by
//! SQLite's message text, so that case holds a real writer lock.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

const FAILED_AUDIT_TITLE: &str = "audit insert fails after the task commits";
const NORMAL_TITLE: &str = "audit insert commits with the task";

fn committed_task_add_stays_success_when_its_audit_insert_fails() {
    let (_root, runtime, repo_root, database) = test_runtime();
    install_audit_failure(&database);

    let failed = add_unless_the_call_reports_failure(&runtime, &repo_root, FAILED_AUDIT_TITLE);
    let id = failed["id"]
        .as_str()
        .expect("success names the committed task")
        .to_string();
    assert_eq!(failed["audit_warning"]["code"], "audit_row_unpersisted");
    assert_eq!(
        failed["audit_warning"]["committed"]["tool"],
        "orbit.task.add"
    );
    assert_eq!(failed["audit_warning"]["committed"]["id"], id);
    let message = failed["audit_warning"]["message"]
        .as_str()
        .expect("warning message");
    assert!(
        message.contains(&id) && message.contains("injected audit write failure"),
        "the warning must name the committed task and the audit failure: {message}"
    );
    assert!(
        failed.get("code").is_none(),
        "the success document must not carry an error code: {failed}"
    );

    let stored = runtime.get_task(&id).expect("the task was committed");
    assert_eq!(stored.title, FAILED_AUDIT_TITLE);
    let committed = tasks_titled(&runtime, FAILED_AUDIT_TITLE);
    assert_eq!(
        committed,
        vec![id.clone()],
        "a caller that saw this success must not have retried into a second task"
    );
    assert_eq!(
        audit_rows(&runtime).len(),
        0,
        "the forced audit failure must not leave a row for the call"
    );

    clear_audit_failure(&database);
    let normal = add_unless_the_call_reports_failure(&runtime, &repo_root, NORMAL_TITLE);
    assert!(
        normal.get("audit_warning").is_none(),
        "a persisted audit row is not a warning: {normal}"
    );
    let rows = audit_rows(&runtime);
    assert_eq!(
        rows.len(),
        1,
        "the call whose audit insert succeeded has exactly one row: {rows:?}"
    );
    assert_eq!(tasks_titled(&runtime, NORMAL_TITLE).len(), 1);
    assert_eq!(tasks_titled(&runtime, FAILED_AUDIT_TITLE), vec![id]);
}

/// A committed mutating call whose audit insert outlasts one busy timeout
/// behind another connection's writer lock retries the insert once, so the
/// row still lands. The lock is released only after the dispatcher reports
/// that its first insert lost to it, so no host speed lets the first insert
/// outwait the holder: without the retry the call returns an `audit_warning`
/// and no row.
fn audit_insert_held_out_by_a_sqlite_writer_lock_is_retried() {
    orbit_common::storage::sqlite::with_sqlite_busy_timeout(500, || {
        let (_root, runtime, repo_root, database) = test_runtime();
        let (held, held_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(&database).expect("open audit database");
            connection
                .execute_batch("BEGIN IMMEDIATE;")
                .expect("hold the audit writer lock");
            held.send(()).expect("report the held lock");
            // Released by the first insert's loss, or when the call returns.
            let _ = release_rx.recv();
            connection
                .execute_batch("ROLLBACK;")
                .expect("release the audit writer lock");
        });
        held_rx.recv().expect("the holder takes the lock");

        let first_attempt_lost = Arc::new(AtomicBool::new(false));
        let subscriber = tracing_subscriber::registry().with(FirstInsertLostTheLock {
            observed: first_attempt_lost.clone(),
            release: Mutex::new(release.clone()),
        });
        let added = tracing::subscriber::with_default(subscriber, || {
            runtime.execute_tool_command(
                "orbit.task.add",
                add_input(&repo_root, FAILED_AUDIT_TITLE),
                None,
                Some("grok".into()),
            )
        });
        // A call that never reported the loss must not leave the holder blocked.
        let _ = release.send(());
        holder.join().expect("the holder releases the lock");
        let added = added.expect("a committed call succeeds behind the audit lock");

        assert!(
            first_attempt_lost.load(Ordering::SeqCst),
            "the first audit insert never reported losing the lock: {added}"
        );
        assert!(
            added.get("audit_warning").is_none(),
            "the retried insert persisted: {added}"
        );
        assert_eq!(audit_rows(&runtime).len(), 1, "exactly one audit row");
    });
}

/// Releases the held writer lock when the dispatcher reports that its first
/// audit insert lost to it and is retrying.
struct FirstInsertLostTheLock {
    observed: Arc<AtomicBool>,
    release: Mutex<std::sync::mpsc::Sender<()>>,
}

impl<S: tracing::Subscriber> Layer<S> for FirstInsertLostTheLock {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        if *event.metadata().level() == tracing::Level::WARN
            && message.0.contains("lost the audit-store lock")
        {
            self.observed.store(true, Ordering::SeqCst);
            let _ = self.release.lock().expect("release sender").send(());
        }
    }
}

/// The caller from the incident: a reported failure is retried once, and a
/// success is not. Retrying the success path would hide the bug.
fn add_unless_the_call_reports_failure(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    title: &str,
) -> Value {
    let input = add_input(repo_root, title);
    match runtime.execute_tool_command("orbit.task.add", input.clone(), None, Some("grok".into())) {
        Ok(value) => value,
        Err(error) => {
            let retry =
                runtime.execute_tool_command("orbit.task.add", input, None, Some("grok".into()));
            panic!(
                "orbit.task.add reported failure after committing ({error}); \
                 retrying that failure left {:?} task(s) titled {title:?}. \
                 retry result: {retry:?}",
                tasks_titled(runtime, title)
            );
        }
    }
}

fn add_input(repo_root: &Path, title: &str) -> Value {
    json!({
        "title": title,
        "description": "Fixture for an audit insert that fails after commit.",
        "acceptance_criteria": ["The committed task is not reported as a failed call."],
        "complexity": "low",
        "workspace": repo_root.to_string_lossy(),
        "type": "bug",
        "model": "grok",
    })
}

fn tasks_titled(runtime: &OrbitRuntime, title: &str) -> Vec<String> {
    runtime
        .list_tasks()
        .expect("list tasks")
        .into_iter()
        .filter(|task| task.title == title)
        .map(|task| task.id)
        .collect()
}

fn audit_rows(runtime: &OrbitRuntime) -> Vec<orbit_types::telemetry::AuditEvent> {
    runtime
        .list_audit_events(None, Some("orbit.task.add".to_string()), None, None, 20)
        .expect("list audit rows")
}

fn test_runtime() -> (TempDir, OrbitRuntime, PathBuf, PathBuf) {
    crate::scenarios::require_isolated_mutable_fixture("audit_commit_report::test_runtime");
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).expect("tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("global root");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    let database = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
        &global_root,
        &workspace_root,
    ))
    .expect("audit database path");
    (root, runtime, repo_root, database)
}

fn install_audit_failure(database: &Path) {
    let connection = rusqlite::Connection::open(database).expect("open audit database");
    connection
        .execute_batch(
            "CREATE TRIGGER injected_tool_audit_failure
             BEFORE INSERT ON audit_events
             WHEN NEW.command = 'tool' AND NEW.tool_name = 'orbit.task.add'
             BEGIN
                 SELECT RAISE(ABORT, 'injected audit write failure');
             END;",
        )
        .expect("install audit failure trigger");
}

fn clear_audit_failure(database: &Path) {
    let connection = rusqlite::Connection::open(database).expect("open audit database");
    connection
        .execute_batch("DROP TRIGGER IF EXISTS injected_tool_audit_failure;")
        .expect("drop audit failure trigger");
}

crate::scenario_table!(
    mutable_scenarios,
    committed_task_add_stays_success_when_its_audit_insert_fails,
    audit_insert_held_out_by_a_sqlite_writer_lock_is_retried,
);
