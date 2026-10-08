//! Doctor behavior through the CLI, with all Orbit state confined to disposable roots.
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::process::Output;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::DOCTOR_FINDINGS_MESSAGE_PREFIX;
use rusqlite::Connection;
use serde_json::Value;

use crate::git_repo::WorkCheckout;

fn command(fixture: &WorkCheckout) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&fixture.work)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env_remove("ORBIT_FORMAT");
    command
}

fn doctor(fixture: &WorkCheckout, extra: &[&str]) -> Output {
    command(fixture)
        .args(["doctor", "--json"])
        .args(extra)
        .output()
        .expect("doctor report")
}

fn rows(output: &Output) -> Vec<Value> {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("doctor JSON: {error}; {output:?}"))
}

fn row<'a>(rows: &'a [Value], name: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["check"] == name)
        .expect("diagnostic row")
}

/// Corrupt an unrelated B-tree page, leaving the header/schema ledger readable.
fn corrupt_unrelated_data_page(fixture: &WorkCheckout) {
    let database = fixture.home.join(".orbit/orbit.db");
    let offset = {
        let db = Connection::open(&database).expect("fixture DB");
        db.execute_batch("CREATE TABLE doctor_probe(value TEXT); INSERT INTO doctor_probe VALUES('data'); PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("fixture data page");
        let page: u64 = db
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = 'doctor_probe'",
                [],
                |row| row.get(0),
            )
            .expect("table root page");
        let page_size: u64 = db
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .expect("page size");
        (page - 1) * page_size
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(&database)
        .expect("fixture DB bytes");
    file.seek(SeekFrom::Start(offset))
        .expect("data page offset");
    file.write_all(&[0]).expect("invalid B-tree page type");
    drop(file);
}

#[test]
fn default_doctor_checks_schema_and_deep_detects_corrupt_data() {
    let fixture = WorkCheckout::new();
    let initial = rows(&doctor(&fixture, &[]));
    assert_eq!(row(&initial, "database")["status"], "ok");
    corrupt_unrelated_data_page(&fixture);
    let cheap = rows(&doctor(&fixture, &[]));
    assert_eq!(
        row(&cheap, "database")["status"],
        "ok",
        "default checks must not scan unrelated data pages"
    );
    let deep = doctor(&fixture, &["--deep"]);
    assert_eq!(
        deep.status.code(),
        Some(1),
        "deep integrity failure must set the exit status"
    );
    let deep_rows = rows(&deep);
    assert_eq!(
        row(&deep_rows, "database")["status"],
        "error",
        "deep must detect the corrupt data page"
    );
    for diagnostics in [&cheap, &deep_rows] {
        assert!(!diagnostics.is_empty());
        for diagnostic in diagnostics {
            assert!(
                diagnostic["duration_ms"].as_u64().is_some(),
                "every diagnostic, including error and skipped rows, must carry elapsed milliseconds: {diagnostic}"
            );
        }
    }
}

#[test]
fn doctor_with_a_failing_check_records_the_check_on_its_audit_row() {
    let fixture = WorkCheckout::new();
    assert_eq!(
        row(&rows(&doctor(&fixture, &[])), "database")["status"],
        "ok"
    );
    corrupt_unrelated_data_page(&fixture);
    let deep = doctor(&fixture, &["--deep"]);
    assert_eq!(deep.status.code(), Some(1), "{deep:?}");

    let listed = command(&fixture)
        .args(["audit", "list", "--limit", "50", "--json"])
        .output()
        .expect("audit list");
    let events: Vec<Value> = serde_json::from_slice(&listed.stdout)
        .unwrap_or_else(|error| panic!("audit JSON: {error}; {listed:?}"));
    let failed = events
        .iter()
        .find(|event| event["command"] == "doctor" && event["exit_code"] == 1)
        .unwrap_or_else(|| panic!("failed doctor audit row missing: {events:?}"));
    let message = failed["error_message"].as_str().unwrap_or_default();
    // Provider checks also fail on hosts without the crews' CLIs (CI
    // runners), so assert that `database` is among the named failures rather
    // than that it is the only one.
    let named_failures = message
        .strip_prefix(DOCTOR_FINDINGS_MESSAGE_PREFIX)
        .and_then(|findings| findings.split_once('('))
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(names, _)| names)
        .unwrap_or_default();
    assert!(
        named_failures.split(", ").any(|name| name == "database"),
        "the audit row must name the failing check: {failed}"
    );
}

