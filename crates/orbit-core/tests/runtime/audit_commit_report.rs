//! A committed tool mutation stays a success when its audit insert fails
//! [ORB-16383].
//!
//! The audit row is written after the tool's own store commits, in the global
//! audit database. Reporting that insert as a call failure makes a caller
//! that retries errors create a second object. The failure here is a trigger,
//! so the test does not hold a database lock or wait out a busy timeout.

use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tempfile::TempDir;

const FAILED_AUDIT_TITLE: &str = "audit insert fails after the task commits";
const NORMAL_TITLE: &str = "audit insert commits with the task";

#[test]
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
