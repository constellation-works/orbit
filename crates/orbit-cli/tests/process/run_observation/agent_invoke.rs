//! Agent invocation observation through the built CLI and fake provider.

use super::*;

const AGENT_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn operator_json(fixture: &Fixture, args: &[&str]) -> Value {
    let output = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .env_remove("RUST_LOG")
        .args(args)
        .timeout(AGENT_COMMAND_TIMEOUT)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

const AGENT_TEST_ANSWER: &str = r#"{"schemaVersion":1,"status":"success","result":{"summary":"fixture answer","findings":["fixture evidence"],"next_steps":["fixture action"],"details":{"ok":true}},"error":null}"#;

fn plant_invoke_provider(fixture: &Fixture, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = fixture.home.join("empty-bin/codex");
    fs::write(&path, format!("#!/bin/sh\n/bin/cat > /dev/null\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn agent_wait_redacts_sensitive_warning_content_in_progress_and_output() {
    let fixture = Fixture::init();
    plant_invoke_provider(&fixture, &format!("printf '%s\\n' '{AGENT_TEST_ANSWER}'"));
    let submission = operator_json(
        &fixture,
        &[
            "run",
            "agent",
            "probe",
            "--wait",
            "--timeout",
            "30s",
            "--provider-sandbox",
            "danger-full-access",
            "--json",
        ],
    );
    // Mark an observed warning as a sensitive env value to exercise the real
    // CLI boundary without pinning its prose or injecting production warnings.
    let sensitive = submission["warnings"][0].as_str().unwrap();
    for as_json in [false, true] {
        let mut command = fixture.orbit();
        command
            .env("ORBIT_OPERATOR", "1")
            .env("RUST_LOG", "off")
            .env("ORBIT_TEST_WARNING_SECRET", sensitive)
            .args([
                "run",
                "agent",
                "probe",
                "--wait",
                "--timeout",
                "30s",
                "--provider-sandbox",
                "danger-full-access",
            ]);
        if as_json {
            command.arg("--json");
        }
        let output = command
            .timeout(std::time::Duration::from_secs(30))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        for rendered in [&stdout, &stderr] {
            assert!(
                !rendered.contains(sensitive),
                "submission warnings must not expose sensitive env values: {rendered}"
            );
            assert!(rendered.contains("[REDACTED_ENV]"), "{rendered}");
        }
        if as_json {
            let result: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(result["state"], "success");
            assert_eq!(result["answer"]["summary"], "fixture answer");
            assert_eq!(result["provider_sandbox"], "codex:danger-full-access");
            assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
            assert!(
                result["warnings"][0]
                    .as_str()
                    .unwrap()
                    .contains("[REDACTED_ENV]")
            );
        } else {
            assert!(stdout.contains("fixture answer"), "{stdout}");
        }
    }
}

#[test]
fn agent_wait_prints_the_answer_and_exits_nonzero_for_failed_invocations() {
    let fixture = Fixture::init();
    plant_invoke_provider(&fixture, &format!("printf '%s\\n' '{AGENT_TEST_ANSWER}'"));
    let answer = operator_json(
        &fixture,
        &[
            "run",
            "agent",
            "probe",
            "--wait",
            "--timeout",
            "30s",
            "--json",
        ],
    );
    assert_eq!(answer["state"], "success", "{answer}");
    assert_eq!(answer["waited"], true);
    assert_eq!(answer["answer"]["summary"], "fixture answer");
    assert_eq!(
        answer["answer"]["findings"],
        serde_json::json!(["fixture evidence"])
    );
    assert_eq!(
        answer["answer"]["next_steps"],
        serde_json::json!(["fixture action"])
    );
    assert_eq!(
        answer["answer"]["extra"],
        serde_json::json!({"details":{"ok":true}})
    );
    assert_eq!(answer["answer"]["final_message"], AGENT_TEST_ANSWER);
    let text = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .env_remove("RUST_LOG")
        .args(["run", "agent", "probe", "--wait", "--timeout", "30s"])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&text).contains("fixture evidence"),
        "the human wait view prints the answer"
    );

    for (body, timeout, reason, expected_log) in [
        (
            "printf '%s\\n' 'no envelope'",
            "30",
            "response envelope",
            Some("no envelope"),
        ),
        ("/bin/sleep 30", "1", "wall-clock timeout", None),
    ] {
        plant_invoke_provider(&fixture, body);
        let output = fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .env_remove("RUST_LOG")
            .args([
                "run",
                "agent",
                "probe",
                "--wait",
                "--timeout",
                timeout,
                "--json",
            ])
            .timeout(AGENT_COMMAND_TIMEOUT)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "a failed invocation must fail --wait: {output:?}"
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["waited"], true, "{result}");
        assert!(
            result["agent_invocation"]["failure_reason"]
                .as_str()
                .unwrap()
                .contains(reason),
            "{result}"
        );
        assert_eq!(result["answer"], Value::Null);
        let run_id = result["run_id"].as_str().unwrap();
        assert_eq!(fixture.run_state(run_id), "failed", "{result}");
        // ORB-14397: the old five-second assertion deadline killed this new
        // CLI process under full-suite load, even after --wait finished the run.
        // Allow the same startup budget as the agent commands; the provider's
        // one-second timeout above still exercises invocation termination.
        let logs = fixture
            .orbit()
            .env_remove("RUST_LOG")
            .args(["run", "logs", run_id, "--follow", "--json"])
            .timeout(AGENT_COMMAND_TIMEOUT)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let mut captured = String::new();
        for line in String::from_utf8(logs).unwrap().lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["run_id"], run_id, "{record}");
            captured.push_str(record["text"].as_str().unwrap());
        }
        if let Some(expected_log) = expected_log {
            assert!(captured.contains(expected_log), "{captured}");
        }
    }
}

