//! `orbit run profiles` through the real CLI: the per-profile table, the full
//! per-minute series under `--json`, the no-diff build flag naming the
//! deliveries behind it, and `--accuracy` pairing recorded admission
//! forecasts with the measured host minutes.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::{DateTime, DurationRound, SecondsFormat, Utc};
use orbit_common::test_env;
use rusqlite::{Connection, params};
use serde_json::Value;

const GIB: i64 = 1024 * 1024 * 1024;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).unwrap();
        crate::git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "profiles"])
            .assert()
            .success();
        // Open, and so create, the host store before seeding it.
        fixture
            .orbit()
            .args(["run", "history", "--no-reconcile", "--json"])
            .assert()
            .success();
        fixture
    }

    fn orbit(&self) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("RUST_LOG", "off");
        command.timeout(Duration::from_secs(60));
        command
    }

    fn stdout(&self, args: &[&str]) -> String {
        let output = self
            .orbit()
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap()
    }

    /// Seed one finished single-run delivery admitted `hours_ago`, with one
    /// row per minute at `cores` and 1 GiB.
    fn delivery(
        &self,
        run_id: &str,
        task_id: &str,
        class: &str,
        complexity: &str,
        hours_ago: i64,
        cores: &[f64],
    ) {
        let db = Connection::open(self.home.join(".orbit/orbit.db")).unwrap();
        let admitted = Utc::now() - chrono::Duration::hours(hours_ago);
        let stamp = |minutes: i64| {
            (admitted + chrono::Duration::minutes(minutes))
                .to_rfc3339_opts(SecondsFormat::Micros, true)
        };
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, \
                 scheduled_at, created_at, finished_at) \
             VALUES (?1, 'ws_fixture', 'task_auto_pipeline', 1, 'success', ?2, ?2, ?3)",
            params![run_id, stamp(0), stamp(cores.len() as i64)],
        )
        .unwrap();
        for (index, cpu) in cores.iter().enumerate() {
            db.execute(
                "INSERT INTO run_resource_minutes (workspace_id, run_id, minute_index, \
                     minute_started_at, task_id, work_class, complexity, resumed, \
                     delivery_admitted_at, cpu_cores, memory_bytes, samples) \
                 VALUES ('ws_fixture', ?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, 12)",
                params![
                    run_id,
                    index as i64,
                    stamp(index as i64),
                    task_id,
                    class,
                    complexity,
                    stamp(0),
                    cpu,
                    GIB
                ],
            )
            .unwrap();
        }
    }
}

impl Fixture {
    fn db(&self) -> Connection {
        Connection::open(self.home.join(".orbit/orbit.db")).unwrap()
    }

    /// Record one admission pass's forecast: `(offset, cores, GiB)` points.
    #[allow(clippy::too_many_arguments)]
    fn forecast(
        &self,
        run_id: &str,
        mode: &str,
        at: DateTime<Utc>,
        points: &[(u32, f64, i64)],
        admitted: &[&str],
        fit_admitted: &[&str],
        fit_skipped: &[&str],
    ) {
        let points: Vec<Value> = points
            .iter()
            .map(|(offset, cores, gib)| {
                serde_json::json!({
                    "offset_minutes": offset,
                    "cpu_cores": cores,
                    "memory_bytes": gib * GIB,
                })
            })
            .collect();
        self.db()
            .execute(
                "INSERT INTO admission_forecasts (workspace_id, run_id, forecast_at, mode, \
                     cpu_count, memory_total_bytes, points_json, admitted_json, \
                     fit_admitted_json, fit_skipped_json) \
                 VALUES ('ws_fixture', ?1, ?2, ?3, 16, ?4, ?5, ?6, ?7, ?8)",
                params![
                    run_id,
                    at.to_rfc3339_opts(SecondsFormat::Micros, true),
                    mode,
                    32 * GIB,
                    serde_json::to_string(&points).unwrap(),
                    serde_json::to_string(admitted).unwrap(),
                    serde_json::to_string(fit_admitted).unwrap(),
                    serde_json::to_string(fit_skipped).unwrap(),
                ],
            )
            .unwrap();
    }

