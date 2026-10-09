//! `GET /api/doctor`: the `orbit doctor` report for the selected workspace,
//! read-only and cached.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const DOCTOR: &str = "/api/doctor?workspace=ws_http_fixture";

/// The dashboard rows are the CLI's rows: same checks in the same order, the
/// same fields and the same verdicts, with each failing check's full message
/// and remediation.
#[cfg(unix)]
#[test]
fn doctor_returns_the_cli_report_rows_with_remediation() {
    isolated(
        "doctor::doctor_returns_the_cli_report_rows_with_remediation",
        || {
            let fixture = Fixture::new();
            // A forced warning: a lock file whose recorded holder is dead.
            let lock = plant_stale_lock(&fixture);
            let server = fixture.server(false);

            let body = json_ok(server.get(DOCTOR));
            let checks = body["checks"].as_array().expect("doctor rows");
            let expected = orbit_cmd::run_doctor_report(&fixture.runtime, false)
                .iter()
                .map(orbit_cmd::doctor_row_json)
                .collect::<Vec<_>>();

            assert_eq!(
                names(checks),
                names(&expected),
                "the dashboard runs the same checks, in the same order, as `orbit doctor`"
            );
            let fields = ["check", "duration_ms", "message", "remediation", "status"]
                .into_iter()
                .collect::<BTreeSet<_>>();
            for (row, cli) in checks.iter().zip(&expected) {
                let keys = row
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                assert_eq!(keys, fields, "{row}");
                assert!(row["duration_ms"].is_u64(), "{row}");
                assert_eq!(row["status"], cli["status"], "{row} vs {cli}");
                assert_eq!(row["remediation"], cli["remediation"], "{row} vs {cli}");
                // Free space moves between the two runs; every other message is stable.
                if row["check"] != "disk-space" {
                    assert_eq!(row["message"], cli["message"], "{row} vs {cli}");
                }
            }

            let stale = row(checks, "stale-locks");
            assert_eq!(stale["status"], "warning", "{stale}");
            assert!(
                stale["message"]
                    .as_str()
                    .unwrap()
                    .contains(&lock.display().to_string()),
                "the full message names the lock: {stale}"
            );
            assert_eq!(
                stale["remediation"],
                "Run `orbit doctor --fix-stale-locks`."
            );
            // The fixture seeds no executor definitions, so a routed crew's
            // provider row fails.
            assert!(
                checks.iter().any(|row| row["status"] == "error"
                    && row["check"].as_str().unwrap().starts_with("provider:")),
                "{body}"
            );
            for failing in checks
                .iter()
                .filter(|row| row["status"] == "error" || row["status"] == "warning")
            {
                assert!(
                    failing["remediation"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty()),
                    "a failing check carries its remediation: {failing}"
                );
            }

            let count = |status: &str| checks.iter().filter(|row| row["status"] == status).count();
            assert_eq!(body["failures"], json!(count("error")));
            assert_eq!(body["warnings"], json!(count("warning")));
            assert!(body["ran_at"].is_string(), "{body}");
            assert!(body["duration_ms"].is_u64(), "{body}");
        },
    );
}

/// The poll's peek never runs doctor; a plain read reuses the last report;
/// only an explicit refresh runs it again.
#[test]
fn doctor_runs_on_demand_and_serves_the_cached_report_with_its_age() {
    isolated(
        "doctor::doctor_runs_on_demand_and_serves_the_cached_report_with_its_age",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);

            let peek = json_ok(server.get(&format!("{DOCTOR}&cached=true")));
            assert_eq!(peek["checks"], Value::Null, "a peek never runs doctor");
            assert_eq!(peek["ran_at"], Value::Null);

            let first = json_ok(server.get(DOCTOR));
            assert!(first["checks"].is_array(), "{first}");
            std::thread::sleep(Duration::from_millis(50));

            let reused = json_ok(server.get(DOCTOR));
            assert_eq!(reused["ran_at"], first["ran_at"], "a plain read reuses");
            assert!(
                reused["age_ms"].as_u64().unwrap() >= 50,
                "a reused report reports its age: {reused}"
            );
            let peeked = json_ok(server.get(&format!("{DOCTOR}&cached=true")));
            assert_eq!(peeked["ran_at"], first["ran_at"]);

            let refreshed = json_ok(server.get(&format!("{DOCTOR}&refresh=true")));
            assert_ne!(refreshed["ran_at"], first["ran_at"], "refresh runs again");
            assert!(
                refreshed["age_ms"].as_u64().unwrap() < reused["age_ms"].as_u64().unwrap(),
                "{refreshed}"
            );
            assert_eq!(
                json_ok(server.get(DOCTOR))["ran_at"],
                refreshed["ran_at"],
                "later reads serve the refreshed report"
            );
        },
    );
}

/// No dashboard request can reach a doctor repair or `--deep`: repair and
/// depth parameters are refused, other methods are not routed, and a stale
/// lock the report flags survives every attempt.
#[cfg(unix)]
#[test]
fn no_dashboard_request_runs_a_doctor_repair() {
    isolated("doctor::no_dashboard_request_runs_a_doctor_repair", || {
        let fixture = Fixture::new();
        let lock = plant_stale_lock(&fixture);
        let holder = fs::read_to_string(&lock).unwrap();
        let server = fixture.server(true);

        for flag in [
            "fix_stale_locks",
            "fix-stale-locks",
            "fix_stale_task_locks",
            "fix_stale_artifacts",
            "fix_orphan_task_stores",
            "fix_automation_pins",
            "fix_retired_activity_backends",
            "remove_graph",
            "confirm",
            "deep",
            "fix",
        ] {
            let response = server.get(&format!("{DOCTOR}&refresh=true&{flag}=true"));
            assert_eq!(response.status().as_u16(), 400, "{flag} must be refused");
            let refusal: Value = response.json().unwrap();
            assert!(
                refusal["error"]
                    .as_str()
                    .is_some_and(|error| error.contains(flag)),
                "the refusal names the parameter: {refusal}"
            );
        }
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let response = server
                .request(method, DOCTOR)
                .header("origin", &server.origin)
                .json(&json!({"fix_stale_locks": true, "confirm": true, "deep": true}))
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 405, "{method} /api/doctor");
        }

        let body = json_ok(server.get(&format!("{DOCTOR}&refresh=true")));
        let checks = body["checks"].as_array().unwrap();
        let stale = row(checks, "stale-locks");
        assert_eq!(stale["status"], "warning", "{stale}");
        assert_eq!(
            stale["remediation"], "Run `orbit doctor --fix-stale-locks`.",
            "the repair is offered as a CLI command, not run"
        );
        assert!(
            !checks
                .iter()
                .any(|row| row["check"].as_str().unwrap().starts_with("fix-")),
            "no repair row: {body}"
        );
        assert_eq!(
            fs::read_to_string(&lock).unwrap(),
            holder,
            "the dead holder record survives every dashboard request"
        );
    });
}

fn names(rows: &[Value]) -> Vec<&str> {
    rows.iter()
        .map(|row| row["check"].as_str().unwrap())
        .collect()
}

fn row<'a>(rows: &'a [Value], check: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["check"] == check)
        .unwrap_or_else(|| panic!("no {check} row"))
}

/// A lock file whose holder record names a process that has exited.
#[cfg(unix)]
fn plant_stale_lock(fixture: &Fixture) -> PathBuf {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let lock = fixture.runtime.paths().state_dir.join(".crashed-op.lock");
    let holder = json!({"pid": pid, "acquired_at": "2026-10-03T00:00:00Z", "label": "crashed op"});
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, holder.to_string()).unwrap();
    lock
}
