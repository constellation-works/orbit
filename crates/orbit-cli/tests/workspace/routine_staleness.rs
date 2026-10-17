//! `orbit doctor` warns when an enabled cron routine's last success is older
//! than three times its interval, and `orbit routine list --json` shows why
//! a skipped slot was not fired.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use assert_cmd::cargo::cargo_bin_cmd;
use chrono::{Duration, Utc};
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).expect("create home");
        crate::git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        // An uninitialized host seeds no routines: `orbit init` owns the
        // machine identity the scheduler and doctor judge them against.
        fixture
            .orbit()
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
                "staleness-host",
                "--task-prefix",
                "STL",
            ])
            .assert()
            .success();
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "staleness"])
            .assert()
            .success();
        let path = fixture.work.join(".orbit/routines/worktree_gc.yaml");
        let raw = fs::read_to_string(&path).expect("seeded worktree gc routine");
        let enabled = raw.replacen("enabled: false", "enabled: true", 1);
        assert_ne!(raw, enabled, "seeded routine must start disabled");
        fs::write(&path, enabled).expect("enable worktree gc");
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
            .env("TZ", "UTC")
            .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
            .env_remove("ORBIT_FORMAT")
            .env_remove("ORBIT_HOME")
            .env("RUST_LOG", "off");
        command.timeout(std::time::Duration::from_secs(60));
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "orbit {args:?} failed: {stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_str(&stdout)
            .unwrap_or_else(|error| panic!("orbit {args:?} JSON: {error}; {stdout}"))
    }
}

fn routine_name(work: &std::path::Path) -> String {
    fs::read_to_string(work.join(".orbit/routines/worktree_gc.yaml"))
        .expect("read routine")
        .lines()
        .find_map(|line| line.strip_prefix("name: "))
        .expect("routine name")
        .trim()
        .to_string()
}

fn sql(db: &std::path::Path, statement: &str) {
    let output = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; conn = sqlite3.connect(sys.argv[1]); conn.execute(sys.argv[2]); conn.commit()",
            &db.display().to_string(),
            statement,
        ])
        .output()
        .expect("python3 sqlite");
    assert!(
        output.status.success(),
        "sqlite failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn staleness(report: &Value) -> &Value {
    report
        .as_array()
        .expect("doctor json is a list of checks")
        .iter()
        .find(|row| row["check"] == "routine-staleness")
        .expect("routine-staleness row")
}

#[test]
fn doctor_warns_when_a_routine_last_succeeded_beyond_three_intervals() {
    let fixture = Fixture::new();
    let name = routine_name(&fixture.work);
    let listed = fixture.json(&["routine", "list", "--format", "json"]);
    assert!(
        listed["routines"]
            .as_array()
            .expect("routines")
            .iter()
            .any(|routine| routine["name"] == name && routine["enabled"] == true),
        "enabled worktree gc must be listed: {listed}"
    );

    let now = Utc::now();
    let succeeded_slot = (now - Duration::hours(5)).to_rfc3339();
    let skip_older = (now - Duration::hours(2)).to_rfc3339();
    let skip_newer = (now - Duration::hours(1)).to_rfc3339();
    let stale_at = (now - Duration::hours(4)).to_rfc3339();
    let db = fixture.home.join(".orbit/orbit.db");
    let insert = format!(
        "INSERT INTO routine_fires (routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at) VALUES \
         ('{name}', '{succeeded_slot}', 1, 'succeeded', 'run-ok', 'staleness', NULL, '{stale_at}', '{stale_at}'), \
         ('{name}', '{skip_older}', 1, 'skipped', NULL, 'staleness', 'missed_run: skip', '{skip_older}', '{skip_older}'), \
         ('{name}', '{skip_newer}', 1, 'skipped', NULL, 'staleness', 'missed_run: skip', '{skip_newer}', '{skip_newer}')"
    );
    sql(&db, &insert);

    let warning_report = fixture.json(&["doctor", "--format", "json"]);
    let warning = staleness(&warning_report);
    assert_eq!(warning["status"], "warning", "{warning}");
    let message = warning["message"].as_str().expect("message");
    assert!(
        message.contains(&name),
        "warning must name the routine: {message}"
    );

    let fresh_at = (now - Duration::minutes(20)).to_rfc3339();
    sql(
        &db,
        &format!(
            "UPDATE routine_fires SET updated_at = '{fresh_at}' WHERE routine_name = '{name}' AND state = 'succeeded'"
        ),
    );
    let fresh_report = fixture.json(&["doctor", "--format", "json"]);
    let fresh = staleness(&fresh_report);
    assert_eq!(fresh["status"], "ok", "{fresh}");

    let listed = fixture.json(&["routine", "list", "--format", "json"]);
    let routine = listed["routines"]
        .as_array()
        .expect("routines")
        .iter()
        .find(|routine| routine["name"] == name)
        .expect("worktree gc row");
    assert_eq!(routine["last_fire"]["detail"], "missed_run: skip");
    let recorded = routine["recent_fires"]
        .as_array()
        .expect("recent fires")
        .iter()
        .filter(|fire| fire["detail"] == "missed_run: skip")
        .count();
    assert_eq!(recorded, 2, "{routine}");
}
