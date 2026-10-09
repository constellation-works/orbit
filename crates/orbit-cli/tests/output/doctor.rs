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

const WARNING_MESSAGE: &str =
    "The index is outdated and must be refreshed before querying the latest workspace records.";
const ERROR_MESSAGE: &str = "The after-landing review consumer is stuck in definition_changed and cannot accept new delivery candidates.";
const FIX: &str = "orbit auto-task recover delivery-code-review";

pub(super) fn render_fixture(case: &str) -> String {
    let logs = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("child logs");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", "doctor::render_fixture_child", "--nocapture"])
        .env("ORBIT_DOCTOR_RENDER_CASE", case);
    let output =
        test_env::run_child_test(&mut command, "doctor::render_fixture_child", logs.path());
    test_env::assert_child_test_passed(
        "doctor::render_fixture_child",
        output.status,
        &output.stdout,
        &output.stderr,
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    stdout
        .split_once("BEGIN_DOCTOR\n")
        .expect("start marker")
        .1
        .split_once("END_DOCTOR")
        .expect("end marker")
        .0
        .to_string()
}

#[test]
fn render_fixture_child() {
    use crate::output::{
        payload::{Block, Payload},
        sink::{FormatArg, OutputSink, SinkEnv},
        table::{Column, Table},
    };
    use orbit_cmd::{WorkspaceDoctorResult, WorkspaceDoctorStatus as Status};
    let Ok(case) = std::env::var("ORBIT_DOCTOR_RENDER_CASE") else {
        return;
    };
    let mut rows = vec![
        WorkspaceDoctorResult {
            check_name: "index".into(),
            status: Status::Warning,
            message: WARNING_MESSAGE.into(),
            remediation: Some("orbit index refresh".into()),
            duration_ms: 0,
        },
        WorkspaceDoctorResult {
            check_name: "database".into(),
            status: Status::Ok,
            message: "Database is healthy".into(),
            remediation: None,
            duration_ms: 0,
        },
        WorkspaceDoctorResult {
            check_name: "review".into(),
            status: Status::Error,
            message: ERROR_MESSAGE.into(),
            remediation: Some(FIX.into()),
            duration_ms: 0,
        },
    ];
    if case == "ok" {
        for row in &mut rows {
            row.status = Status::Ok;
            row.remediation = None;
        }
    }
    let mut table = Table::new(vec![
        Column::new("CHECK").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("DETAILS"),
    ]);
    for row in &rows {
        let status = match row.status {
            Status::Ok => "OK",
            Status::Warning => "WARN",
            Status::Error => "ERROR",
            Status::Skipped => "SKIP",
        };
        let detail = row.remediation.as_ref().map_or_else(
            || row.message.clone(),
            |fix| format!("{} | Action: {fix}", row.message),
        );
        table.add_row(vec![
            comfy_table::Cell::new(&row.check_name),
            comfy_table::Cell::new(status),
            comfy_table::Cell::new(detail),
        ]);
    }
    let doc = serde_json::Value::Array(rows.iter().map(orbit_cmd::doctor_row_json).collect());
    let mode = match case.as_str() {
        "plain" => FormatArg::Plain,
        "json" => FormatArg::Json,
        _ => FormatArg::Table,
    };
    let sink = OutputSink::resolve(
        true,
        &SinkEnv {
            columns: Some(
                if case == "wide" {
                    "240"
                } else if case == "narrow" {
                    "32"
                } else {
                    "48"
                }
                .into(),
            ),
            no_color: (case != "plain").then(|| "1".into()),
            ..SinkEnv::default()
        },
        None,
        Some(mode),
        false,
    );
    if case == "plain" {
        assert!(
            !sink.color_allowed(),
            "explicit plain must disable terminal styling"
        );
    }
    writeln!(std::io::stdout(), "BEGIN_DOCTOR").expect("start marker");
    crate::output::render::emit(
        Payload::blocks(doc, vec![Block::table(table), Block::DoctorFindings(rows)]).into(),
        &sink,
    )
    .expect("emit");
    writeln!(std::io::stdout(), "END_DOCTOR").expect("end marker");
}

#[test]
fn terminal_findings_preserve_messages_commands_and_one_line_rows() {
    let stdout = render_fixture("mixed");
    let (table, findings) = stdout
        .split_once("\nFindings:\n")
        .expect("findings below table");
    assert_eq!(
        table.lines().count(),
        4,
        "header plus three one-line rows: {table}"
    );
    assert!(table.contains("database  OK"), "{table}");
    assert!(table.contains('…'), "fixture must truncate: {table}");
    assert!(findings.find("review  ERROR").unwrap() < findings.find("index  WARN").unwrap());
    for message in [ERROR_MESSAGE, WARNING_MESSAGE] {
        assert!(
            findings
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(message),
            "{findings}"
        );
    }
    assert!(findings.lines().any(|line| line == format!("Fix: {FIX}")));
    assert!(findings.contains("Fix: orbit index refresh"));
    assert!(!findings.contains("database"));
    for line in findings
        .lines()
        .filter(|line| !line.starts_with("Fix:") && !line.starts_with("Table details"))
    {
        assert!(line.chars().count() <= 48, "unwrapped prose: {line}");
    }
    assert!(findings.contains("--format plain or --json"));
    let narrow = render_fixture("narrow");
    assert!(
        narrow.lines().any(|line| line == format!("Fix: {FIX}")),
        "even an over-width command stays verbatim: {narrow}"
    );
    let wide = render_fixture("wide");
    assert!(wide.contains(ERROR_MESSAGE));
    assert!(!wide.contains("Table details shortened"));
    let ok = render_fixture("ok");
    assert_eq!(ok.lines().count(), 4);
    assert!(!ok.contains("Findings:"));
    assert!(!ok.contains("--format plain"));
}

#[test]
fn findings_leave_plain_and_json_documents_unchanged() {
    let plain = render_fixture("plain");
    assert_eq!(plain.lines().count(), 3);
    assert!(plain.contains(&format!("review\tERROR\t{ERROR_MESSAGE} | Action: {FIX}")));
    assert!(!plain.contains("Findings:"));
    let json: Value = serde_json::from_str(&render_fixture("json")).expect("JSON document");
    assert_eq!(json[2]["message"], ERROR_MESSAGE);
    assert_eq!(json[2]["remediation"], FIX);
    assert_eq!(json.as_array().unwrap().len(), 3);
}

#[test]
fn doctor_cli_table_adds_every_non_ok_finding_and_plain_alias_keeps_values() {
    const TEST: &str =
        "doctor::doctor_cli_table_adds_every_non_ok_finding_and_plain_alias_keeps_values";
    if std::env::var_os("ORBIT_DOCTOR_CLI_CHILD").is_none() {
        let logs = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("child logs");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", TEST, "--nocapture"])
            .env("ORBIT_DOCTOR_CLI_CHILD", "1");
        let output = test_env::run_child_test(&mut command, TEST, logs.path());
        test_env::assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
        return;
    }
    let fixture = WorkCheckout::new();
    doctor(&fixture, &[]);
    corrupt_unrelated_data_page(&fixture);
    let diagnostics = rows(&doctor(&fixture, &["--deep"]));
    let table = command(&fixture)
        .args(["doctor", "--deep", "--format", "table"])
        .output()
        .expect("table");
    assert_eq!(table.status.code(), Some(1));
    let stdout = String::from_utf8(table.stdout).expect("table UTF-8");
    let (_, findings) = stdout.split_once("Findings:").expect("CLI findings");
    for row in diagnostics.iter().filter(|row| row["status"] != "ok") {
        assert!(
            findings.contains(row["check"].as_str().unwrap()),
            "{findings}"
        );
        if let Some(fix) = row["remediation"].as_str() {
            assert!(findings.contains(&format!("Fix: {fix}")), "{findings}");
        }
    }
    let piped = command(&fixture)
        .args(["doctor", "--deep"])
        .output()
        .expect("plain");
    let explicit = command(&fixture)
        .args(["doctor", "--deep", "--format", "plain"])
        .output()
        .expect("explicit plain");
    let database_line = |output: Output| {
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find(|line| line.starts_with("database\t"))
            .unwrap()
            .to_string()
    };
    assert_eq!(database_line(piped), database_line(explicit));
}
