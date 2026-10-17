//! A federated session to a destination whose `orbit` gave up on its own
//! locked store, through the public [`SshDestinationProbe`] and a stub `ssh`
//! on `PATH`, each row in an isolated child.
//!
//! The destination was reached and decided nothing, so the session reports
//! its `lock_busy` answer rather than an unreachable host: a bound worker
//! retries it, and a follower drain backs off instead of suppressing its own
//! host for the window. Only `orbit`'s final error counts — ssh's own failure
//! (exit 255) and a lock error quoted by an earlier warning stay unreachable —
//! and a `tools/call` lost the same way stays `outcome_unknown`, because it
//! may already have run. The errors the stub prints are built from the
//! `OrbitError` values the destination would fail with, so a change to their
//! display cannot silently stop the classification.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::file_lock::FileLockTimeout;
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{Destination, DestinationProbe, SshDestinationProbe};
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

const CHILD: &str = "ORBIT_BUSY_OWNER_ROUTE_CHILD";
const TEST: &str =
    "busy_owner_route::a_session_that_ends_on_the_destinations_locked_store_is_lock_busy";

/// Answers `initialize` and fails at `ORBIT_STUB_AT` (`start` for an `orbit`
/// that exits before reading its input) by writing `ORBIT_STUB_STDERR` and
/// exiting with `ORBIT_STUB_EXIT`, as ssh relays the remote command's exit.
const STUB: &str = r#"#!/usr/bin/env python3
import json, os, sys
def fail():
    sys.stderr.write(os.environ['ORBIT_STUB_STDERR'] + '\n')
    sys.stderr.flush()
    os._exit(int(os.environ['ORBIT_STUB_EXIT']))
at = os.environ['ORBIT_STUB_AT']
if at == 'start':
    fail()
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    if request['method'] == at:
        fail()
    if request['method'] != 'initialize':
        raise RuntimeError('unexpected request: ' + request['method'])
    result = {'protocolVersion': request['params']['protocolVersion']}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

/// How `orbit` reports a failed command on stderr.
fn orbit_error(error: &OrbitError) -> String {
    format!("error: {error}")
}

/// The store error a SQLite writer lock that outlasted the busy timeout
/// becomes, as the destination's `orbit` builds it: a real write that loses
/// to another connection's exclusive transaction.
fn locked_store() -> OrbitError {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orbit.db");
    let holder = rusqlite::Connection::open(&path).unwrap();
    holder
        .execute_batch("CREATE TABLE t (x); BEGIN EXCLUSIVE; INSERT INTO t VALUES (1);")
        .unwrap();
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    let error = writer
        .execute("INSERT INTO t VALUES (2)", [])
        .expect_err("the holder's exclusive transaction locks the database");
    orbit_common::storage::sqlite::sqlite_store_error(error)
}

fn contended_store() -> OrbitError {
    OrbitError::SqliteContention(Box::new(orbit_common::SqliteContention {
        path: "/state/orbit.db".into(),
        phase: "open".into(),
        detail: "database is locked".into(),
    }))
}

fn file_lock_timeout() -> OrbitError {
    OrbitError::FileLockTimeout(Box::new(FileLockTimeout {
        lock_path: PathBuf::from("/state/tasks/.task-commit.lock"),
        label: "task commit boundary".into(),
        timeout_ms: 30_000,
        holder: None,
        shared_holders: Vec::new(),
    }))
}

#[test]
fn a_session_that_ends_on_the_destinations_locked_store_is_lock_busy() {
    if std::env::var_os(CHILD).is_some() {
        exercise_child();
        return;
    }
    let warning = format!(
        "2026-10-16T00:00:00Z WARN orbit.store: retrying after {}",
        locked_store()
    );
    let rows = [
        // The incident: `orbit` exits at startup on its locked store, after
        // tracing a warning.
        (
            "locked-store",
            format!("{warning}\n{}", orbit_error(&locked_store())),
            1,
            "start",
            "lock_busy",
        ),
        (
            "contended-store",
            orbit_error(&contended_store()),
            1,
            "start",
            "lock_busy",
        ),
        (
            "file-lock-timeout",
            orbit_error(&file_lock_timeout()),
            1,
            "initialize",
            "lock_busy",
        ),
        // ssh's own failure, whatever the remote printed before it.
        (
            "ssh-failure",
            orbit_error(&locked_store()),
            255,
            "start",
            "unreachable",
        ),
        (
            "warning-then-broken-pipe",
            format!("{warning}\nclient_loop: send disconnect: Broken pipe"),
            255,
            "start",
            "unreachable",
        ),
        // A lock error only an earlier warning quoted is not the exit error.
        (
            "warning-then-other-error",
            format!(
                "{warning}\n{}",
                orbit_error(&OrbitError::Io("permission denied".into()))
            ),
            1,
            "start",
            "unreachable",
        ),
        // A dispatched call may already have run.
        (
            "tools-call",
            orbit_error(&locked_store()),
            1,
            "tools/call",
            "outcome_unknown",
        ),
    ];
    for (row, stderr, exit, at, expected) in rows {
        let fixture = tempfile::tempdir().unwrap();
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let stub = bin.join("ssh");
        fs::write(&stub, STUB).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
        let home = fixture.path().join("home");
        fs::create_dir(&home).unwrap();
        let output = orbit_common::process::run_bounded(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .current_dir(&home)
                .env(CHILD, row)
                .env("HOME", &home)
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .env("ORBIT_STUB_STDERR", stderr)
                .env("ORBIT_STUB_EXIT", exit.to_string())
                .env("ORBIT_STUB_AT", at)
                .env("ORBIT_STUB_EXPECT", expected),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{row}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn exercise_child() {
    let row = std::env::var(CHILD).unwrap();
    let expected = std::env::var("ORBIT_STUB_EXPECT").unwrap();
    let probe = SshDestinationProbe::new(
        "hm_follower".to_string(),
        Duration::from_secs(10),
        Duration::from_secs(10),
        None,
        McpSessionAuthority::Agent,
    );
    let destination = Destination::ssh("orbit-owner", "hm_owner");
    let error = match probe.open_route(&destination) {
        Err(error) => error,
        Ok(mut route) => route
            .call_tool(
                "orbit.task.add",
                json!({"title": "a write"}),
                ToolSessionContext::default(),
            )
            .expect_err(&format!(
                "{row}: the destination exits instead of answering"
            )),
    };
    let message = error.to_string();
    match expected.as_str() {
        "lock_busy" => {
            assert!(error.is_lock_busy(), "{row}: {error:?}");
            assert!(
                message.contains("hm_owner") && message.contains("ssh stderr:"),
                "{row}: the failure names the host and quotes its diagnostic: {message}"
            );
        }
        "unreachable" => assert!(
            matches!(error, OrbitError::UnreachableDestination(_)),
            "{row}: {error:?}"
        ),
        "outcome_unknown" => assert!(
            matches!(error, OrbitError::OutcomeUnknown { .. }),
            "{row}: a dispatched call whose session ended stays ambiguous: {error:?}"
        ),
        other => panic!("{row}: unknown expectation {other}"),
    }
}
