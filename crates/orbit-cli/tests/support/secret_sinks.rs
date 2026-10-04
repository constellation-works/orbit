//! One seeded fixture crosses the CLI, persistence, provider and dashboard boundaries.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use serde_json::{Value, json};

use super::Fixture;
use crate::fixture_crew;

const TEST: &str =
    "plugin_secrets::secret_sinks::seeded_secrets_stay_out_of_persistence_logs_and_children";
const CHILD: &str = "ORBIT_SECRET_SINK_FIXTURE";
const SECRETS: &[(&str, &str)] = &[
    ("E2E_PROVIDER_TOKEN", "sk-orbitSeedProvider0123456789abcdef"),
    ("E2E_SCM_TOKEN", "ghp_0123456789abcdefghijklmnopqrstuvwxyz"),
    ("E2E_CLOUD_SECRET", "AKIA0123456789ABCDEF"),
    ("E2E_DATABASE_PASSWORD", "orbitSeedPassword7f3a9c1e5b"),
];
// These have no ambient environment entry, so env-value masking cannot cover
// a regression in the credential-pattern redactor.
const PATTERN_SECRETS: &[(&str, &str)] = &[
    (
        "provider pattern",
        "sk-orbitPatternProviderabcdef0123456789",
    ),
    ("SCM pattern", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
    ("cloud pattern", "AKIAFEDCBA9876543210"),
    (
        "connection password pattern",
        "orbitPatternPassword9c1e5b7f3a",
    ),
];

#[test]
fn seeded_secrets_stay_out_of_persistence_logs_and_children() {
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let home = tempfile::tempdir().expect("isolated child home");
        let mut command = Command::new(std::env::current_exe().expect("integration binary"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, TEST)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env_remove("ORBIT_LOG_PATH")
            .envs(SECRETS.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let output = orbit_exec::supervise_child(command.spawn().unwrap(), Some(120_000), None)
            .expect("bounded isolated fixture")
            .result;
        assert!(
            output.success,
            "isolated secret-sink fixture: {}\n{}",
            output.stdout, output.stderr
        );
        assert!(output.stdout.contains("test result: ok. 1 passed;"));
        return;
    }

    // Seeds are in the process environment before any redactor cache is initialized.
    for (name, value) in SECRETS {
        assert_eq!(std::env::var(name).unwrap(), *value, "ambient seed {name}");
    }
    let fixture = Fixture::new();
    let routing = fixture.json(&["workspace", "show"]);
    assert_eq!(
        routing["checkout"]["repo_root"],
        fs::canonicalize(&fixture.work).unwrap().to_str().unwrap()
    );
    let payload = format!(
        "seeded diagnostic {} {} {} postgres://fixture:{}@localhost/db password={} {} {} {} postgres://pattern:{}@localhost/db",
        SECRETS[0].1,
        SECRETS[1].1,
        SECRETS[2].1,
        SECRETS[3].1,
        SECRETS[3].1,
        PATTERN_SECRETS[0].1,
        PATTERN_SECRETS[1].1,
        PATTERN_SECRETS[2].1,
        PATTERN_SECRETS[3].1
    );
    let mut leaks = Vec::new();

    let added = tool(
        &fixture,
        "orbit.task.add",
        json!({
            "title": "Secret sink fixture", "description": payload,
            "acceptance_criteria": [payload],
            "complexity": "low", "model": "codex"
        }),
    );
    let id = added["id"].as_str().expect("minted fixture task");
    tool(
        &fixture,
        "orbit.task.update",
        json!({
            "id": id, "plan": payload, "execution_summary": payload, "comment": payload, "model": "codex"
        }),
    );
    let shown = tool(
        &fixture,
        "orbit.task.show",
        json!({"id": id, "model": "codex"}),
    );
    check("task read", shown.to_string().as_bytes(), &mut leaks);
    assert!(
        shown["description"]
            .as_str()
            .unwrap()
            .contains("seeded diagnostic")
    );
    assert!(!shown["comments"].as_array().unwrap().is_empty());
    scan(
        "persisted task records",
        &fixture.home.join(".orbit/tasks"),
        &mut leaks,
    );

    let scratch = fixture.work.join(".orbit/tmp");
    fs::create_dir_all(&scratch).unwrap();
    let global = fixture.home.join(".orbit");
    fixture_crew::configure_sol(&global);
    let provider = fixture.work.join("codex");
    let child_env = scratch.join("child-env.txt");
    let response = json!({"schemaVersion": 1, "status": "success",
        "result": {"diagnostic": payload, "nested": {"diagnostics": [payload, 7, true, null]}, "provider_ran": true}, "error": null});
    fs::write(
        &provider,
        format!(
            "#!/bin/sh\ncat >/dev/null\nenv > '{}'\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{}' >&2\n",
            child_env.display(),
            response,
            payload
        ),
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    // The fixture owns this executor; explicit off avoids nesting an OS sandbox.
    fs::write(global.join("resources/executors/codex.yaml"), format!(
        "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: codex\nspec:\n  executor_type: direct_agent\n  command: '{}'\n  args: []\n  sandbox: off\n  env: {{}}\n", provider.display()
    )).unwrap();
    fs::write(global.join("resources/jobs/secret_sinks.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: secret_sinks\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: probe\n      spec:\n        type: agent_loop\n        description: Secret output and environment probe\n        instruction: Return the fixture response\n        provider: codex\n        backend: cli\n        wall_clock_timeout_seconds: 10\n").unwrap();
    let run_output = fixture.run(&["job", "run", "secret_sinks", "--wait", "--json"], None);
    let run: Value = serde_json::from_slice(&run_output.stdout).unwrap();
    let run_id = run["run_id"].as_str().unwrap();
    assert!(
        run_output.status.success(),
        "provider job: {run}; detail: {}; stderr: {}",
        fixture.json(&["run", "show", run_id, "--no-reconcile"]),
        String::from_utf8_lossy(&run_output.stderr)
    );
    assert_eq!(run["state"], "success", "provider job: {run}");
    check("job result", run.to_string().as_bytes(), &mut leaks);
    let result = &run["pipeline"]["probe"];
    assert_eq!(
        result["provider_ran"], true,
        "structured provider result retained"
    );
    let nested = result["nested"]["diagnostics"].as_array().unwrap();
    assert!(nested[0].as_str().unwrap().contains("seeded diagnostic"));
    assert_eq!(&nested[1..], &[json!(7), json!(true), Value::Null]);
    let environment = fs::read_to_string(&child_env).expect("provider actually ran");
    assert!(environment.contains(&format!("HOME={}\n", fixture.home.display())));
    check(
        "provider child environment",
        environment.as_bytes(),
        &mut leaks,
    );
    for (name, _) in SECRETS {
        assert!(
            !environment
                .lines()
                .any(|line| line.starts_with(&format!("{name}="))),
            "provider child environment inherited {name}"
        );
    }
    let logs = fixture.json(&["run", "logs", run_id, "--no-reconcile"]);
    check("CLI run logs", logs.to_string().as_bytes(), &mut leaks);
    assert!(
        logs.to_string().contains("seeded diagnostic"),
        "run logs were exercised: {logs}"
    );
    // Raw attachments preserve bytes by contract. Attach the diagnostic artifact
    // produced by the public run-log path, whose producer must already redact it.
    let source = scratch.join("seeded.json");
    fs::write(&source, logs.to_string()).unwrap();
    tool(
        &fixture,
        "orbit.task.artifact.put",
        json!({
            "id": id, "source_path": source, "path": "seeded.json", "model": "codex"
        }),
    );
    let artifact = tool(
        &fixture,
        "orbit.task.artifact.get",
        json!({
            "id": id, "path": "seeded.json", "model": "codex"
        }),
    );
    assert!(
        artifact["content"]
            .as_str()
            .unwrap()
            .contains("seeded diagnostic")
    );
    check(
        "attached diagnostic artifact",
        artifact["content"].as_str().unwrap().as_bytes(),
        &mut leaks,
    );
    scan(
        "persisted task artifact",
        &fixture.home.join(".orbit/tasks"),
        &mut leaks,
    );
    let events = fixture.json(&["run", "events", run_id, "--no-reconcile"]);
    check(
        "run audit events",
        events.to_string().as_bytes(),
        &mut leaks,
    );
    scan(
        "persisted run output and blobs",
        &fixture.work.join(".orbit/state"),
        &mut leaks,
    );
    let db = rusqlite::Connection::open(global.join("orbit.db")).unwrap();
    for (sink, table) in [
        ("command audit rows", "audit_events"),
        ("run audit rows", "v2_audit_events"),
        ("persisted run state", "job_runs"),
        ("persisted step output", "job_run_steps"),
    ] {
        let mut statement = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        let mut count = 0;
        while let Some(row) = rows.next().unwrap() {
            count += 1;
            for column in 0..columns {
                match row.get_ref(column).unwrap() {
                    rusqlite::types::ValueRef::Text(bytes)
                    | rusqlite::types::ValueRef::Blob(bytes) => {
                        check(sink, bytes, &mut leaks);
                    }
                    _ => {}
                }
            }
        }
        assert!(count > 0, "{sink} must be exercised");
    }

    let log_path = global.join("state/logs/orbit.jsonl");
    check(
        "persisted process log",
        &fs::read(&log_path).expect("CLI tracing log"),
        &mut leaks,
    );
    // A hostile legacy record independently exercises the dashboard's read-time
    // defence; it is input to the renderer, appended after checking producer logs.
    let legacy = json!({"timestamp": "2026-10-03T00:00:00Z", "level": "INFO",
        "target": "orbit.secret_fixture", "fields": {"message": payload, "nested": [payload]}});
    writeln!(
        fs::OpenOptions::new().append(true).open(&log_path).unwrap(),
        "{legacy}"
    )
    .unwrap();
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server_cmd = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        server_cmd.env_remove(name);
    });
    let mut server = Process(
        server_cmd
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .args(["web", "serve", "--port", &port.to_string(), "--no-open"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "dashboard exited before readiness"
        );
        assert!(
            Instant::now() < deadline,
            "dashboard readiness exceeded 10s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let web_logs = get(
        port,
        &format!(
            "/api/runs/{run_id}/logs?workspace={}",
            routing["workspace"]["id"].as_str().unwrap()
        ),
    );
    assert!(
        !web_logs.as_array().unwrap().is_empty(),
        "dashboard run logs exercised"
    );
    assert!(web_logs.to_string().contains("seeded diagnostic"));
    check(
        "dashboard run log view",
        web_logs.to_string().as_bytes(),
        &mut leaks,
    );
    let rendered = get(port, "/api/log?target=orbit.secret_fixture");
    assert_eq!(
        rendered["events"].as_array().unwrap().len(),
        1,
        "legacy log rendered"
    );
    assert!(rendered.to_string().contains("seeded diagnostic"));
    check(
        "dashboard rendered log view",
        rendered.to_string().as_bytes(),
        &mut leaks,
    );
    assert!(
        leaks.is_empty(),
        "seeded secrets leaked to sinks: {}",
        leaks.join("; ")
    );
}

fn tool(fixture: &Fixture, name: &str, input: Value) -> Value {
    let output = fixture.run_ok(&["tool", "run", name, "--input", &input.to_string()]);
    serde_json::from_slice(&output.stdout).expect("tool JSON")
}

fn check(sink: &str, bytes: &[u8], leaks: &mut Vec<String>) {
    for (name, value) in SECRETS.iter().chain(PATTERN_SECRETS) {
        if bytes
            .windows(value.len())
            .any(|window| window == value.as_bytes())
        {
            leaks.push(format!("{sink}: {name}"));
        }
    }
}

fn scan(sink: &str, root: &Path, leaks: &mut Vec<String>) {
    assert!(scan_files(sink, root, leaks) > 0, "{sink} is empty");
}

fn scan_files(sink: &str, root: &Path, leaks: &mut Vec<String>) -> usize {
    let entries =
        fs::read_dir(root).unwrap_or_else(|error| panic!("{sink} at {}: {error}", root.display()));
    let mut files = 0;
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files += scan_files(sink, &path, leaks);
        } else if path.is_file() {
            files += 1;
            check(
                &format!("{sink} ({})", path.display()),
                &fs::read(path).unwrap(),
                leaks,
            );
        }
    }
    files
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn get(port: u16, path: &str) -> Value {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "dashboard {path}: {response}"
    );
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}
