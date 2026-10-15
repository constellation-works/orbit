#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Audit behavior through the real CLI, with disposable machine/workspace state.

use std::fs;
use std::path::PathBuf;

use orbit_common::test_env;
use orbit_core::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

#[cfg(unix)]
#[path = "../support/child_guard.rs"]
mod child_guard;

mod native_reads;

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let fixture = Self {
            home: temp.path().join("home"),
            repo: temp.path().join("repo"),
            root: temp.path().join("state"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).unwrap();
        fs::create_dir_all(&fixture.repo).unwrap();
        let output = crate::git_repo::command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(output.status.success());
        fixture
            .command(&[
                "init",
                "--non-interactive",
                "--machine-name",
                "audit-qa",
                "--task-prefix",
                "AQ",
            ])
            .assert()
            .success();
        fixture
            .command(&["workspace", "init", "--name", "audit-qa"])
            .assert()
            .success();
        fixture.json(&[
            "task",
            "add",
            "--title",
            "Audit fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Persist audit evidence",
            "--json",
        ]);
        fixture
    }

    fn command(&self, args: &[&str]) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.std_command(args))
    }

    fn std_command(&self, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            // The tracing feed is host-global, not root-scoped: the first
            // event passing the file filter creates `$HOME/.orbit/state/logs`.
            // On a loaded host a healthy command emits one (a task-commit
            // section held past its 2 s threshold), and an inherited
            // `RUST_LOG` can lower the filter, so a fixture that asserts its
            // HOME stays empty has to switch the feed off.
            .env("RUST_LOG", "off")
            .arg("--root")
            .arg(&self.root)
            .args(args);
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .command(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap_or_else(|error| {
            panic!(
                "JSON for {args:?}: {error}; {}",
                String::from_utf8_lossy(&output)
            )
        })
    }

    fn task_events(&self) -> Value {
        self.json(&[
            "audit", "list", "--kind", "task", "--status", "success", "--json",
        ])
    }

    fn command_events(&self, command: &str, subcommand: &str) -> Vec<Value> {
        self.json(&["audit", "list", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["command"] == command && row["subcommand"] == subcommand)
            .cloned()
            .collect()
    }
}

/// Re-executes `test` in a child process with its own HOME, after `configure`
/// has adjusted the child's environment, and reports whether this call was
/// that parent. The child (identified by the marker variable) returns `false`
/// and runs the test body.
fn run_in_child(test: &str, configure: impl FnOnce(&mut std::process::Command)) -> bool {
    const CHILD: &str = "ORBIT_AUDIT_EXPORT_FIXTURE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return false;
    }
    let home = tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    child
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    configure(&mut child);
    let output = test_env::run_child_test(&mut child, test, home.path());
    test_env::assert_child_test_passed(test, output.status, &output.stdout, &output.stderr);
    true
}

#[test]
fn audit_cli_missing_log_records_the_render_failure() {
    const TEST: &str = "audit_cli::audit_cli_missing_log_records_the_render_failure";
    if run_in_child(TEST, |_| {}) {
        return;
    }
    let fixture = Fixture::new();
    // A non-default basename avoids the split operational/agent feed behavior.
    let path = fixture.repo.join("missing-explicit.jsonl");
    let output = fixture
        .command(&["log", "tail", "--path", path.to_str().unwrap(), "--json"])
        .timeout(test_env::FIXTURE_STEP_DEADLINE)
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains(path.to_str().unwrap())
    );
    let rows = fixture.command_events("log", "tail");
    assert_eq!(rows.len(), 1, "one audit row for the failed invocation");
    assert_eq!(rows[0]["status"], "failure");
    assert_eq!(rows[0]["exit_code"], output.status.code().unwrap());
    assert_eq!(rows[0]["error_message"], error["error"]);
}