    /// Record the host's measured minute at `at`.
    fn host_minute(&self, at: DateTime<Utc>, cores: f64, gib: i64) {
        self.db()
            .execute(
                "INSERT INTO host_resource_minutes (minute_at, cpu_cores, cpu_count, \
                     memory_bytes, memory_total_bytes, samples, recorded_by_run_id) \
                 VALUES (?1, ?2, 16, ?3, ?4, 12, 'jrun-worker')",
                params![
                    at.to_rfc3339_opts(SecondsFormat::Micros, true),
                    cores,
                    gib * GIB,
                    32 * GIB
                ],
            )
            .unwrap();
    }
}

#[test]
fn profiles_report_tabulates_profiles_emits_series_and_flags_building_no_diff_work() {
    let fixture = Fixture::new();
    let empty: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--json"])).unwrap();
    assert_eq!(empty["profiles"], serde_json::json!([]));

    fixture.delivery("jrun-a", "ORB-1", "change", "hard", 5, &[2.0, 2.0, 2.0]);
    fixture.delivery("jrun-b", "ORB-2", "change", "hard", 4, &[2.0, 2.0, 2.0]);
    fixture.delivery(
        "jrun-review",
        "ORB-3",
        "no-diff",
        "medium",
        3,
        &[0.2, 2.5, 3.0],
    );

    let doc: Value = serde_json::from_str(&fixture.stdout(&["run", "profiles", "--json"])).unwrap();
    let profiles = doc["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2, "{doc:#}");
    let change = profiles
        .iter()
        .find(|profile| profile["key"]["work_class"] == "change")
        .unwrap();
    assert_eq!(change["key"]["complexity"], "hard");
    assert_eq!(change["delivery_count"], 2);
    assert_eq!(change["sparse"], true);
    assert_eq!(change["horizon_minutes"], 3);
    let series = change["series"].as_array().unwrap();
    assert_eq!(series.len(), 3);
    for point in series {
        assert_eq!(point["alive"], 1.0);
        assert_eq!(point["cpu_cores"], 2.0);
        assert_eq!(point["memory_bytes"], GIB);
    }
    assert!(change["build_like"].as_array().unwrap().is_empty());
    let review = profiles
        .iter()
        .find(|profile| profile["key"]["work_class"] == "no-diff")
        .unwrap();
    let flagged = review["build_like"].as_array().unwrap();
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0]["task_id"], "ORB-3");
    assert_eq!(flagged[0]["run_ids"], serde_json::json!(["jrun-review"]));
    assert_eq!(flagged[0]["from_minute"], 1);

    let plain = fixture.stdout(&["run", "profiles", "--format", "plain"]);
    let rows: Vec<Vec<&str>> = plain
        .lines()
        .filter(|line| line.starts_with("ws_fixture\t"))
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(
        rows,
        vec![
            vec![
                "ws_fixture",
                "change",
                "hard",
                "2 (sparse)",
                "3m",
                "2.0 cores @ +0m",
                "1.0 GiB @ +0m",
                "cpu 0.00 mem 0.00",
                "-",
            ],
            vec![
                "ws_fixture",
                "no-diff",
                "medium",
                "1 (sparse)",
                "3m",
                "3.0 cores @ +2m",
                "1.0 GiB @ +0m",
                "cpu 0.00 mem 0.00",
                "build-like (1)",
            ],
        ],
        "{plain}"
    );
    assert!(
        plain
            .lines()
            .any(|line| line.trim() == "ORB-3 from +1m, peak 3.0 cores (runs jrun-review)"),
        "the flag names the delivery behind it: {plain}"
    );
}

