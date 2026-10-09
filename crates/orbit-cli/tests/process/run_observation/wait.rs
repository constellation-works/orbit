//! CLI wait deadlines leave active runs observable under their actual state.

use std::fs;

use serde_json::Value;

use super::{FAILED, Fixture, isolated_run_observation};

#[test]
fn wait_expiry_reports_active_state_without_timing_out_the_run() {
    if !isolated_run_observation(
        "run_observation::wait::wait_expiry_reports_active_state_without_timing_out_the_run",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    // Keep both entry points active regardless of scheduler load. Cancel each
    // worker after observing it so the fixture leaves no detached children.
    for name in ["wait_fixture", "task_pilot_pipeline"] {
        fs::write(
            jobs.join(format!("{name}.yaml")),
            serde_json::json!({
                "schemaVersion": 2, "kind": "Job", "metadata": {"name": name},
                "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                    "id": "nap", "default_input": {"seconds": 300},
                    "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                }]}
            })
            .to_string(),
        )
        .unwrap();
    }
    for args in [
        vec!["run", "job", "wait_fixture"],
        vec!["run", "task-pilot"],
    ] {
        for as_json in [true, false] {
            let mut command = fixture.orbit();
            command
                .args(&args)
                .args(["--wait", "--timeout-seconds", "1"]);
            if as_json {
                command.arg("--json");
            }
            let output = command
                .timeout(std::time::Duration::from_secs(60))
                .assert()
                .code(1)
                .get_output()
                .clone();
            let run_id = if as_json {
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["wait_timeout"], true, "{result}");
                assert!(
                    matches!(result["state"].as_str(), Some("pending" | "running")),
                    "{result}"
                );
                if args[1] == "job" {
                    assert!(result["finished_at"].is_null(), "{result}");
                    assert!(result["error"].is_null(), "{result}");
                } else {
                    assert!(result["error_code"].is_null(), "{result}");
                    assert!(result["error_message"].is_null(), "{result}");
                }
                result["run_id"].as_str().unwrap().to_string()
            } else {
                let text = String::from_utf8(output.stdout).unwrap();
                assert!(text.contains("wait_timeout=true"), "{text}");
                if args[1] == "job" {
                    assert!(
                        text.contains("State: pending") || text.contains("State: running"),
                        "{text}"
                    );
                    text.lines()
                        .find_map(|line| line.strip_prefix("Run ID: "))
                        .unwrap()
                        .to_string()
                } else {
                    assert!(
                        text.contains("state=pending;") || text.contains("state=running;"),
                        "{text}"
                    );
                    text.split(';')
                        .find_map(|field| field.strip_prefix("run_id="))
                        .unwrap()
                        .to_string()
                }
            };
            assert!(matches!(
                fixture.run_state(&run_id).as_str(),
                "pending" | "running"
            ));
            let shown = fixture.json(&["run", "show", &run_id, "--no-reconcile", "--json"]);
            assert!(shown["run"]["finished_at"].is_null(), "{shown}");
            fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
            assert_eq!(fixture.run_state(&run_id), "cancelled");
        }
    }
}

#[test]
fn invalid_wait_deadline_does_not_submit_work() {
    if !isolated_run_observation(
        "run_observation::wait::invalid_wait_deadline_does_not_submit_work",
    ) {
        return;
    }
    let fixture = Fixture::init();
    // Runtime open may reconcile the fixture's pre-seeded stale runs. Finish
    // that normal repair before comparing invalid submissions for mutations.
    fixture.json(&["run", "history", "--json"]);
    let before = fixture.snapshot();
    for args in [
        vec!["run", "job", "task_pilot_pipeline"],
        vec!["run", "task-pilot"],
        vec!["job", "resume", FAILED],
    ] {
        let mut command = fixture.orbit();
        let output = command
            .args(&args)
            .args(["--wait", "--timeout-seconds", "21601", "--json"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(error.to_string().contains("must be <= 21600"), "{error}");
        fixture
            .orbit()
            .args(&args)
            .args(["--timeout-seconds", "1"])
            .assert()
            .code(2);
    }
    assert_eq!(
        fixture.snapshot(),
        before,
        "invalid wait options must not submit any runs"
    );
}
