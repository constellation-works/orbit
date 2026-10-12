//! `orbit run profiles` through the real CLI: the per-profile table, the full
//! per-minute series under `--json`, and the no-diff build flag naming the
//! deliveries behind it.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::{SecondsFormat, Utc};
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