/// `--accuracy` pairs each recorded forecast point with the measured host
/// minute it forecast, summarizes the error per offset for CPU and memory,
/// counts a point whose minute is unmeasured, and counts shadow decisions
/// against actual admissions per drain.
#[test]
fn accuracy_pairs_forecasts_with_measured_minutes_and_counts_shadow_decisions() {
    let fixture = Fixture::new();
    let empty: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--accuracy", "--json"]))
            .unwrap();
    assert_eq!(empty["forecasts"], 0);
    assert_eq!(empty["pairs"], serde_json::json!([]));

    let minute = (Utc::now() - chrono::Duration::hours(2))
        .duration_trunc(chrono::Duration::minutes(1))
        .unwrap();
    let at = |minutes: i64| minute + chrono::Duration::minutes(minutes);
    // A shadow pass at 20 s past the minute forecasts +5, +10, +20 and +30;
    // the +30 minute is never measured.
    fixture.forecast(
        "jrun-shadow",
        "shadow",
        minute + chrono::Duration::seconds(20),
        &[(5, 4.0, 8), (10, 2.0, 4), (20, 1.0, 2), (30, 1.0, 2)],
        &["ORB-1", "ORB-2"],
        &["ORB-2", "ORB-3"],
        &["ORB-1"],
    );
    // An enforce pass a minute later, measured at +5 only.
    fixture.forecast(
        "jrun-enforce",
        "enforce",
        at(1),
        &[(5, 3.0, 1)],
        &["ORB-4"],
        &["ORB-4"],
        &[],
    );
    fixture.host_minute(at(5), 6.0, 6);
    fixture.host_minute(at(6), 2.0, 1);
    fixture.host_minute(at(10), 1.0, 4);
    fixture.host_minute(at(20), 1.5, 1);

    let doc: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--accuracy", "--json"]))
            .unwrap();
    assert_eq!(doc["forecasts"], 2, "{doc:#}");
    let pairs = doc["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 4, "{doc:#}");
    let first = &pairs[0];
    assert_eq!(first["run_id"], "jrun-shadow");
    assert_eq!(first["offset_minutes"], 5);
    let paired_minute: DateTime<Utc> = first["minute_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(paired_minute, at(5));
    assert_eq!(first["forecast_cpu_cores"], 4.0);
    assert_eq!(first["measured_cpu_cores"], 6.0);
    assert_eq!(first["forecast_memory_bytes"], 8 * GIB);
    assert_eq!(first["measured_memory_bytes"], 6 * GIB);

    let offsets = doc["offsets"].as_array().unwrap();
    let offset = |minutes: u64| {
        offsets
            .iter()
            .find(|offset| offset["offset_minutes"] == minutes)
            .unwrap()
    };
    // +5: CPU errors -2 and +1 cores; memory errors +2 GiB and 0.
    let five = offset(5);
    assert_eq!(five["paired"], 2);
    assert_eq!(five["unmeasured"], 0);
    assert_eq!(five["cpu"]["mean_error"], -0.5);
    assert_eq!(five["cpu"]["mean_absolute_error"], 1.5);
    assert_eq!(five["cpu"]["under_rate"], 0.5);
    assert_eq!(five["cpu"]["worst_miss"]["error"], -2.0);
    assert_eq!(five["cpu"]["worst_miss"]["run_id"], "jrun-shadow");
    assert_eq!(five["memory"]["mean_error"], GIB as f64);
    assert_eq!(five["memory"]["under_rate"], 0.0);
    assert_eq!(five["memory"]["worst_miss"]["error"], 2.0 * GIB as f64);
    // +20: CPU under by half a core; memory over by 1 GiB.
    let twenty = offset(20);
    assert_eq!(twenty["cpu"]["mean_error"], -0.5);
    assert_eq!(twenty["cpu"]["under_rate"], 1.0);
    assert_eq!(twenty["memory"]["under_rate"], 0.0);
    let thirty = offset(30);
    assert_eq!(thirty["paired"], 0);
    assert_eq!(thirty["unmeasured"], 1);
    assert_eq!(thirty["cpu"]["mean_error"], Value::Null);

    let drains = doc["drains"].as_array().unwrap();
    assert_eq!(drains.len(), 2, "{doc:#}");
    let drain = |run_id: &str| {
        drains
            .iter()
            .find(|drain| drain["run_id"] == run_id)
            .unwrap()
    };
    let shadow = drain("jrun-shadow");
    assert_eq!(shadow["shadow_passes"], 1);
    assert_eq!(shadow["admitted"], 2);
    assert_eq!(shadow["would_hold"], serde_json::json!(["ORB-1"]));
    assert_eq!(shadow["would_add"], serde_json::json!(["ORB-3"]));
    let enforce = drain("jrun-enforce");
    assert_eq!(enforce["enforce_passes"], 1);
    assert_eq!(enforce["shadow_passes"], 0);
    assert_eq!(enforce["would_hold"], serde_json::json!([]));

    let plain = fixture.stdout(&["run", "profiles", "--accuracy", "--format", "plain"]);
    let drain_rows: Vec<Vec<&str>> = plain
        .lines()
        .filter(|line| line.starts_with("ws_fixture\t"))
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(
        drain_rows,
        vec![
            vec!["ws_fixture", "jrun-enforce", "0", "1", "0", "0", "0"],
            vec!["ws_fixture", "jrun-shadow", "1", "0", "2", "1", "1"],
        ],
        "{plain}"
    );
}