/// Manual performance fixture; setup writes a real 1 GiB SQLite table and 50k directories.
/// Keep its disk and setup cost out of routine CI; run and record it before handoff.
#[test]
#[ignore = "large doctor performance fixture; run with --ignored --nocapture"]
fn default_doctor_large_store_and_worktrees_under_five_seconds() {
    let fixture = WorkCheckout::new();
    assert_eq!(
        row(&rows(&doctor(&fixture, &[])), "database")["status"],
        "ok"
    );
    let database = fixture.home.join(".orbit/orbit.db");
    {
        let db = Connection::open(&database).expect("fixture DB");
        db.execute_batch(
            "PRAGMA synchronous=OFF; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE doctor_large(value BLOB); BEGIN;
            INSERT INTO doctor_large VALUES(zeroblob(268435456));
            INSERT INTO doctor_large VALUES(zeroblob(268435456));
            INSERT INTO doctor_large VALUES(zeroblob(268435456));
            INSERT INTO doctor_large VALUES(zeroblob(268435456)); COMMIT; PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .expect("real 1 GiB database pages");
    }
    let database_bytes = fs::metadata(&database).expect("database size").len();
    assert!(database_bytes >= 1 << 30);
    // Restrict only the small Orbit-owned fixture tree before adding writable,
    // excluded checkout contents. The host's umask may otherwise create
    // legitimate writable container directories unrelated to this regression.
    #[cfg(unix)]
    for root in [fixture.home.join(".orbit"), fixture.work.join(".orbit")] {
        restrict_fixture_directories(&root);
    }
    let worktrees = fixture.work.join(".orbit/state/worktrees");
    let target = worktrees.join("run/target");
    for index in 0..50_000 {
        let path = if index < 10_000 {
            target.join(index.to_string())
        } else {
            worktrees.join(format!("checkout-{index}"))
        };
        fs::create_dir_all(&path).expect("worktree fixture directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o777))
                .expect("writable excluded directory");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for container in [
            &fixture.work.join(".orbit/state"),
            &worktrees,
            &worktrees.join("run"),
        ] {
            fs::set_permissions(container, fs::Permissions::from_mode(0o700))
                .expect("private Orbit-owned container");
        }
    }
    // A freshly linked debug binary can exceed 250 MB. Initialize the settled
    // generation through an ordinary fixture writer so timing measures doctor
    // rather than rehashing an uncached debug image on a read-only invocation.
    // This uses normal admission and does not change the database/page or walk probes.
    let initialized = command(&fixture)
        .args(["config", "set", "review.before_pr", "false"])
        .output()
        .expect("initialize fixture generation digest");
    assert!(
        initialized.status.success(),
        "fixture config writer failed: {initialized:?}"
    );
    let start = Instant::now();
    let output = doctor(&fixture, &[]);
    let elapsed = start.elapsed();
    let diagnostics = rows(&output);
    assert_eq!(row(&diagnostics, "database")["status"], "ok");
    assert_eq!(
        row(&diagnostics, "state-directory-permissions")["status"],
        "ok",
        "excluded writable worktrees must not warn: {}",
        row(&diagnostics, "state-directory-permissions")
    );
    if let Some(scratch) = std::env::var_os("ORBIT_SCRATCH_DIR") {
        let report = serde_json::json!({
            "database_bytes": database_bytes,
            "worktree_directories": 50_000,
            "target_directories": 10_000,
            "elapsed_ms": elapsed.as_millis(),
            "checks": diagnostics,
        });
        fs::write(
            std::path::PathBuf::from(scratch).join("doctor-performance.json"),
            serde_json::to_vec_pretty(&report).expect("benchmark JSON"),
        )
        .expect("record benchmark evidence under Orbit scratch");
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "default doctor took {elapsed:?} with a 1 GiB DB and 50k worktree directories"
    );
}

#[cfg(unix)]
fn restrict_fixture_directories(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("private fixture directory");
    for entry in fs::read_dir(path).expect("fixture children") {
        let entry = entry.expect("fixture entry");
        if entry.file_type().expect("fixture entry type").is_dir() {
            restrict_fixture_directories(&entry.path());
        }
    }
}