#[test]
fn agent_timeout_admission_and_engine_use_the_same_ceiling() {
    let fixture = Fixture::init();
    plant_invoke_provider(&fixture, &format!("printf '%s\\n' '{AGENT_TEST_ANSWER}'"));
    // Compare the public ceiling with the activity the fixture actually loads,
    // then inspect the engine's dispatch event, not the source text.
    let path = fixture
        .home
        .join(".orbit/resources/activities/agent_invoke.yaml");
    let asset = orbit_engine::activity_job::load_activity_catalog_asset(
        &path,
        &fs::read_to_string(&path).unwrap(),
        false,
    )
    .unwrap()
    .unwrap();
    let orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec
    else {
        panic!("agent invocation must load as an agent loop");
    };
    let ceiling = orbit_core::application::job::MAX_AGENT_INVOKE_TIMEOUT_SECONDS;
    assert_eq!(
        spec.wall_clock_timeout_seconds, ceiling,
        "API and loaded activity must agree on the execution ceiling"
    );
    for timeout in [None, Some(2400), Some(ceiling)] {
        let mut args = vec!["run", "agent", "probe", "--wait", "--json"];
        let seconds = timeout.map(|seconds| seconds.to_string());
        if let Some(seconds) = &seconds {
            args.extend(["--timeout", seconds]);
        }
        let result = operator_json(&fixture, &args);
        let expected =
            timeout.unwrap_or(orbit_core::application::job::DEFAULT_AGENT_INVOKE_TIMEOUT_SECONDS);
        assert_eq!(result["timeout_seconds"], expected);
        let trace = operator_json(
            &fixture,
            &["run", "trace", result["run_id"].as_str().unwrap(), "--json"],
        );
        fn timeouts(value: &Value, found: &mut Vec<u64>) {
            match value {
                Value::Object(fields) => {
                    if let Some(ms) = fields.get("wall_clock_timeout_ms").and_then(Value::as_u64) {
                        found.push(ms);
                    }
                    for value in fields.values() {
                        timeouts(value, found);
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        timeouts(value, found);
                    }
                }
                _ => {}
            }
        }
        let mut actual = Vec::new();
        timeouts(&trace, &mut actual);
        assert_eq!(
            actual,
            vec![expected * 1000],
            "the engine enforces the bound the API admitted: {trace}"
        );
    }
    let before: i64 = fixture
        .db()
        .query_row("SELECT count(*) FROM job_runs", [], |row| row.get(0))
        .unwrap();
    for seconds in [0, ceiling + 1] {
        let output = fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .env_remove("RUST_LOG")
            .args([
                "run",
                "agent",
                "probe",
                "--timeout",
                &seconds.to_string(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error: Value = serde_json::from_slice(
            &output.stderr[String::from_utf8_lossy(&output.stderr).find('{').unwrap()..],
        )
        .unwrap();
        assert_eq!(
            error["code"], "invalid_input",
            "invalid bounds must be refused by admission: {error}"
        );
    }
    let after: i64 = fixture
        .db()
        .query_row("SELECT count(*) FROM job_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, after, "invalid bounds must not submit a run");
}

#[test]
fn agent_submissions_report_saturation_and_their_queue_position() {
    let fixture = Fixture::init();
    let limit = operator_json(
        &fixture,
        &["job", "show", "agent_invoke_pipeline", "--json"],
    )["max_active_runs"]
        .as_u64()
        .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    // A live unrelated process owns each seeded slot; reconciliation must not
    // turn saturation into an orphan fixture. No cancellation targets that PID.
    for index in 0..limit {
        fixture.db().execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid) VALUES (?1, ?2, 'agent_invoke_pipeline', 1, 'running', ?3, ?3, ?3, ?4)",
            params![format!("jrun-occupied-{index}"), fixture.workspace_id(), now, std::process::id()],
        ).unwrap();
    }
    let mut queued_runs = Vec::new();
    for position in 1..=2 {
        let queued = operator_json(
            &fixture,
            &[
                "run",
                "agent",
                "queued probe",
                "--provider-sandbox",
                "read-only",
                "--json",
            ],
        );
        assert_eq!(queued["queued"], true, "{queued}");
        assert_eq!(queued["state"], "queued");
        assert_eq!(queued["queue_position"], position);
        assert!(!queued["warnings"].as_array().unwrap().is_empty());
        queued_runs.push(queued["run_id"].as_str().unwrap().to_string());
    }
    for run_id in queued_runs {
        operator_json(&fixture, &["run", "cancel", &run_id, "--confirm", "--json"]);
    }
}

#[test]
fn run_logs_follow_emits_live_output_once_and_stops_at_terminal() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let fixture = Fixture::init();
    let release = fixture.work.join("release-provider");
    test_env::create_fixture_fifo(&release).unwrap();
    let live = serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"live fixture line"}}).to_string();
    let final_frame = serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":AGENT_TEST_ANSWER}}).to_string();
    plant_invoke_provider(
        &fixture,
        &format!(
            "printf '%s\\n' '{live}'\nprintf '%s\\n' 'live diagnostic' >&2\nread -r _ < '{}'\nprintf '%s\\n' '{final_frame}'",
            release.display()
        ),
    );
    let submitted = operator_json(
        &fixture,
        &["run", "agent", "follow probe", "--timeout", "30", "--json"],
    );
    let run_id = submitted["run_id"].as_str().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    // Own the child before any assertion so every panic path reaps it.
    let mut child = crate::child_guard::ChildGuard::new(
        command
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env_remove("RUST_LOG")
            .args(["run", "logs", run_id, "--follow", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::sync_channel(16);
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first = receive.recv_timeout(Duration::from_secs(15));
    // Release on assertion failures too: never leave a fixture provider alive.
    let state_before_release = fixture.run_state(run_id);
    test_env::release_fixture_fifo(&release, Instant::now() + Duration::from_secs(15)).unwrap();
    let first = first.expect("--follow must emit before the provider finishes");
    let first: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["run_id"], run_id);
    assert_eq!(
        state_before_release, "running",
        "output must stream while the provider is still running"
    );
    let mut records = vec![first];
    while let Ok(line) = receive.recv_timeout(Duration::from_secs(15)) {
        records.push(serde_json::from_str(&line).unwrap());
    }
    // Bound the exit wait and kill before joining: the reader only ends at EOF,
    // so joining first would hang on a follow that never stops.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            reader.join().unwrap();
            panic!("--follow did not stop at terminal");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    reader.join().unwrap();
    assert!(status.success());
    assert_eq!(
        fixture.run_state(run_id),
        "success",
        "{}",
        operator_json(&fixture, &["run", "show", run_id, "--json"])
    );
    for expected in [
        &format!("{live}\n"),
        "live diagnostic\n",
        &format!("{final_frame}\n"),
    ] {
        assert_eq!(
            records
                .iter()
                .filter(|record| record["text"] == expected)
                .count(),
            1,
            "follow must emit each line once: {records:?}"
        );
    }
    // A terminal invocation is still readable with a step filter in follow mode.
    let logs = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .env_remove("RUST_LOG")
        .args([
            "run", "logs", run_id, "--follow", "--step", "invoke", "--format", "ndjson",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!logs.is_empty());
    for line in String::from_utf8(logs).unwrap().lines() {
        assert_eq!(
            serde_json::from_str::<Value>(line).unwrap()["run_id"],
            run_id
        );
    }
    // A rotated or disabled live feed must not hide the retained capture.
    let feed = fixture.home.join(".orbit/state/logs/orbit-agent.jsonl");
    fs::rename(&feed, feed.with_file_name("rotated.jsonl")).unwrap();
    let fallback = fixture
        .orbit()
        .env("RUST_LOG", "warn")
        .args(["run", "logs", run_id, "--follow", "--json"])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(fallback)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["stream"] == "stdout")
        .map(|record| record["text"].as_str().unwrap().to_string())
        .collect::<String>();
    assert_eq!(
        stdout,
        format!("{live}\n{final_frame}\n"),
        "follow must recover the complete retained stdout when tracing is unavailable"
    );
}

#[cfg(unix)]
#[test]
fn run_logs_follow_exits_cleanly_when_merged_output_reader_closes_early() {
    let fixture = Fixture::init();
    // Keep the captured stderr larger than a pipe buffer so `head` closes its
    // read end while `--follow` is still writing the retained capture.
    plant_invoke_provider(
        &fixture,
        &format!("printf '%2097152s\\n' x >&2\nprintf '%s\\n' '{AGENT_TEST_ANSWER}'"),
    );
    let submitted = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .env_remove("RUST_LOG")
        .env("ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES", "4194304")
        .args([
            "run",
            "agent",
            "pipe probe",
            "--wait",
            "--timeout",
            "30",
            "--json",
        ])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .output()
        .unwrap();
    assert!(submitted.status.success(), "{submitted:?}");
    let submission: Value = serde_json::from_slice(&submitted.stdout).unwrap();
    let run_id = submission["run_id"].as_str().unwrap();
    assert_eq!(fixture.run_state(run_id), "success", "{submission}");

    let mut shell = Command::new("/bin/bash");
    test_env::clear_inherited_authority(|name| {
        shell.env_remove(name);
    });
    let output = shell
        .current_dir(&fixture.work)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES", "4194304")
        .env("ORBIT_BINARY", env!("CARGO_BIN_EXE_orbit"))
        .env("ORBIT_RUN_ID", run_id)
        .args([
            "-o",
            "pipefail",
            "-c",
            "\"$ORBIT_BINARY\" run logs \"$ORBIT_RUN_ID\" --follow 2>&1 | head -c 1",
        ])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "a closed merged output pipe must end follow successfully, without a panic: {output:?}"
    );
    assert_eq!(
        output.stdout.len(),
        1,
        "the reader must receive one byte before closing: {output:?}"
    );
}
