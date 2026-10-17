//! `orbit run profiles` through the real CLI: the per-profile table, the full
//! per-minute series under `--json` without deliveries whose CPU accounting
//! is superseded, the no-diff build flag naming the deliveries behind it, and
//! `--accuracy` pairing recorded admission forecasts with the measured host
//! minutes.

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
        self.delivery_with_accounting(
            run_id,
            task_id,
            class,
            complexity,
            hours_ago,
            cores,
            "linux-cgroup-v1",
        );
    }

    /// [`Self::delivery`] with its minutes stamped `cpu_accounting`.
    #[allow(clippy::too_many_arguments)]
    fn delivery_with_accounting(
        &self,
        run_id: &str,
        task_id: &str,
        class: &str,
        complexity: &str,
        hours_ago: i64,
        cores: &[f64],
        cpu_accounting: &str,
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
                     delivery_admitted_at, cpu_cores, memory_bytes, samples, cpu_accounting) \
                 VALUES ('ws_fixture', ?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, 12, ?10)",
                params![
                    run_id,
                    index as i64,
                    stamp(index as i64),
                    task_id,
                    class,
                    complexity,
                    stamp(0),
                    cpu,
                    GIB,
                    cpu_accounting
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

    fn measurements(&self) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        ["admission_forecasts", "host_resource_minutes"]
            .into_iter()
            .map(|table| {
                let db = self.db();
                let mut query = db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                    .unwrap();
                let columns = query.column_count();
                query
                    .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap()
            })
            .collect()
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

    /// Insert one job run. `started` is absent when the worker never started.
    fn job_run(
        &self,
        run_id: &str,
        job_id: &str,
        state: &str,
        started: Option<DateTime<Utc>>,
        finished: Option<DateTime<Utc>>,
    ) {
        let stamp = |at: DateTime<Utc>| at.to_rfc3339_opts(SecondsFormat::Micros, true);
        let created = started.or(finished).unwrap_or_else(Utc::now);
        self.db()
            .execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, \
                     scheduled_at, started_at, finished_at, created_at) \
                 VALUES (?1, 'ws_fixture', ?2, 1, ?3, ?4, ?5, ?6, ?4)",
                params![
                    run_id,
                    job_id,
                    state,
                    stamp(created),
                    started.map(stamp),
                    finished.map(stamp),
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

/// Calibration must select the intersection of time, drain and recorded
/// mode, including decisions and errors, without rewriting its evidence.
#[test]
fn accuracy_selects_a_cohort_and_rejects_invalid_selectors_without_mutation() {
    if crate::isolated_test::run_in_child(
        "run_profiles::accuracy_selects_a_cohort_and_rejects_invalid_selectors_without_mutation",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let since = (Utc::now() - chrono::Duration::hours(3))
        .duration_trunc(chrono::Duration::minutes(1))
        .unwrap();
    let selected_at = since + chrono::Duration::seconds(20);
    for (run, mode, at, cpu) in [
        (
            "jrun-cohort",
            "shadow",
            since - chrono::Duration::seconds(1),
            90.0,
        ),
        ("jrun-other", "shadow", selected_at, 80.0),
        ("jrun-cohort", "enforce", selected_at, 70.0),
        ("jrun-cohort", "shadow", selected_at, 4.0),
        // Inclusive lower bound, with a different selected error.
        ("jrun-cohort", "shadow", since, 6.0),
        (
            "jrun-expired",
            "shadow",
            since - chrono::Duration::days(31),
            60.0,
        ),
    ] {
        fixture.forecast(run, mode, at, &[(5, cpu, 8)], &["ORB-1"], &[], &["ORB-1"]);
    }
    fixture.host_minute(since + chrono::Duration::minutes(5), 5.0, 6);
    let before = fixture.measurements();
    let stamp = since.to_rfc3339();
    let args = [
        "run",
        "profiles",
        "--accuracy",
        "--since",
        &stamp,
        "--drain",
        "jrun-cohort",
        "--mode",
        "shadow",
    ];
    let mut json_args = args.to_vec();
    json_args.push("--json");
    let report: Value = serde_json::from_str(&fixture.stdout(&json_args)).unwrap();
    assert_eq!(report["forecasts"], 2, "{report:#}");
    assert_eq!(report["selection"]["drain"], "jrun-cohort");
    assert_eq!(report["selection"]["mode"], "shadow");
    assert_eq!(report["selection"]["since"], report["since"]);
    assert_eq!(
        report["since"]
            .as_str()
            .unwrap()
            .parse::<DateTime<Utc>>()
            .unwrap(),
        since
    );
    let pairs = report["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(
        pairs
            .iter()
            .all(|p| p["run_id"] == "jrun-cohort" && p["mode"] == "shadow")
    );
    let five = &report["offsets"][0];
    assert_eq!(five["cpu"]["mean_error"], 0.0);
    assert_eq!(five["cpu"]["worst_miss"]["error"], 1.0);
    assert_eq!(five["memory"]["mean_error"], 2.0 * GIB as f64);
    let drains = report["drains"].as_array().unwrap();
    assert_eq!(drains.len(), 1);
    assert_eq!(drains[0]["shadow_passes"], 2);
    assert_eq!(drains[0]["enforce_passes"], 0);
    assert_eq!(drains[0]["admitted"], 1);
    assert_eq!(drains[0]["would_hold"], serde_json::json!(["ORB-1"]));

    let mut plain_args = args.to_vec();
    plain_args.extend(["--format", "plain"]);
    let plain = fixture.stdout(&plain_args);
    assert!(
        plain.contains("jrun-cohort") && plain.contains("shadow") && plain.contains(&stamp),
        "{plain}"
    );
    assert!(
        !plain.contains("jrun-other") && !plain.contains("jrun-expired"),
        "{plain}"
    );

    let legacy: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--accuracy", "--json"]))
            .unwrap();
    assert_eq!(legacy["forecasts"], 5);
    assert_eq!(legacy["selection"]["drain"], Value::Null);
    assert_eq!(legacy["selection"]["mode"], Value::Null);
    let report_at: DateTime<Utc> = legacy["report_at"].as_str().unwrap().parse().unwrap();
    let lower: DateTime<Utc> = legacy["since"].as_str().unwrap().parse().unwrap();
    assert_eq!(report_at - lower, chrono::Duration::days(30));
    for selectors in [
        vec!["--drain", "jrun-absent"],
        vec!["--since", "2999-01-01T00:00:00Z"],
    ] {
        // An unmatched drain is empty; a reversed time range is invalid.
        let mut command = fixture.orbit();
        command
            .args(["run", "profiles", "--accuracy", "--json"])
            .args(&selectors);
        if selectors[0] == "--drain" {
            let out = command.assert().success().get_output().stdout.clone();
            let empty: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(empty["forecasts"], 0);
            assert_eq!(empty["pairs"], serde_json::json!([]));
            assert_eq!(empty["drains"], serde_json::json!([]));
        } else {
            command.assert().failure();
        }
    }
    for selectors in [
        vec!["--since", "not-a-time"],
        vec!["--mode", "off"],
        vec!["--drain", ""],
        vec!["--mode", "SHADOW"],
    ] {
        fixture
            .orbit()
            .args(["run", "profiles", "--accuracy"])
            .args(selectors)
            .assert()
            .failure();
    }
    for selectors in [
        ["--since", &stamp],
        ["--drain", "jrun-cohort"],
        ["--mode", "shadow"],
    ] {
        fixture
            .orbit()
            .args(["run", "profiles"])
            .args(selectors)
            .assert()
            .failure();
    }
    assert_eq!(
        fixture.measurements(),
        before,
        "accuracy reads preserve recorded evidence"
    );
}

#[test]
fn profiles_report_tabulates_profiles_emits_series_and_flags_building_no_diff_work() {
    if crate::isolated_test::run_in_child(
        "run_profiles::profiles_report_tabulates_profiles_emits_series_and_flags_building_no_diff_work",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let empty: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--json"])).unwrap();
    assert_eq!(empty["profiles"], serde_json::json!([]));

    fixture.delivery("jrun-a", "ORB-1", "change", "hard", 5, &[2.0, 2.0, 2.0]);
    fixture.delivery("jrun-b", "ORB-2", "change", "hard", 4, &[2.0, 2.0, 2.0]);
    // Superseded macOS accounting that missed reaped descendants stays out
    // of the profile it would otherwise join.
    fixture.delivery_with_accounting(
        "jrun-own-only",
        "ORB-4",
        "change",
        "hard",
        2,
        &[9.0, 9.0, 9.0],
        "macos-mach-ns-v1",
    );
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
    assert_eq!(
        change["delivery_count"], 2,
        "the own-only macOS delivery is excluded: {change:#}"
    );
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
    if crate::isolated_test::run_in_child(
        "run_profiles::accuracy_pairs_forecasts_with_measured_minutes_and_counts_shadow_decisions",
    ) {
        return;
    }
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
    // Three pull requests: a shadow claim, a shadow idle response and an
    // enforce-held request. Only requests sent under shadow would be held.
    for (mode, sent, admitted, minute) in [
        ("shadow", true, vec!["ORB-5"], 2),
        ("shadow", true, vec![], 3),
        ("enforce", false, vec![], 4),
    ] {
        fixture.forecast(
            "jrun-pull",
            mode,
            at(minute),
            &[(5, 7.0, 9)],
            &admitted,
            &[],
            &[],
        );
        fixture.db().execute(
            "UPDATE admission_forecasts SET pull_decision_json = ?1 WHERE run_id = 'jrun-pull' AND forecast_at = ?2",
            params![serde_json::json!({"sent": sent, "forecast_no_headroom": true}).to_string(),
                at(minute).to_rfc3339_opts(SecondsFormat::Micros, true)],
        ).unwrap();
        fixture.host_minute(at(minute + 5), 7.0, 9);
    }
    fixture.host_minute(at(5), 6.0, 6);
    fixture.host_minute(at(6), 2.0, 1);
    fixture.host_minute(at(10), 1.0, 4);
    fixture.host_minute(at(20), 1.5, 1);

    let doc: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--accuracy", "--json"]))
            .unwrap();
    assert_eq!(doc["forecasts"], 5, "{doc:#}");
    let pairs = doc["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 7, "{doc:#}");
    let first = &pairs[0];
    assert_eq!(first["run_id"], "jrun-shadow");
    assert_eq!(first["offset_minutes"], 5);
    let paired_minute: DateTime<Utc> = first["minute_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(paired_minute, at(5));
    assert_eq!(first["forecast_cpu_cores"], 4.0);
    assert_eq!(first["measured_cpu_cores"], 6.0);
    assert_eq!(first["forecast_memory_bytes"], 8 * GIB);
    assert_eq!(first["measured_memory_bytes"], 6 * GIB);
    let pull_pairs: Vec<_> = pairs
        .iter()
        .filter(|pair| pair["run_id"] == "jrun-pull")
        .collect();
    assert_eq!(pull_pairs.len(), 3);
    for pair in pull_pairs {
        assert_eq!(pair["offset_minutes"], 5);
        assert_eq!(pair["forecast_cpu_cores"], 7.0);
        assert_eq!(pair["measured_cpu_cores"], 7.0);
        assert_eq!(pair["forecast_memory_bytes"], 9 * GIB);
        assert_eq!(pair["measured_memory_bytes"], 9 * GIB);
    }

    let offsets = doc["offsets"].as_array().unwrap();
    let offset = |minutes: u64| {
        offsets
            .iter()
            .find(|offset| offset["offset_minutes"] == minutes)
            .unwrap()
    };
    // +5: CPU errors -2 and +1 cores, then three exact pull predictions;
    // memory errors +2 GiB followed by four exact predictions.
    let five = offset(5);
    assert_eq!(five["paired"], 5);
    assert_eq!(five["unmeasured"], 0);
    assert_eq!(five["pending"], 0);
    assert_eq!(five["missing"], 0);
    assert_eq!(five["cpu"]["mean_error"], -0.2);
    assert_eq!(five["cpu"]["mean_absolute_error"], 0.6);
    assert_eq!(five["cpu"]["under_rate"], 0.2);
    assert_eq!(five["cpu"]["worst_miss"]["error"], -2.0);
    assert_eq!(five["cpu"]["worst_miss"]["run_id"], "jrun-shadow");
    assert_eq!(five["memory"]["mean_error"], 2.0 * GIB as f64 / 5.0);
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
    assert_eq!(thirty["pending"], 0);
    assert_eq!(thirty["missing"], 1);
    assert_eq!(thirty["unsampled"], 1);
    assert_eq!(thirty["gap"], 0);
    assert_eq!(thirty["cpu"]["mean_error"], Value::Null);

    let drains = doc["drains"].as_array().unwrap();
    assert_eq!(drains.len(), 3, "{doc:#}");
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
    let pull = drain("jrun-pull");
    assert_eq!(pull["shadow_passes"], 2);
    assert_eq!(pull["enforce_passes"], 1);
    assert_eq!(pull["admitted"], 1);
    assert_eq!(pull["pull_requests"], 3);
    assert_eq!(pull["pull_sent"], 2);
    assert_eq!(pull["pull_would_hold"], 2);
    assert_eq!(pull["would_hold"], serde_json::json!([]));
    assert_eq!(pull["would_add"], serde_json::json!([]));
    let enforce = drain("jrun-enforce");
    assert_eq!(enforce["enforce_passes"], 1);
    assert_eq!(enforce["shadow_passes"], 0);
    assert_eq!(enforce["admitted"], 0);
    assert_eq!(enforce["would_hold"], serde_json::json!([]));
    assert_eq!(enforce["enforce_admitted"], 1);
    assert_eq!(enforce["gap_held"], serde_json::json!([]));
    assert_eq!(enforce["pull_held"], 0);
    assert_eq!(pull["pull_held"], 1);
    assert_eq!(pull["enforce_admitted"], 0);
    assert_eq!(pull["pull_would_hold"], 2);
    assert_eq!(shadow["enforce_admitted"], 0);
    assert_eq!(shadow["gap_held"], serde_json::json!([]));
    assert_eq!(shadow["pull_held"], 0);

    let plain = fixture.stdout(&["run", "profiles", "--accuracy", "--format", "plain"]);
    let drain_rows: Vec<Vec<&str>> = plain
        .lines()
        .filter(|line| line.starts_with("ws_fixture\t"))
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(
        drain_rows,
        vec![
            vec![
                "ws_fixture",
                "jrun-enforce",
                "0",
                "1",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "1",
                "0",
                "0"
            ],
            vec![
                "ws_fixture",
                "jrun-pull",
                "2",
                "1",
                "1",
                "0",
                "0",
                "3",
                "2",
                "2",
                "0",
                "0",
                "1"
            ],
            vec![
                "ws_fixture",
                "jrun-shadow",
                "1",
                "0",
                "2",
                "1",
                "1",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0"
            ],
        ],
        "{plain}"
    );
}

/// Idle target minutes are `unsampled`. A delivery that was live and wrote
/// no host row is a `gap`. A drain, a run that never started, and a run that
/// only touches the minute's boundary stay unsampled. A measured minute stays
/// paired, with the same error, even when a delivery overlaps it.
#[test]
fn accuracy_splits_idle_minutes_from_sampling_gaps() {
    if crate::isolated_test::run_in_child(
        "run_profiles::accuracy_splits_idle_minutes_from_sampling_gaps",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let minute = (Utc::now() - chrono::Duration::hours(3))
        .duration_trunc(chrono::Duration::minutes(1))
        .unwrap();
    let at = |minutes: i64| minute + chrono::Duration::minutes(minutes);
    fixture.forecast(
        "jrun-drain",
        "shadow",
        minute + chrono::Duration::seconds(20),
        &[(5, 4.0, 8), (10, 3.0, 2), (20, 1.0, 1)],
        &[],
        &[],
        &[],
    );
    // Still inside its horizon, so this point is pending rather than a gap.
    fixture.forecast(
        "jrun-drain",
        "shadow",
        Utc::now() - chrono::Duration::seconds(20),
        &[(5, 9.0, 9)],
        &[],
        &[],
        &[],
    );
    fixture.host_minute(at(5), 6.0, 6);
    fixture.job_run(
        "jrun-paired-worker",
        "task_auto_pipeline",
        "success",
        Some(at(5) + chrono::Duration::seconds(1)),
        Some(at(5) + chrono::Duration::seconds(50)),
    );
    fixture.job_run(
        "jrun-gap-worker",
        "task_claimed_pr_pipeline",
        "success",
        Some(at(10) + chrono::Duration::seconds(15)),
        Some(at(10) + chrono::Duration::seconds(45)),
    );
    // Finished as the idle minute started, so it does not cover that minute.
    fixture.job_run(
        "jrun-boundary",
        "task_gate_pipeline",
        "success",
        Some(at(19)),
        Some(at(20)),
    );
    // The drain records forecasts while idle and does not sample the host.
    fixture.job_run(
        "jrun-pull",
        "workspace_pull_pipeline",
        "running",
        Some(at(20)),
        Some(at(21)),
    );
    fixture.job_run(
        "jrun-unstarted",
        "task_local_pipeline",
        "pending",
        None,
        None,
    );

    let doc: Value =
        serde_json::from_str(&fixture.stdout(&["run", "profiles", "--accuracy", "--json"]))
            .unwrap();
    let offsets = doc["offsets"].as_array().unwrap();
    let offset = |minutes: u64| {
        offsets
            .iter()
            .find(|offset| offset["offset_minutes"] == minutes)
            .unwrap()
    };
    let five = offset(5);
    assert_eq!(five["paired"], 1);
    assert_eq!(five["pending"], 1);
    assert_eq!(five["missing"], 0);
    assert_eq!(five["unsampled"], 0);
    assert_eq!(five["gap"], 0);
    assert_eq!(five["unmeasured"], 1);
    assert_eq!(five["cpu"]["mean_error"], -2.0);
    assert_eq!(five["cpu"]["under_rate"], 1.0);
    assert_eq!(five["memory"]["mean_error"], 2.0 * GIB as f64);
    assert_eq!(five["memory"]["under_rate"], 0.0);
    let ten = offset(10);
    assert_eq!(ten["paired"], 0);
    assert_eq!(ten["pending"], 0);
    assert_eq!(ten["missing"], 1);
    assert_eq!(ten["unsampled"], 0);
    assert_eq!(ten["gap"], 1);
    assert_eq!(ten["cpu"]["mean_error"], Value::Null);
    assert_eq!(ten["memory"]["worst_miss"], Value::Null);
    let twenty = offset(20);
    assert_eq!(twenty["paired"], 0);
    assert_eq!(twenty["pending"], 0);
    assert_eq!(twenty["missing"], 1);
    assert_eq!(twenty["unsampled"], 1);
    assert_eq!(twenty["gap"], 0);
    assert_eq!(twenty["cpu"]["mean_error"], Value::Null);
    let thirty = offset(30);
    assert_eq!(thirty["paired"], 0);
    assert_eq!(thirty["pending"], 0);
    assert_eq!(thirty["missing"], 0);
    assert_eq!(thirty["unsampled"], 0);
    assert_eq!(thirty["gap"], 0);

    let plain = fixture.stdout(&["run", "profiles", "--accuracy", "--format", "plain"]);
    let row = |label: &str| {
        plain
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("missing {label} row in {plain}"))
            .split('\t')
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let ten_row = row("+10m\t");
    assert_eq!(
        &ten_row[..6],
        ["+10m", "0 (1 unmeasured)", "0", "1", "0", "1"]
    );
    assert!(ten_row[6..].iter().all(|cell| cell == "-"), "{ten_row:?}");
    let twenty_row = row("+20m\t");
    assert_eq!(
        &twenty_row[..6],
        ["+20m", "0 (1 unmeasured)", "0", "1", "1", "0"]
    );
    let five_row = row("+5m\t");
    assert_eq!(
        &five_row[..6],
        ["+5m", "1 (1 unmeasured)", "1", "0", "0", "0"]
    );
    assert_eq!(five_row[6], "-2.0 cores");
    assert_eq!(five_row[9], "+2.0 GiB");
}

/// Enforce cohorts report the distinct tasks local passes admitted, the
/// distinct tasks gap fit held, and enforce pull holds, while shadow columns
/// stay on shadow passes.
#[test]
fn enforce_accuracy_reports_admitted_tasks_gap_holds_and_pull_holds() {
    if crate::isolated_test::run_in_child(
        "run_profiles::enforce_accuracy_reports_admitted_tasks_gap_holds_and_pull_holds",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let at = (Utc::now() - chrono::Duration::hours(2))
        .duration_trunc(chrono::Duration::minutes(1))
        .unwrap();
    let stamp = |minutes: i64| at + chrono::Duration::minutes(minutes);

    // A shadow pass must stay out of `--mode enforce` and keep its own columns.
    fixture.forecast(
        "jrun-shadow",
        "shadow",
        stamp(0),
        &[(5, 1.0, 1)],
        &["ORB-S"],
        &[],
        &["ORB-S"],
    );
    // Two local enforce passes. ORB-2 is admitted twice and ORB-3 is held
    // twice; each counts once. ORB-4 is held only.
    fixture.forecast(
        "jrun-local",
        "enforce",
        stamp(1),
        &[(5, 2.0, 1)],
        &["ORB-1", "ORB-2"],
        &["ORB-1", "ORB-2"],
        &["ORB-3"],
    );
    fixture.forecast(
        "jrun-local",
        "enforce",
        stamp(2),
        &[(5, 2.0, 1)],
        &["ORB-2"],
        &["ORB-2"],
        &["ORB-3", "ORB-4"],
    );
    // Pull enforce: one sent with room, one held and not sent, one sent and held.
    for (minute, sent, held) in [(3i64, true, false), (4, false, true), (5, true, true)] {
        fixture.forecast(
            "jrun-pull",
            "enforce",
            stamp(minute),
            &[(5, 3.0, 1)],
            &[],
            &[],
            &[],
        );
        fixture
            .db()
            .execute(
                "UPDATE admission_forecasts SET pull_decision_json = ?1 \
                 WHERE run_id = 'jrun-pull' AND forecast_at = ?2",
                params![
                    serde_json::json!({"sent": sent, "forecast_no_headroom": held}).to_string(),
                    stamp(minute).to_rfc3339_opts(SecondsFormat::Micros, true),
                ],
            )
            .unwrap();
    }

    let doc: Value = serde_json::from_str(&fixture.stdout(&[
        "run",
        "profiles",
        "--accuracy",
        "--mode",
        "enforce",
        "--json",
    ]))
    .unwrap();
    assert_eq!(doc["selection"]["mode"], "enforce");
    assert_eq!(doc["forecasts"], 5, "{doc:#}");
    let drains = doc["drains"].as_array().unwrap();
    assert_eq!(drains.len(), 2, "{doc:#}");
    let drain = |run_id: &str| {
        drains
            .iter()
            .find(|drain| drain["run_id"] == run_id)
            .unwrap_or_else(|| panic!("missing {run_id}: {doc:#}"))
    };
    let local = drain("jrun-local");
    assert_eq!(local["shadow_passes"], 0);
    assert_eq!(local["enforce_passes"], 2);
    assert_eq!(local["admitted"], 0);
    assert_eq!(local["would_hold"], serde_json::json!([]));
    assert_eq!(local["would_add"], serde_json::json!([]));
    assert_eq!(local["enforce_admitted"], 2);
    assert_eq!(local["gap_held"], serde_json::json!(["ORB-3", "ORB-4"]));
    assert_eq!(local["pull_requests"], 0);
    assert_eq!(local["pull_sent"], 0);
    assert_eq!(local["pull_would_hold"], 0);
    assert_eq!(local["pull_held"], 0);
    let pull = drain("jrun-pull");
    assert_eq!(pull["enforce_passes"], 3);
    assert_eq!(pull["shadow_passes"], 0);
    assert_eq!(pull["admitted"], 0);
    assert_eq!(pull["enforce_admitted"], 0);
    assert_eq!(pull["gap_held"], serde_json::json!([]));
    assert_eq!(pull["pull_requests"], 3);
    assert_eq!(pull["pull_sent"], 2);
    assert_eq!(pull["pull_would_hold"], 0);
    assert_eq!(pull["pull_held"], 2);

    let plain = fixture.stdout(&[
        "run",
        "profiles",
        "--accuracy",
        "--mode",
        "enforce",
        "--format",
        "plain",
    ]);
    let drain_rows: Vec<Vec<&str>> = plain
        .lines()
        .filter(|line| line.starts_with("ws_fixture\t"))
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(
        drain_rows,
        vec![
            vec![
                "ws_fixture",
                "jrun-local",
                "0",
                "2",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "2",
                "2",
                "0"
            ],
            vec![
                "ws_fixture",
                "jrun-pull",
                "0",
                "3",
                "0",
                "0",
                "0",
                "3",
                "2",
                "0",
                "0",
                "0",
                "2"
            ],
        ],
        "{plain}"
    );

    let shadow: Value = serde_json::from_str(&fixture.stdout(&[
        "run",
        "profiles",
        "--accuracy",
        "--mode",
        "shadow",
        "--json",
    ]))
    .unwrap();
    let shadow_drains = shadow["drains"].as_array().unwrap();
    assert_eq!(shadow_drains.len(), 1, "{shadow:#}");
    assert_eq!(shadow_drains[0]["run_id"], "jrun-shadow");
    assert_eq!(shadow_drains[0]["admitted"], 1);
    assert_eq!(shadow_drains[0]["would_hold"], serde_json::json!(["ORB-S"]));
    assert_eq!(shadow_drains[0]["would_add"], serde_json::json!([]));
    assert_eq!(shadow_drains[0]["enforce_passes"], 0);
    assert_eq!(shadow_drains[0]["enforce_admitted"], 0);
    assert_eq!(shadow_drains[0]["gap_held"], serde_json::json!([]));
    assert_eq!(shadow_drains[0]["pull_would_hold"], 0);
    assert_eq!(shadow_drains[0]["pull_held"], 0);
}