#[cfg(unix)]
#[test]
fn audit_cli_follow_stream_records_final_outcome_and_elapsed_duration() {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const TEST: &str =
        "audit_cli::audit_cli_follow_stream_records_final_outcome_and_elapsed_duration";
    if run_in_child(TEST, |_| {}) {
        return;
    }
    // Both a late read failure and a successful closed pipe must finalize the
    // same guard, after the stream has spent time following a live file.
    for close_pipe in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.repo.join("explicit-follow.jsonl");
        let record = json!({"level": "INFO", "fields": {"message": "ready"}});
        fs::write(&path, format!("{record}\n")).unwrap();
        let mut stderr = tempfile::tempfile().unwrap();
        let mut command = fixture.std_command(&[
            "log",
            "tail",
            "--path",
            path.to_str().unwrap(),
            "--follow",
            "--json",
        ]);
        let mut child = child_guard::ChildGuard::new(
            command
                .stdout(Stdio::piped())
                .stderr(stderr.try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let mut line = String::new();
            let result = stdout.read_line(&mut line);
            let _ = tx.send((stdout, line, result));
        });
        let (stdout, line, read) = rx
            .recv_timeout(test_env::FIXTURE_STEP_DEADLINE)
            .unwrap_or_else(|error| panic!("stream readiness: {error}; {}", test_env::host_load()));
        reader.join().unwrap();
        assert!(read.unwrap() > 0, "stream exited before emitting readiness");
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), record);
        assert!(child.try_wait().unwrap().is_none());

        let db = Connection::open(fixture.root.join("orbit.db")).unwrap();
        let completed: i64 = db
            .query_row(
                "SELECT count(*) FROM audit_events WHERE command = 'log' AND pid = ?1",
                [child.id()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(completed, 0, "a live stream has no completed command row");
        drop(db);

        let held = Instant::now();
        // This is an intentional idle interval after observed readiness, not
        // a race window or a startup deadline. The audit must include it.
        std::thread::sleep(Duration::from_millis(150));
        let held_ms = held.elapsed().as_millis() as u64;
        if close_pipe {
            drop(stdout);
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(file, "{record}").unwrap();
        } else {
            // The open descriptor keeps the old file valid. On the next EOF
            // poll the replacement directory deterministically fails reading.
            fs::rename(&path, fixture.repo.join("retained.jsonl")).unwrap();
            fs::create_dir(&path).unwrap();
        }
        let mut status = None;
        test_env::wait_until("log stream termination", || {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        let status = status.unwrap();
        stderr.seek(SeekFrom::Start(0)).unwrap();
        let mut errors = String::new();
        stderr.read_to_string(&mut errors).unwrap();
        assert_eq!(
            status.code(),
            Some(if close_pipe { 0 } else { 1 }),
            "{errors}"
        );
        let rows = fixture.command_events("log", "tail");
        assert_eq!(rows.len(), 1, "one final row per stream: {rows:?}");
        let row = &rows[0];
        assert_eq!(row["pid"], child.id());
        assert_eq!(row["exit_code"], status.code().unwrap());
        assert_eq!(
            row["status"],
            if close_pipe { "success" } else { "failure" }
        );
        assert!(
            row["duration_ms"].as_u64().unwrap() >= held_ms,
            "audit duration must include time spent streaming: {row}"
        );
        if close_pipe {
            assert!(
                errors.is_empty(),
                "closed pipe must remain silent: {errors}"
            );
            assert!(row["error_message"].is_null());
        } else {
            let error: Value = serde_json::from_str(&errors).unwrap();
            assert_eq!(row["error_message"], error["error"]);
            assert!(!row["error_message"].as_str().unwrap().is_empty());
        }
    }
}

#[cfg(unix)]
#[test]
fn audit_cli_closed_stdout_preserves_the_success_row_for_human_rendering() {
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;

    const TEST: &str =
        "audit_cli::audit_cli_closed_stdout_preserves_the_success_row_for_human_rendering";
    if run_in_child(TEST, |_| {}) {
        return;
    }
    let fixture = Fixture::new();
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let mut command = fixture.std_command(&["task", "list", "--format", "plain"]);
    let mut child = child_guard::ChildGuard::new(
        command
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut status = None;
    test_env::wait_until("human rendering with closed stdout", || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    let mut errors = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut errors)
        .unwrap();
    assert!(status.unwrap().success(), "{errors}");
    assert!(errors.is_empty(), "{errors}");
    let rows = fixture.command_events("task", "list");
    assert_eq!(
        rows.len(),
        1,
        "closed stdout still finalizes the command audit"
    );
    assert_eq!(rows[0]["status"], "success");
    assert_eq!(rows[0]["exit_code"], 0);
    assert!(rows[0]["error_message"].is_null());
}

#[test]
fn audit_cli_round_trips_real_mutation_filters_stats_and_export() {
    const TEST: &str = "audit_cli::audit_cli_round_trips_real_mutation_filters_stats_and_export";
    if run_in_child(TEST, |_| {}) {
        return;
    }
    let fixture = Fixture::new();
    let events = fixture.task_events();
    let rows = events.as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "successful task creation must leave audit evidence"
    );
    assert!(
        rows.iter()
            .any(|row| row["command"] == "task" && row["subcommand"] == "add")
    );
    assert!(
        rows.iter()
            .all(|row| row["status"] == "success" && row["target_type"] == "task")
    );
    let event = &rows[0];
    let id = event["id"].as_i64().unwrap().to_string();
    assert_eq!(fixture.json(&["audit", "show", &id, "--json"]), *event);
    let empty_metadata = json!({
        "self_reported_actor": null,
        "plugin": null,
        "plugin_secrets": [],
        "plugin_secret_updates": {},
        "brokered": false,
        "peer_pid": null,
    });
    assert_fields(event, &empty_metadata);

    let workspace = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_id = workspace["workspace"]["id"].as_str().unwrap();
    let scoped = fixture.json(&["audit", "list", "--workspace-id", workspace_id, "--json"]);
    assert!(
        scoped
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["workspace_id"] == workspace_id)
    );
    assert_eq!(
        fixture.json(&["audit", "list", "--workspace-id", "ws_absent", "--json"]),
        serde_json::json!([])
    );
    assert_eq!(
        fixture
            .json(&["audit", "list", "--limit", "1", "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let stats = fixture.json(&["audit", "stats", "--json"]);
    assert!(stats["total"].as_u64().unwrap() >= rows.len() as u64);
    assert!(stats["success_count"].as_u64().unwrap() >= rows.len() as u64);
    let filtered = fixture.json(&["audit", "stats", "--tool", "absent.tool", "--json"]);
    assert_eq!(filtered["total"], 0);
    assert_eq!(filtered["denied_by_operation"], serde_json::json!([]));

    // Seed persisted evidence independently of the CLI JSON projection, so a
    // field omitted by every CLI surface still fails this test.
    let mut expected = json!({
        "execution_id": "audit-plugin-fixture",
        "command": "tool",
        "tool_name": "audit-fixture.echo",
        "role": "unverified",
        "status": "success",
        "self_reported_actor": "client, \"claimed\"",
        "plugin": {
            "name": "audit-fixture",
            "version": "1.2.3",
            "manifest_digest": "sha256:fixture-manifest",
            "grants": ["net:example.invalid", "secret:api_token"],
        },
        "plugin_secrets": ["api_token", "refresh_token"],
        "plugin_secret_updates": {"api_token": "refused", "refresh_token": "applied"},
        "brokered": true,
        "peer_pid": 4242,
    });
    let db = Connection::open(fixture.root.join("orbit.db")).unwrap();
    db.execute(
        "INSERT INTO audit_events (
            execution_id, timestamp, command, subcommand, tool_name, role,
            status, exit_code, duration_ms, working_directory, pid,
            self_reported_actor, plugin_name, plugin_version, plugin_manifest_digest,
            plugin_grants, plugin_secrets, plugin_secret_updates, brokered, peer_pid
        ) VALUES (?1, ?2, 'tool', 'run-mcp', ?3, 'unverified', 'success', 0, 1, ?4, 1234,
                  ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, 4242)",
        params![
            expected["execution_id"].as_str().unwrap(),
            chrono::Utc::now().to_rfc3339(),
            expected["tool_name"].as_str().unwrap(),
            fixture.repo.to_str().unwrap(),
            expected["self_reported_actor"].as_str().unwrap(),
            expected["plugin"]["name"].as_str().unwrap(),
            expected["plugin"]["version"].as_str().unwrap(),
            expected["plugin"]["manifest_digest"].as_str().unwrap(),
            expected["plugin"]["grants"].to_string(),
            expected["plugin_secrets"].to_string(),
            expected["plugin_secret_updates"].to_string(),
        ],
    )
    .unwrap();
    expected["id"] = json!(db.last_insert_rowid());
    drop(db);

    // Values exist in the separate host secret store, while the audit row
    // deliberately carries only names and rotation outcomes.
    const SECRET: &str = "audit-export-secret-value-3bd481";
    let secrets = PluginSecretStore::new(&fixture.root);
    for name in ["api_token", "refresh_token"] {
        secrets
            .put(
                "audit-fixture",
                name,
                &PluginSecretValue::new(SECRET.to_string()).unwrap(),
            )
            .unwrap();
    }
    let plugin_id = expected["id"].as_i64().unwrap().to_string();
    let shown = fixture.json(&["audit", "show", &plugin_id, "--json"]);
    assert_fields(&shown, &expected);
    assert!(!shown.to_string().contains(SECRET));
    let listed = fixture.json(&["audit", "list", "--tool", "audit-fixture.echo", "--json"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_fields(&listed[0], &expected);
    assert!(!listed.to_string().contains(SECRET));

    let export = fixture.repo.join("audit.json");
    fixture
        .command(&[
            "audit",
            "export",
            "--format",
            "json",
            "--output",
            export.to_str().unwrap(),
        ])
        .assert()
        .success();
    let export_bytes = fs::read(export).unwrap();
    assert!(!String::from_utf8_lossy(&export_bytes).contains(SECRET));
    let exported: Value = serde_json::from_slice(&export_bytes).unwrap();
    let exported_plugin = exported
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == expected["id"])
        .unwrap();
    assert_fields(exported_plugin, &expected);
    assert_fields(
        exported
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == event["id"])
            .unwrap(),
        &empty_metadata,
    );
    let csv_export = fixture.repo.join("audit.csv");
    fixture
        .command(&[
            "audit",
            "export",
            "--format",
            "csv",
            "--output",
            csv_export.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(!fs::read_to_string(&csv_export).unwrap().contains(SECRET));
    let mut csv = csv::Reader::from_path(csv_export).unwrap();
    let headers = csv.headers().unwrap().clone();
    let id_column = headers.iter().position(|column| column == "id").unwrap();
    let records: Vec<_> = csv.records().map(Result::unwrap).collect();
    let plugin_row = records
        .iter()
        .find(|record| record.get(id_column) == Some(plugin_id.as_str()))
        .unwrap();
    let cell = |record: &csv::StringRecord, name: &str| {
        let column = headers.iter().position(|header| header == name).unwrap();
        record.get(column).unwrap().to_string()
    };
    for field in ["plugin", "plugin_secrets", "plugin_secret_updates"] {
        let value: Value = serde_json::from_str(&cell(plugin_row, field)).unwrap();
        assert_eq!(value, expected[field], "CSV must preserve {field}");
    }
    assert_eq!(cell(plugin_row, "brokered"), "true");
    assert_eq!(cell(plugin_row, "peer_pid"), "4242");
    assert_eq!(
        cell(plugin_row, "self_reported_actor"),
        expected["self_reported_actor"].as_str().unwrap()
    );
    let legacy_row = records
        .iter()
        .find(|record| record.get(id_column) == Some(id.as_str()))
        .unwrap();
    for (field, value) in [
        ("self_reported_actor", ""),
        ("plugin", "null"),
        ("plugin_secrets", "[]"),
        ("plugin_secret_updates", "{}"),
        ("brokered", "false"),
        ("peer_pid", ""),
    ] {
        assert_eq!(cell(legacy_row, field), value, "legacy CSV field {field}");
    }
    assert!(
        fs::read_dir(&fixture.home).unwrap().next().is_none(),
        "explicit root must keep isolated HOME untouched"
    );
}

fn assert_fields(row: &Value, expected: &Value) {
    for (field, value) in expected.as_object().unwrap() {
        assert_eq!(row.get(field), Some(value), "audit field {field}");
    }
}

#[test]
fn audit_cli_refusals_preserve_existing_events_and_export_bytes() {
    let fixture = Fixture::new();
    let before = fixture.task_events();
    fixture
        .command(&["audit", "show", "9223372036854775807", "--json"])
        .assert()
        .failure();
    fixture
        .command(&["audit", "list", "--since", "not-a-duration", "--json"])
        .assert()
        .failure();
    fixture
        .command(&["audit", "prune", "--older-than", "0s"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure();
    assert_eq!(
        fixture.task_events(),
        before,
        "unconfirmed prune must preserve all matching task events"
    );

    let export = fixture.repo.join("retained.json");
    fs::write(&export, b"retained evidence\n").unwrap();
    fixture
        .command(&[
            "audit",
            "export",
            "--since",
            "not-a-duration",
            "--output",
            export.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert_eq!(
        fs::read(export).unwrap(),
        b"retained evidence\n",
        "invalid export arguments must be validated before opening the destination"
    );
    // Confirmation alone does not grant authority to destroy audit evidence.
    fixture
        .command(&["audit", "prune", "--older-than", "0s", "--confirm"])
        .assert()
        .failure();
    assert_eq!(fixture.task_events(), before);
    let output = fixture
        .command(&[
            "audit",
            "prune",
            "--older-than",
            "0s",
            "--confirm",
            "--format",
            "json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let pruned: Value = serde_json::from_slice(&output).unwrap();
    assert!(pruned["pruned"].as_u64().unwrap() >= before.as_array().unwrap().len() as u64);
    assert_eq!(
        fixture.task_events(),
        serde_json::json!([]),
        "confirmed prune must remove matching persisted events"
    );
}
