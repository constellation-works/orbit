//! Doctor behavior through the CLI, with all Orbit state confined to disposable roots.
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::process::Output;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_config::{ConfigRoots, OperationLayerSource, ResolvedConfig};
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

/// ORB-16351: a unit file that names this binary is not `clock-unit: ok` while
/// the systemd timer is disabled or inactive. A substitute `systemctl` reports
/// each state; enabled and active stays ok.
#[cfg(target_os = "linux")]
#[test]
fn doctor_clock_unit_requires_an_enabled_active_systemd_timer() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_clock_unit_requires_an_enabled_active_systemd_timer",
    ) {
        return;
    }
    for mode in ["disabled", "inactive", "enabled"] {
        let fixture = WorkCheckout::new();
        install_matching_sweep_unit(&fixture.home);
        let bin = write_substitute_systemctl(fixture.temp.path());
        let output = command(&fixture)
            .env("PATH", prepend_path(&bin))
            .env("ORBIT_TEST_CLOCK_MANAGER", mode)
            .args(["doctor", "--json"])
            .output()
            .expect("doctor report");
        let parsed = rows(&output);
        let clock = row(&parsed, "clock-unit");
        let rendered = format!("{clock}");
        if mode == "enabled" {
            assert_eq!(clock["status"], "ok", "{mode}: {rendered}");
        } else {
            assert_ne!(clock["status"], "ok", "{mode}: {rendered}");
            assert!(
                rendered.contains("orbit clock enable"),
                "{mode}: {rendered}"
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn install_matching_sweep_unit(home: &std::path::Path) {
    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("systemd user unit dir");
    let program = env!("CARGO_BIN_EXE_orbit")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        unit_dir.join("orbit-sweep.service"),
        format!(
            "[Service]\nType=oneshot\nTimeoutStartSec=10min\nKillMode=mixed\nExecStart=\"{program}\" clock tick\n"
        ),
    )
    .expect("sweep unit");
}

#[cfg(target_os = "linux")]
fn write_substitute_systemctl(root: &std::path::Path) -> std::path::PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("substitute bin");
    let path = bin.join("systemctl");
    fs::write(
        &path,
        r#"#!/bin/sh
mode=${ORBIT_TEST_CLOCK_MANAGER:-enabled}
case " $* " in
  *" is-enabled "*)
    if [ "$mode" = "disabled" ]; then
      echo disabled
      exit 1
    fi
    echo enabled
    exit 0
    ;;
esac
for arg in "$@"; do
  case "$arg" in
    orbit-sweep.timer)
      if [ "$mode" = "disabled" ] || [ "$mode" = "inactive" ]; then
        printf '%s\n' LoadState=loaded ActiveState=inactive
      else
        printf '%s\n' LoadState=loaded ActiveState=active \
          'NextElapseUSecRealtime=Mon 2026-10-19 12:00:00 UTC' \
          NextElapseUSecMonotonic=1000000
      fi
      exit 0
      ;;
    orbit-sweep.service)
      printf '%s\n' ActiveState=inactive Result=success TimeoutStartUSec=10min KillMode=mixed
      exit 0
      ;;
  esac
done
exit 0
"#,
    )
    .expect("substitute systemctl");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&path)
        .expect("systemctl metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("systemctl mode");
    bin
}

#[cfg(target_os = "linux")]
fn prepend_path(dir: &std::path::Path) -> std::ffi::OsString {
    let mut entries = vec![dir.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(entries).expect("join PATH")
}

#[cfg(unix)]
#[test]
fn doctor_warns_about_selected_root_skill_links_without_changing_discovery_entries() {
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    let fixture = WorkCheckout::new();
    let base = fixture.temp.path().join("selected");
    let global = base.join(".orbit");
    let live_target = base.join("live-skill");
    fs::create_dir_all(&live_target).expect("live skill target");
    fs::write(live_target.join("SKILL.md"), "live skill bytes").expect("live content");
    let roots = [base.join(".agents/skills"), base.join(".claude/skills")];
    for root in &roots {
        fs::create_dir_all(root.join("regular-skill")).expect("discovery directory");
        fs::write(root.join("regular-skill/notes.txt"), "operator bytes")
            .expect("operator content");
        symlink(&live_target, root.join("live-skill")).expect("live skill link");
        // Only immediate discovery links are in scope; nested content is operator-owned.
        symlink(
            base.join("missing-nested"),
            root.join("regular-skill/nested"),
        )
        .expect("nested dangling link");
    }
    let home_discovery = fixture.home.join(".claude/skills");
    fs::create_dir_all(&home_discovery).expect("other discovery root");
    let home_link = home_discovery.join("unselected-skill");
    let home_target = fixture.home.join("missing-skill");
    symlink(&home_target, &home_link).expect("unselected dangling link");

    let invoke = |args: &[&str]| {
        command(&fixture)
            .arg("--root")
            .arg(&global)
            .args(args)
            .output()
            .expect("selected-root doctor")
    };
    command(&fixture)
        .arg("--root")
        .arg(&global)
        .args([
            "init",
            "--non-interactive",
            "--machine-name",
            "doctor-fixture",
            "--task-prefix",
            "DF",
        ])
        .assert()
        .success();
    let initial = rows(&invoke(&["doctor", "--json"]));
    assert_eq!(row(&initial, "artifacts-skills")["status"], "ok");

    let links = [roots[0].join("leaked-skill"), roots[1].join("leaked-skill")];
    let targets = [
        base.join("missing-skill"),
        PathBuf::from("../../missing-skill"),
    ];
    for (link, target) in links.iter().zip(&targets) {
        symlink(target, link).expect("dangling discovery link");
    }
    let plain = invoke(&["doctor", "--format", "plain"]);
    let stdout = String::from_utf8(plain.stdout).expect("plain UTF-8");
    let skills_line = stdout
        .lines()
        .find(|line| line.starts_with("artifacts-skills\t"))
        .expect("plain skills row");
    assert_eq!(skills_line.split('\t').nth(1), Some("warning"));
    for (link, target) in links.iter().zip(&targets) {
        assert!(
            skills_line.contains(&link.display().to_string()),
            "{skills_line}"
        );
        assert!(
            fs::symlink_metadata(link)
                .expect("plain doctor retains link")
                .file_type()
                .is_symlink(),
            "plain diagnosis must preserve dangling discovery symlinks",
        );
        assert_eq!(fs::read_link(link).unwrap(), *target);
    }

    let diagnostics = rows(&invoke(&["doctor", "--json"]));
    let warning = row(&diagnostics, "artifacts-skills");
    assert_eq!(warning["status"], "warning");
    let message = warning["message"].as_str().expect("skills warning message");
    let remediation = warning["remediation"].as_str().expect("manual fix-it");
    assert!(!remediation.is_empty());
    assert_eq!(remediation.lines().count(), 1);
    assert!(
        skills_line.contains(message),
        "plain and JSON carry the same warning"
    );
    assert!(
        skills_line.contains(remediation),
        "plain and JSON carry the same fix-it"
    );
    for link in &links {
        assert!(message.contains(&link.display().to_string()), "{message}");
        assert!(
            remediation.contains(&link.display().to_string()),
            "{remediation}"
        );
    }
    let skill_diagnostics = rows(&invoke(&["skill", "doctor", "--json"]));
    for (link, target) in links.iter().zip(&targets) {
        let finding = skill_diagnostics
            .iter()
            .find(|row| {
                row["message"]
                    .as_str()
                    .unwrap()
                    .contains(&link.display().to_string())
            })
            .expect("skill doctor link diagnostic");
        assert_eq!(finding["status"], "warning");
        assert_eq!(fs::read_link(link).unwrap(), *target);
    }
    for root in &roots {
        assert_eq!(fs::read_link(root.join("live-skill")).unwrap(), live_target);
        assert!(root.join("regular-skill").is_dir());
        assert_eq!(
            fs::read_to_string(root.join("regular-skill/notes.txt")).unwrap(),
            "operator bytes"
        );
        assert_eq!(
            fs::read_link(root.join("regular-skill/nested")).unwrap(),
            base.join("missing-nested")
        );
    }
    assert_eq!(
        fs::read_to_string(live_target.join("SKILL.md")).unwrap(),
        "live skill bytes"
    );
    assert_eq!(fs::read_link(home_link).unwrap(), home_target);

    for link in &links {
        fs::remove_file(link).expect("operator removes fixture's dangling link");
    }
    let healthy = rows(&invoke(&["doctor", "--json"]));
    assert_eq!(row(&healthy, "artifacts-skills")["status"], "ok");
    let healthy_plain = invoke(&["doctor", "--format", "plain"]);
    assert!(
        String::from_utf8(healthy_plain.stdout)
            .unwrap()
            .lines()
            .any(|line| line.starts_with("artifacts-skills\tok\t"))
    );
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
    if crate::isolated_test::run_in_child(
        "doctor::default_doctor_checks_schema_and_deep_detects_corrupt_data",
    ) {
        return;
    }
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

    let table = command(&fixture)
        .args(["doctor", "--deep", "--format", "table"])
        .output()
        .expect("table");
    assert_eq!(table.status.code(), Some(1));
    let stdout = String::from_utf8(table.stdout).expect("table UTF-8");
    let (_, findings) = stdout.split_once("Findings:").expect("CLI findings");
    // An `info` row is a fact about the setup, not a finding.
    for diagnostic in deep_rows
        .iter()
        .filter(|diagnostic| diagnostic["status"] != "ok" && diagnostic["status"] != "info")
    {
        assert!(
            findings.contains(diagnostic["check"].as_str().unwrap()),
            "{findings}"
        );
        if let Some(fix) = diagnostic["remediation"].as_str() {
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

#[cfg(unix)]
#[test]
fn permission_remediation_repairs_legacy_state_and_preserves_excluded_trees() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = WorkCheckout::new();
    doctor(&fixture, &[]);
    let global = fixture.home.join(".orbit");
    let workspace = fixture.work.join(".orbit");
    let legacy = [
        global.clone(),
        global.join("tasks"),
        global.join("tasks/workspaces/legacy/artifacts/files/validation/claim-legacy"),
        global.join("state/logs/legacy"),
        global.join("frictions/legacy"),
        workspace.clone(),
        workspace.join("tasks/legacy"),
        workspace.join("state/legacy"),
        workspace.join("frictions/legacy"),
    ];
    for path in &legacy {
        fs::create_dir_all(path).expect("legacy state");
        fs::set_permissions(path, fs::Permissions::from_mode(0o775)).expect("shared legacy mode");
    }
    let excluded = [
        global.join("state/worktrees"),
        global.join("state/worktrees/run/nested"),
        global.join("tasks/target"),
        global.join("tasks/target/nested"),
        workspace.join("state/worktrees"),
        workspace.join("state/worktrees/run/nested"),
        workspace.join("state/legacy/target"),
        workspace.join("state/legacy/target/nested"),
        fixture.temp.path().join("outside"),
    ];
    for path in &excluded {
        fs::create_dir_all(path).expect("excluded tree");
        fs::set_permissions(path, fs::Permissions::from_mode(0o777)).expect("excluded mode");
    }
    let cache = global.join("cache");
    if cache.exists() {
        fs::rename(&cache, fixture.temp.path().join("retained-cache"))
            .expect("retain existing fixture cache");
    }
    symlink(excluded.last().unwrap(), &cache).expect("linked scan subtree root");
    symlink(excluded.last().unwrap(), global.join("state/linked"))
        .expect("external directory link");
    let file = global.join("state/legacy-file");
    fs::write(&file, "retained").expect("legacy file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).expect("legacy file mode");
    let before = rows(&doctor(&fixture, &[]));
    let warning = row(&before, "state-directory-permissions");
    assert_eq!(warning["status"], "warning");
    let remediation = warning["remediation"]
        .as_str()
        .expect("runnable remediation");
    let runnable = remediation.split('`').nth(1).expect("repair command");
    let args = runnable
        .strip_prefix("orbit ")
        .expect("Orbit repair")
        .split_whitespace()
        .collect::<Vec<_>>();
    let repaired = command(&fixture)
        .args(args)
        .arg("--json")
        .output()
        .expect("run remediation as written");
    let after = rows(&repaired);
    assert_eq!(
        row(&after, "fix-state-directory-permissions")["status"],
        "ok"
    );
    assert_eq!(row(&after, "state-directory-permissions")["status"], "ok");
    for path in &legacy {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700,
            "{}",
            path.display()
        );
    }
    for path in &excluded {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o777,
            "repair must leave worktrees, targets and symlink targets untouched: {}",
            path.display()
        );
    }
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o666
    );
    assert_eq!(fs::read_to_string(file).unwrap(), "retained");
    let repeated = rows(&doctor(&fixture, &["--fix-state-directory-permissions"]));
    assert_eq!(
        row(&repeated, "state-directory-permissions")["status"],
        "ok"
    );
    assert_eq!(
        row(&repeated, "fix-state-directory-permissions")["status"],
        "ok"
    );
    for path in &excluded {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o777,
            "excluded directories must also survive repeated repair: {}",
            path.display()
        );
    }
}

/// Manual performance fixture; setup writes a real 1 GiB SQLite table and 50k directories.
/// Keep its disk and setup cost out of routine CI; run and record it before handoff.
#[test]
#[ignore = "large doctor performance fixture; run with --ignored --nocapture"]
fn default_doctor_large_store_and_worktrees_under_five_seconds() {
    if crate::isolated_test::run_in_child(
        "doctor::default_doctor_large_store_and_worktrees_under_five_seconds",
    ) {
        return;
    }
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
        .args(["config", "set", "review.timing", "off"])
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
            Status::Info => "INFO",
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

/// `orbit doctor` runs the scan a sandboxed leaf meets, so an operator learns
/// of a refusal, and of its fix, before a drain claims work.
/// `orbit doctor` names the worker CPU ceiling, or says there is none, so an
/// operator can tell whether one leaf can still take every core [ORB-15196].
#[cfg(target_os = "linux")]
#[test]
fn doctor_reports_the_worker_cpu_quota() {
    let fixture = WorkCheckout::new();
    let unset = rows(&doctor(&fixture, &[]));
    let unset = row(&unset, "worker-containment");
    assert_eq!(unset["status"], "info", "{unset}");
    assert!(
        unset["message"]
            .as_str()
            .is_some_and(|message| message.contains("CPUQuota is not set")),
        "{unset}"
    );

    let global = fixture.home.join(".orbit");
    fs::create_dir_all(&global).expect("global root");
    fs::write(
        global.join("config.toml"),
        "[machine]\nworker_cpu_quota = 300\n",
    )
    .expect("global config");
    let set = rows(&doctor(&fixture, &[]));
    let set = row(&set, "worker-containment");
    assert_eq!(set["status"], "ok", "{set}");
    assert!(
        set["message"]
            .as_str()
            .is_some_and(|message| message.contains("CPUQuota=300%")),
        "{set}"
    );
}

/// A retired key is reported with its file and left in place. The fix removes
/// only that key, writes one backup, and a second run writes nothing more.
#[test]
fn doctor_lists_a_retired_config_key_and_the_fix_removes_only_that_key() {
    let fixture = WorkCheckout::new();
    let global_dir = fixture.home.join(".orbit");
    fs::create_dir_all(&global_dir).expect("global root");
    let global_config = global_dir.join("config.toml");
    let global_body = "[workflow]\nbase_branch = \"main\"\n";
    fs::write(&global_config, global_body).expect("global config");

    let workspace_config = fixture.work.join(".orbit/config.toml");
    let workspace_body = "\
# keep this note
[workflow]
base_branch = \"agent-main\"
system_crew = \"sol\"

[plugin]
legacy_callback_identity = true

[scoring]
enabled = false

[task.approval]
delegate_approval = true

[workflow.future]
enabled = true

[plugins.future]
arbitrary = { nested = true }

[future]
";
    fs::write(&workspace_config, workspace_body).expect("workspace config");

    let reported = doctor(&fixture, &[]);
    assert_eq!(
        fs::read_to_string(&workspace_config).expect("workspace config"),
        workspace_body,
        "diagnosis must not rewrite config"
    );
    assert_eq!(
        fs::read_to_string(&global_config).expect("global config"),
        global_body,
        "diagnosis must not rewrite the other config"
    );
    assert!(
        backup_names(&fixture.work.join(".orbit")).is_empty(),
        "diagnosis must not write a backup"
    );
    let reported_rows = rows(&reported);
    let retired = reported_rows
        .iter()
        .filter(|row| row["check"] == "retired-config-keys" && row["status"] == "warning")
        .collect::<Vec<_>>();
    assert!(
        retired.iter().any(|row| mentions_key_and_file(
            row,
            "workflow.system_crew",
            &workspace_config
        )),
        "doctor names the deprecated key and its file: {reported_rows:?}"
    );
    assert!(
        retired
            .iter()
            .any(|row| mentions_key_and_file(row, "scoring.enabled", &workspace_config)),
        "doctor names the removed key and its file: {reported_rows:?}"
    );
    assert!(
        retired.iter().any(|row| mentions_key_and_file(
            row,
            "plugin.legacy_callback_identity",
            &workspace_config
        ) && row["message"]
            .as_str()
            .is_some_and(|message| message.contains("removed"))),
        "doctor reports the retired callback setting as removed: {reported_rows:?}"
    );
    assert!(
        ["task.approval", "workflow.future", "future"]
            .iter()
            .all(|key| retired.iter().any(|row| mentions_key_and_file(
                row,
                key,
                &workspace_config
            ))),
        "doctor names unknown tables and their file: {reported_rows:?}"
    );
    assert!(
        !retired
            .iter()
            .any(|row| mentions_key_and_file(row, "plugins.future", &workspace_config)),
        "plugin namespaces are owned by plugins: {reported_rows:?}"
    );
    assert!(
        retired.iter().all(|row| row["remediation"]
            .as_str()
            .is_some_and(|text| text.contains("--fix-retired-config-keys"))),
        "each retired key names the repair: {retired:?}"
    );
    assert!(
        !reported_rows
            .iter()
            .any(|row| row["check"] == "fix-retired-config-keys"),
        "the report does not run the repair"
    );

    let fixed = doctor(&fixture, &["--fix-retired-config-keys"]);
    let fixed_rows = rows(&fixed);
    let repair = row(&fixed_rows, "fix-retired-config-keys");
    assert_eq!(repair["status"], "ok", "{repair}");
    let repair_message = repair["message"].as_str().expect("repair message");
    assert!(
        repair_message.contains("workflow.system_crew")
            && repair_message.contains("scoring.enabled")
            && repair_message.contains("plugin.legacy_callback_identity")
            && repair_message.contains(&workspace_config.display().to_string()),
        "{repair}"
    );
    let updated = fs::read_to_string(&workspace_config).expect("updated config");
    assert!(
        updated.contains("# keep this note"),
        "the repair keeps the operator's comment: {updated}"
    );
    assert!(
        updated.contains("base_branch = \"agent-main\""),
        "the repair keeps the neighboring key: {updated}"
    );
    assert!(
        !updated.contains("legacy_callback_identity")
            && !updated.contains("system_crew")
            && !updated.contains("enabled")
            && !updated.contains("[future]")
            && !updated.contains("delegate_approval"),
        "the repair removes only the retired keys: {updated}"
    );
    let parsed: Value = serde_json::to_value(updated.parse::<toml::Value>().expect("updated TOML"))
        .expect("json config");
    assert_eq!(
        parsed["plugins"]["future"]["arbitrary"]["nested"], true,
        "plugin config survives cleanup"
    );
    assert_eq!(
        fs::read_to_string(&global_config).expect("global config"),
        global_body,
        "the repair leaves a file with no retired keys untouched"
    );
    let backups = backup_names(&fixture.work.join(".orbit"));
    assert_eq!(backups, vec!["config.toml.bak".to_string()], "{backups:?}");
    let backup = fixture.work.join(".orbit").join(&backups[0]);
    assert_eq!(
        fs::read_to_string(&backup).expect("backup"),
        workspace_body,
        "the backup is the file from before the repair"
    );
    assert_eq!(
        row(&fixed_rows, "retired-config-keys")["status"],
        "ok",
        "the report after the repair no longer warns: {fixed_rows:?}"
    );

    let settled = fs::read(&workspace_config).expect("settled config");
    let backup_bytes = fs::read(&backup).expect("backup bytes");
    let again = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
    let again_repair = row(&again, "fix-retired-config-keys");
    assert_eq!(again_repair["status"], "ok", "{again_repair}");
    assert!(
        again_repair["message"].as_str().is_some_and(
            |message| message.contains("No retired, deprecated or unknown config keys")
        ),
        "{again_repair}"
    );
    assert_eq!(
        fs::read(&workspace_config).expect("config after second run"),
        settled,
        "a second run does not rewrite the config"
    );
    assert_eq!(
        fs::read(&backup).expect("backup after second run"),
        backup_bytes,
        "a second run does not rewrite the backup"
    );
    assert_eq!(
        backup_names(&fixture.work.join(".orbit")),
        backups,
        "a second run writes no further backup"
    );
    assert_eq!(
        fs::read_to_string(&global_config).expect("global config"),
        global_body
    );
}

/// Live `[[review.host_evidence]]` rules are not unknown keys, so the repair
/// leaves them in each layer that states them and removes only the neighbouring
/// unknown key. A workspace list replaces the global one; an explicit empty
/// workspace list clears it, and both must survive repeated repair.
#[test]
fn doctor_retired_repair_keeps_host_evidence_rules_in_each_layer() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_retired_repair_keeps_host_evidence_rules_in_each_layer",
    ) {
        return;
    }
    const SEATBELT: &str = "[[review.host_evidence]]\nkind = \"host_sandbox_test\"\n\
        name = \"Seatbelt\"\npaths = [\"crates/orbit-exec/**\"]\nos = \"macos\"\n\
        command = \"cargo test -p orbit-exec --test seatbelt\"\n\
        artifact = \"evidence/seatbelt-macos.json\"\n";
    const CODEQL: &str = "[[review.host_evidence]]\nkind = \"codeql\"\n\
        name = \"Rust CodeQL (Linux)\"\npaths = [\"**/*.rs\"]\nos = \"linux\"\n\
        command = \"scripts/codeql-rust-local.sh\"\nartifact = \"evidence/codeql.json\"\n";
    for (workspace_body, expected_names) in [
        (
            format!("[review]\nreviewers = 3\n{CODEQL}"),
            vec!["Rust CodeQL (Linux)"],
        ),
        (
            "[review]\nhost_evidence = []\nreviewers = 3\n".to_string(),
            vec![],
        ),
    ] {
        let fixture = WorkCheckout::new();
        let global_dir = fixture.home.join(".orbit");
        fs::create_dir_all(&global_dir).expect("global root");
        let global_config = global_dir.join("config.toml");
        let global_body = format!("[review]\nreviewers = 2\n{SEATBELT}");
        fs::write(&global_config, &global_body).expect("global config");
        let workspace_dir = fixture.work.join(".orbit");
        let workspace_config = workspace_dir.join("config.toml");
        fs::write(&workspace_config, &workspace_body).expect("workspace config");

        let before = effective_host_evidence(&fixture);
        assert_eq!(before.0, OperationLayerSource::Workspace, "before repair");
        assert_eq!(before.1, expected_names, "before repair");

        let reported = rows(&doctor(&fixture, &[]));
        let retired = reported
            .iter()
            .filter(|row| row["check"] == "retired-config-keys")
            .collect::<Vec<_>>();
        assert!(
            retired.iter().all(|row| !row["message"]
                .as_str()
                .unwrap_or_default()
                .contains("review.host_evidence")),
            "live host rules are not reported as unknown: {reported:?}"
        );
        for config in [&global_config, &workspace_config] {
            assert!(
                retired
                    .iter()
                    .any(|row| mentions_key_and_file(row, "review.reviewers", config)),
                "doctor names the neighbouring unknown key and its file: {reported:?}"
            );
        }

        let fixed = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
        assert_eq!(
            row(&fixed, "fix-retired-config-keys")["status"],
            "ok",
            "{fixed:?}"
        );
        assert_eq!(
            effective_host_evidence(&fixture),
            before,
            "the effective rules survive the repair"
        );
        for (path, original) in [
            (&global_config, &global_body),
            (&workspace_config, &workspace_body),
        ] {
            let original_doc: toml::Value = toml::from_str(original).expect("original TOML");
            let updated: toml::Value =
                toml::from_str(&fs::read_to_string(path).expect("updated config"))
                    .expect("updated TOML");
            assert_eq!(
                updated["review"]["host_evidence"],
                original_doc["review"]["host_evidence"],
                "{}",
                path.display()
            );
            assert!(
                updated["review"].get("reviewers").is_none(),
                "{}",
                path.display()
            );
        }
        for dir in [&global_dir, &workspace_dir] {
            assert_eq!(
                backup_names(dir),
                vec!["config.toml.bak".to_string()],
                "{}",
                dir.display()
            );
        }
        assert_eq!(
            fs::read_to_string(global_dir.join("config.toml.bak")).expect("global backup"),
            global_body
        );
        assert_eq!(
            fs::read_to_string(workspace_dir.join("config.toml.bak")).expect("workspace backup"),
            workspace_body
        );

        let settled = (
            fs::read(&global_config).expect("global config"),
            fs::read(&workspace_config).expect("workspace config"),
        );
        let again = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
        assert_eq!(
            row(&again, "fix-retired-config-keys")["status"],
            "ok",
            "{again:?}"
        );
        assert_eq!(
            effective_host_evidence(&fixture),
            before,
            "a repeat repair keeps the effective rules"
        );
        assert_eq!(
            (
                fs::read(&global_config).expect("global config"),
                fs::read(&workspace_config).expect("workspace config"),
            ),
            settled,
            "a second run does not rewrite either config"
        );
        for dir in [&global_dir, &workspace_dir] {
            assert_eq!(
                backup_names(dir),
                vec!["config.toml.bak".to_string()],
                "a second run writes no further backup"
            );
        }
    }
}

/// The repair preserves translated review values in their original layer,
/// including explicit modern overrides, and keeps the first backup on retry.
#[test]
#[allow(clippy::print_stderr)] // Name each scenario and its elapsed time in isolated child failure output.
fn doctor_retired_review_repair_preserves_effective_policy() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_retired_review_repair_preserves_effective_policy",
    ) {
        return;
    }
    for (global, workspace, timing, minutes) in [
        (
            "",
            "[operation]\nreview_policy = 'before-pr'\nreview_minutes = 7\n",
            "before_pr",
            7,
        ),
        (
            "[operation]\nreview_policy = 'before-pr'\nreview_minutes = 11\n",
            "",
            "before_pr",
            11,
        ),
        (
            "[review]\ntiming = 'before_pr'\nminutes = 11\n",
            "[operation]\nreview_policy = 'none'\nreview_minutes = 7\n",
            "off",
            7,
        ),
        (
            "[operation]\nreview_policy = 'before-pr'\nreview_minutes = 11\n",
            "[review]\ntiming = 'off'\nminutes = 9\n",
            "off",
            9,
        ),
        (
            "",
            "[operation]\nreview_policy = 'before-pr'\nreview_minutes = 7\n[review]\ntiming = 'off'\nminutes = 9\n",
            "off",
            9,
        ),
    ] {
        let started = Instant::now();
        eprintln!("scenario: global={global:?}, workspace={workspace:?}");
        let fixture = WorkCheckout::new();
        let global_dir = fixture.home.join(".orbit");
        fs::create_dir_all(&global_dir).unwrap();
        let global_path = global_dir.join("config.toml");
        let workspace_path = fixture.work.join(".orbit/config.toml");
        fs::write(&global_path, global).unwrap();
        fs::write(&workspace_path, workspace).unwrap();
        let expected = (Value::from(timing), Value::from(minutes));
        assert_eq!(effective_review(&fixture), expected, "before repair");
        let fixed = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
        assert_eq!(
            row(&fixed, "fix-retired-config-keys")["status"],
            "ok",
            "{fixed:?}"
        );
        assert_eq!(effective_review(&fixture), expected, "after repair");
        for (path, original) in [(&global_path, global), (&workspace_path, workspace)] {
            let updated = fs::read_to_string(path).unwrap();
            let document: toml::Value = toml::from_str(&updated).unwrap();
            assert!(
                document
                    .get("operation")
                    .and_then(|v| v.get("review_policy"))
                    .is_none()
            );
            assert!(
                document
                    .get("operation")
                    .and_then(|v| v.get("review_minutes"))
                    .is_none()
            );
            if original.contains("[operation]") {
                assert_eq!(
                    fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
                    original
                );
            } else {
                assert_eq!(updated, original, "unaffected layer stays byte-identical");
            }
        }
        let settled = (
            fs::read(&global_path).unwrap(),
            fs::read(&workspace_path).unwrap(),
        );
        let backups = (
            backup_names(&global_dir),
            backup_names(&fixture.work.join(".orbit")),
        );
        let again = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
        assert_eq!(row(&again, "fix-retired-config-keys")["status"], "ok");
        assert_eq!(effective_review(&fixture), expected, "after repeat repair");
        assert_eq!(
            (
                fs::read(&global_path).unwrap(),
                fs::read(&workspace_path).unwrap()
            ),
            settled
        );
        assert_eq!(
            (
                backup_names(&global_dir),
                backup_names(&fixture.work.join(".orbit"))
            ),
            backups
        );
        for (path, original) in [(&global_path, global), (&workspace_path, workspace)] {
            if original.contains("[operation]") {
                assert_eq!(
                    fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
                    original
                );
            }
        }
        eprintln!("scenario elapsed: {:?}", started.elapsed());
    }
}

/// An after-landing policy, even when masked by another layer, requires an
/// operator migration. Refusal must precede every config write and backup.
#[test]
#[allow(clippy::print_stderr)] // Name each scenario and its elapsed time in isolated child failure output.
fn doctor_retired_review_repair_refuses_after_landing_before_any_write() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_retired_review_repair_refuses_after_landing_before_any_write",
    ) {
        return;
    }
    for (global, workspace) in [
        (
            "[workflow]\nsystem_crew = 'sol'\n",
            "[operation]\nreview_policy = 'after-landing'\nreview_minutes = 7\n",
        ),
        (
            "[operation]\nreview_policy = 'after-landing'\n",
            "[operation]\nreview_policy = 'none'\n",
        ),
        (
            "[operation]\nreview_policy = 'after-landing'\n",
            "[review]\ntiming = 'off'\n",
        ),
    ] {
        let started = Instant::now();
        eprintln!("scenario: global={global:?}, workspace={workspace:?}");
        let fixture = WorkCheckout::new();
        let global_dir = fixture.home.join(".orbit");
        fs::create_dir_all(&global_dir).unwrap();
        let global_path = global_dir.join("config.toml");
        let workspace_path = fixture.work.join(".orbit/config.toml");
        fs::write(&global_path, global).unwrap();
        fs::write(&workspace_path, workspace).unwrap();
        let before = effective_review(&fixture);
        let output = doctor(&fixture, &["--fix-retired-config-keys"]);
        assert!(!output.status.success(), "repair must refuse: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("orbit auto-task toggle delivery-code-review"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&global_path).unwrap(), global);
        assert_eq!(fs::read_to_string(&workspace_path).unwrap(), workspace);
        assert!(backup_names(&global_dir).is_empty());
        assert!(backup_names(&fixture.work.join(".orbit")).is_empty());
        assert_eq!(effective_review(&fixture), before);
        eprintln!("scenario elapsed: {:?}", started.elapsed());
    }
}

/// [ORB-16775] The repair writes each consolidated family's replacement into
/// the file that set the deprecated spelling, removes the old keys, and the
/// effective values `orbit config get` reports are the same before and after.
#[test]
fn doctor_retired_repair_rewrites_consolidated_key_families() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_retired_repair_rewrites_consolidated_key_families",
    ) {
        return;
    }
    const KEYS: [&str; 6] = [
        "workflow.purpose_crews.review",
        "workflow.purpose_crews.final_recovery",
        "workflow.purpose_crews.sweep_tasks",
        "review.timing",
        "machine.worker_containment",
        "workflow.validation_env.shell_probe",
    ];
    let fixture = WorkCheckout::new();
    let global_dir = fixture.home.join(".orbit");
    fs::create_dir_all(&global_dir).unwrap();
    let global_path = global_dir.join("config.toml");
    let workspace_path = fixture.work.join(".orbit/config.toml");
    let global = "[workflow]\nfinal_recovery_crews = [\"sol\"]\n\n\
                  [workflow.validation_env]\nlogin_shell = false\n\n\
                  [machine]\nworker_containment = false\n";
    let workspace = "# keep this note\n[operation]\nreview_crew = [\"sol\", \"grok\"]\n\n\
                     [review]\nbefore_landing = false\n\n\
                     [workflow.purpose_crews]\nci_failure_sweep = [\"sol\"]\n\
                     merged_head_repair = [\"sol\"]\n\n\
                     [workflow.validation_env]\ninteractive = false\n";
    fs::write(&global_path, global).unwrap();
    fs::write(&workspace_path, workspace).unwrap();
    let effective = || {
        KEYS.map(|key| {
            let output = command(&fixture)
                .args(["config", "get", key, "--json"])
                .output()
                .unwrap();
            assert!(output.status.success(), "config get {key}: {output:?}");
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["value"].clone()
        })
    };
    let before = effective();
    assert_eq!(
        before,
        [
            serde_json::json!(["grok", "sol"]),
            serde_json::json!(["sol"]),
            serde_json::json!(["sol"]),
            serde_json::json!("off"),
            serde_json::json!("off"),
            serde_json::json!("off"),
        ],
        "the deprecated spellings load"
    );

    let fixed = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
    assert_eq!(
        row(&fixed, "fix-retired-config-keys")["status"],
        "ok",
        "{fixed:?}"
    );
    assert_eq!(
        row(&fixed, "retired-config-keys")["status"],
        "ok",
        "{fixed:?}"
    );
    assert_eq!(
        effective(),
        before,
        "the repair keeps every effective value"
    );

    let read = |path: &std::path::Path| -> toml::Value {
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    let global_doc = read(&global_path);
    assert_eq!(
        global_doc["workflow"]["purpose_crews"]["final_recovery"],
        toml::Value::Array(vec!["sol".into()])
    );
    assert!(global_doc["workflow"].get("final_recovery_crews").is_none());
    assert_eq!(
        global_doc["workflow"]["validation_env"]["shell_probe"].as_str(),
        Some("off")
    );
    assert!(
        global_doc["workflow"]["validation_env"]
            .get("login_shell")
            .is_none()
    );
    assert_eq!(
        global_doc["machine"]["worker_containment"].as_str(),
        Some("off")
    );

    let workspace_doc = read(&workspace_path);
    assert_eq!(
        workspace_doc["workflow"]["purpose_crews"]["review"],
        toml::Value::Array(vec!["sol".into(), "grok".into()])
    );
    assert_eq!(
        workspace_doc["workflow"]["purpose_crews"]["sweep_tasks"],
        toml::Value::Array(vec!["sol".into()])
    );
    for legacy in ["ci_failure_sweep", "merged_head_repair"] {
        assert!(
            workspace_doc["workflow"]["purpose_crews"]
                .get(legacy)
                .is_none()
        );
    }
    assert!(
        workspace_doc
            .get("operation")
            .and_then(|v| v.get("review_crew"))
            .is_none()
    );
    assert_eq!(workspace_doc["review"]["timing"].as_str(), Some("off"));
    assert!(workspace_doc["review"].get("before_landing").is_none());
    assert_eq!(
        workspace_doc["workflow"]["validation_env"]["shell_probe"].as_str(),
        Some("off")
    );
    assert!(
        fs::read_to_string(&workspace_path)
            .unwrap()
            .contains("# keep this note"),
        "the repair keeps the operator's comment"
    );
    assert_eq!(
        fs::read_to_string(global_dir.join("config.toml.bak")).unwrap(),
        global
    );
    assert_eq!(
        fs::read_to_string(fixture.work.join(".orbit/config.toml.bak")).unwrap(),
        workspace
    );
}

fn effective_review(fixture: &WorkCheckout) -> (Value, Value) {
    let get = |key: &str| {
        let output = command(fixture)
            .args(["config", "get", key, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "config get {key}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["value"].clone()
    };
    (get("review.timing"), get("review.minutes"))
}

/// The review host rules a resolved load gives effect to, with the layer that set them.
fn effective_host_evidence(fixture: &WorkCheckout) -> (OperationLayerSource, Vec<String>) {
    let config = ResolvedConfig::load(&ConfigRoots::new(
        fixture.home.join(".orbit"),
        fixture.work.join(".orbit"),
    ))
    .expect("resolved config");
    let rules = config.operation.review_host_evidence;
    (
        rules.source,
        rules.value.into_iter().map(|rule| rule.name).collect(),
    )
}

fn mentions_key_and_file(row: &Value, key: &str, file: &std::path::Path) -> bool {
    row["message"].as_str().is_some_and(|message| {
        message.contains(key) && message.contains(&file.display().to_string())
    })
}

fn backup_names(directory: &std::path::Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .expect("config directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("config.toml.bak"))
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// `orbit doctor` reports mid-run steering's resolved settings, read through
/// both config layers, so an operator can tell whether this workspace
/// records steering watermarks.
#[test]
fn doctor_reports_the_resolved_mid_run_steering_settings() {
    let fixture = WorkCheckout::new();
    let global = fixture.home.join(".orbit");
    fs::create_dir_all(&global).expect("global root");
    fs::write(
        global.join("config.toml"),
        "[workflow.mid_run_steering]\nmax_rounds = 3\n",
    )
    .expect("global config");
    fs::write(
        fixture.work.join(".orbit/config.toml"),
        "[workflow.mid_run_steering]\nenabled = true\n",
    )
    .expect("workspace config");
    let rows = rows(&doctor(&fixture, &[]));
    let steering = row(&rows, "mid-run-steering");
    assert_eq!(steering["status"], "ok", "{steering}");
    assert!(
        steering["message"].as_str().is_some_and(|message| {
            message.contains("enabled = true") && message.contains("max_rounds = 3")
        }),
        "the workspace's enable and the global round budget both resolve: {steering}"
    );
}

#[cfg(unix)]
#[test]
fn doctor_reports_git_protection_refusals_with_their_fix() {
    let fixture = WorkCheckout::new();
    let git = fixture.work.join(".git");
    assert_eq!(
        row(&rows(&doctor(&fixture, &[])), "git-protection")["status"],
        "ok"
    );

    // Git's own interrupted write: a temporary name beside the final one.
    let object = git.join("objects/61/5bfcb2ad167392037266f1f2dfec4546b37dd6");
    fs::create_dir_all(object.parent().unwrap()).expect("object fan-out");
    fs::write(&object, "loose object").expect("object");
    fs::hard_link(&object, object.with_file_name("tmp_obj_Ab12Cd")).expect("temp leftover");
    assert_eq!(
        row(&rows(&doctor(&fixture, &[])), "git-protection")["status"],
        "ok",
        "a temp leftover paired inside the object store is not an attack"
    );

    // A hook aliased to a file outside `.git` stays refused.
    let outside = fixture.temp.path().join("outside-hook");
    fs::write(&outside, "#!/bin/sh\n").expect("outside file");
    let hook = git.join("hooks/pre-commit");
    fs::hard_link(&outside, &hook).expect("aliased hook");
    let report = rows(&doctor(&fixture, &[]));
    let refused = row(&report, "git-protection");
    assert_eq!(refused["status"], "warning");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|message| message.contains(&hook.display().to_string())),
        "the warning names the entry: {refused}"
    );
    assert!(
        refused["remediation"]
            .as_str()
            .is_some_and(|remediation| remediation.contains("git fsck")),
        "the warning names the remedy: {refused}"
    );
}

#[test]
fn fs_access_reports_the_shipped_policy_verdicts_for_workspace_relative_paths() {
    let fixture = WorkCheckout::new();
    for (path, read, modify) in [
        ("src/main.rs", true, true),
        (".orbit/tmp/x", true, true),
        (".orbit/tasks/x", true, false),
        ("src/.env", false, false),
    ] {
        let output = command(&fixture)
            .args(["doctor", "fs-access", "implementer", path, "--json"])
            .assert()
            .success()
            .get_output()
            .clone();
        let json: Value = serde_json::from_slice(&output.stdout).expect("fs-access JSON");
        assert_eq!(json["policy"], "default", "{json}");
        assert_eq!(json["profile"], "implementer", "{json}");
        assert_eq!(json["path"], path, "{json}");
        assert_eq!(json["read"]["allowed"], read, "{path}: {json}");
        assert_eq!(json["modify"]["allowed"], modify, "{path}: {json}");
        assert!(json["read"]["matched_rule"].is_string(), "{json}");
        let human = command(&fixture)
            .args(["doctor", "fs-access", "implementer", path])
            .assert()
            .success()
            .get_output()
            .clone();
        let expected = format!("modify:  {}", if modify { "allowed" } else { "denied" });
        assert!(
            String::from_utf8_lossy(&human.stdout).contains(&expected),
            "{path}: human output must carry the modify verdict"
        );
    }
}

/// An enabled hourly routine warns once its last success is past three hours,
/// and a newer skipped fire does not make that success look fresh.
#[test]
fn doctor_warns_when_routine_last_success_is_stale() {
    if crate::isolated_test::run_in_child("doctor::doctor_warns_when_routine_last_success_is_stale")
    {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("empty-home");
    let repo = temp.path().join("repo");
    let root = temp.path().join("custom-root");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&repo).expect("repo");
    crate::git_repo::SeededRepo::new(&repo)
        .readme("# routine freshness\n")
        .build();
    let root_arg = root.to_string_lossy().into_owned();
    let run = |args: &[&str]| {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ORBIT_FORMAT")
            .args(args)
            .output()
            .expect("orbit")
    };
    for args in [
        &[
            "--root",
            root_arg.as_str(),
            "init",
            "--non-interactive",
            "--machine-name",
            "freshness-host",
            "--task-prefix",
            "FR",
        ][..],
        &[
            "--root",
            root_arg.as_str(),
            "workspace",
            "init",
            "--name",
            "routine-freshness",
        ][..],
    ] {
        let output = run(args);
        assert!(
            output.status.success(),
            "orbit {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let list = run(&[
        "--root",
        root_arg.as_str(),
        "routine",
        "list",
        "--format",
        "json",
    ]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let listed: Value = serde_json::from_slice(&list.stdout).expect("routine list JSON");
    let routine = listed["routines"]
        .as_array()
        .and_then(|routines| {
            routines.iter().find(|routine| {
                routine["target"]
                    .as_str()
                    .is_some_and(|target| target.contains("worktree_gc"))
            })
        })
        .expect("seeded worktree-gc routine");
    let name = routine["name"].as_str().expect("name").to_string();
    let now = chrono::Utc::now();
    let rfc = |ago: chrono::Duration| (now - ago).to_rfc3339();
    let database = Connection::open(root.join("orbit.db")).expect("host database");
    database
        .execute(
            "INSERT INTO routine_cursors (routine_name, baseline_at, last_slot, updated_at) \
             VALUES (?1, ?2, ?2, ?2)",
            rusqlite::params![name, rfc(chrono::Duration::hours(5))],
        )
        .expect("cursor");
    database
        .execute(
            "INSERT INTO routine_fires \
             (routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at) \
             VALUES (?1, ?2, 1, 'succeeded', 'jrun-fresh', 'routine-freshness', NULL, ?3, ?4)",
            rusqlite::params![
                name,
                rfc(chrono::Duration::hours(5)),
                rfc(chrono::Duration::hours(5)),
                rfc(chrono::Duration::hours(4)),
            ],
        )
        .expect("old success");
    fn freshness(rows: &[Value]) -> &Value {
        row(rows, "routine-freshness")
    }
    let before_enable = rows(&run(&["--root", root_arg.as_str(), "doctor", "--json"]));
    assert_eq!(
        freshness(&before_enable)["status"],
        "ok",
        "a disabled routine is not stale: {}",
        freshness(&before_enable)
    );

    // With `--root`, this host keeps seeded routines in the global catalog.
    let definition = root.join("routines/worktree_gc.yaml");
    let yaml = fs::read_to_string(&definition).expect("seeded routine");
    assert!(yaml.contains("enabled: false"), "{yaml}");
    fs::write(
        &definition,
        yaml.replacen("enabled: false", "enabled: true", 1),
    )
    .expect("enable");

    let stale = rows(&run(&["--root", root_arg.as_str(), "doctor", "--json"]));
    let stale_row = freshness(&stale);
    assert_eq!(stale_row["status"], "warning", "{stale_row}");
    assert!(
        stale_row["message"]
            .as_str()
            .is_some_and(|message| message.contains(&name)),
        "the warning names the routine: {stale_row}"
    );

    database
        .execute(
            "INSERT INTO routine_fires \
             (routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at) \
             VALUES (?1, ?2, 1, 'skipped', NULL, 'routine-freshness', 'missed_run: skip', ?2, ?2)",
            rusqlite::params![name, rfc(chrono::Duration::minutes(30))],
        )
        .expect("newer skip");
    let still_stale = rows(&run(&["--root", root_arg.as_str(), "doctor", "--json"]));
    let still = freshness(&still_stale);
    assert_eq!(
        still["status"], "warning",
        "a skip is not a success: {still}"
    );
    assert!(
        still["message"]
            .as_str()
            .is_some_and(|message| message.contains(&name)),
        "{still}"
    );
    let listed = run(&[
        "--root",
        root_arg.as_str(),
        "routine",
        "list",
        "--format",
        "json",
    ]);
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("list after skip");
    let last_fire = listed["routines"]
        .as_array()
        .and_then(|routines| routines.iter().find(|routine| routine["name"] == name))
        .and_then(|routine| routine.get("last_fire"))
        .expect("last fire");
    assert_eq!(last_fire["detail"], "missed_run: skip", "{last_fire}");
    assert_eq!(last_fire["state"], "skipped", "{last_fire}");

    database
        .execute(
            "UPDATE routine_fires SET updated_at = ?1 \
             WHERE routine_name = ?2 AND state = 'succeeded'",
            rusqlite::params![rfc(chrono::Duration::hours(1)), name],
        )
        .expect("refresh success");
    let fresh = rows(&run(&["--root", root_arg.as_str(), "doctor", "--json"]));
    assert_eq!(
        freshness(&fresh)["status"],
        "ok",
        "a success inside 3× the hourly interval is ok: {}",
        freshness(&fresh)
    );
}

/// A home whose global config routes the default lane to `crew`, on `provider`.
fn routed_crew_fixture(crew: &str, provider: &str) -> WorkCheckout {
    let fixture = WorkCheckout::new();
    let init = command(&fixture)
        .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
        .args([
            "init",
            "--non-interactive",
            "--machine-name",
            "doctor-host-os",
            "--task-prefix",
            "DH",
        ])
        .output()
        .expect("init");
    assert!(init.status.success(), "{init:?}");
    let global = fixture.home.join(".orbit");
    fs::create_dir_all(&global).expect("global root");
    fs::write(
        global.join("config.toml"),
        format!(
            "[workflow]\ndefault_crew = \"{crew}\"\n\n\
             [crews.{crew}]\nprovider = \"{provider}\"\nmodel = \"fixture\"\n"
        ),
    )
    .expect("routed crew config");
    fixture
}

/// A crew whose executor `host_os` excludes this host is unsupported here, not
/// a missing install: its provider row is `skipped` with no install advice, as
/// `orbit doctor providers` reports it. The shipped Muse executor is macOS-only,
/// so every other host, Linux and Windows alike, takes this branch.
#[cfg(not(target_os = "macos"))]
#[test]
fn doctor_skips_a_crew_whose_executor_host_os_excludes_this_host() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_skips_a_crew_whose_executor_host_os_excludes_this_host",
    ) {
        return;
    }
    let fixture = routed_crew_fixture("muse-wonder", "muse");
    let parsed = rows(&doctor(&fixture, &[]));
    let crew = row(&parsed, "provider:muse-wonder");
    assert_eq!(crew["status"], "skipped", "{crew}");
    assert!(
        crew["message"]
            .as_str()
            .is_some_and(|message| message.contains("unsupported on this OS")),
        "{crew}"
    );
    assert!(
        crew["remediation"].is_null(),
        "an unsupported crew gets no install advice: {crew}"
    );
}

/// Point `provider`'s seeded executor at a launcher that cannot exist: an
/// absolute path under the disposable fixture root. An absolute command is
/// looked up only at that path, never on `PATH` or in the system prefixes, so
/// no installed CLI can satisfy it. Every other seeded field, including
/// `host_os`, stays as shipped. Returns the configured command.
fn pin_missing_launcher(fixture: &WorkCheckout, provider: &str) -> String {
    let launcher = fixture
        .temp
        .path()
        .join("absent-launchers")
        .join(provider)
        .display()
        .to_string();
    let path = fixture
        .home
        .join(".orbit/resources/executors")
        .join(format!("{provider}.yaml"));
    let mut executor: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(&path).expect("seeded executor"))
            .expect("seeded executor YAML");
    executor["spec"]["command"] = serde_yaml::Value::String(launcher.clone());
    fs::write(
        &path,
        serde_yaml::to_string(&executor).expect("pinned executor YAML"),
    )
    .expect("pinned executor");
    launcher
}

/// The complement: on macOS, where Muse runs, a missing CLI is still an error
/// with install advice. Its launcher is pinned to an absent path under the
/// fixture root, so an installed Muse in a system prefix cannot satisfy it.
#[cfg(target_os = "macos")]
#[test]
fn doctor_keeps_a_missing_cli_an_error_on_a_host_the_executor_supports() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_keeps_a_missing_cli_an_error_on_a_host_the_executor_supports",
    ) {
        return;
    }
    let fixture = routed_crew_fixture("muse-wonder", "muse");
    let launcher = pin_missing_launcher(&fixture, "muse");
    let output = command(&fixture)
        .env("PATH", fixture.temp.path())
        .args(["doctor", "--json"])
        .output()
        .expect("doctor report");
    let parsed = rows(&output);
    let crew = row(&parsed, "provider:muse-wonder");
    assert_eq!(crew["status"], "error", "{crew}");
    assert!(
        crew["message"]
            .as_str()
            .is_some_and(|message| message.contains(&format!("CLI '{launcher}' was not found"))),
        "{crew}"
    );
    assert!(
        crew["remediation"].as_str().is_some_and(
            |remediation| remediation.contains(&format!("Install the '{launcher}' CLI"))
        ),
        "{crew}"
    );
}

/// An unrestricted executor (no `host_os`) keeps its missing-CLI error on every
/// host. Its launcher is pinned to an absent path under the fixture root, so no
/// installed CLI or `PATH` entry can satisfy it.
#[test]
fn doctor_keeps_a_missing_cli_an_error_for_an_unrestricted_executor() {
    if crate::isolated_test::run_in_child(
        "doctor::doctor_keeps_a_missing_cli_an_error_for_an_unrestricted_executor",
    ) {
        return;
    }
    let fixture = routed_crew_fixture("codex-wonder", "codex");
    let launcher = pin_missing_launcher(&fixture, "codex");
    let output = command(&fixture)
        .env("PATH", fixture.temp.path())
        .args(["doctor", "--json"])
        .output()
        .expect("doctor report");
    let parsed = rows(&output);
    let crew = row(&parsed, "provider:codex-wonder");
    assert_eq!(crew["status"], "error", "{crew}");
    assert!(
        crew["message"]
            .as_str()
            .is_some_and(|message| message.contains(&format!("CLI '{launcher}' was not found"))),
        "{crew}"
    );
    assert!(
        crew["remediation"].as_str().is_some_and(
            |remediation| remediation.contains(&format!("Install the '{launcher}' CLI"))
        ),
        "{crew}"
    );
}

/// Unknown-only files must enter the existing locked, backed-up repair flow.
#[test]
fn doctor_fixes_unknown_config_without_retired_keys() {
    let fixture = WorkCheckout::new();
    let global_dir = fixture.home.join(".orbit");
    fs::create_dir_all(&global_dir).expect("global root");
    let global = global_dir.join("config.toml");
    let workspace = fixture.work.join(".orbit/config.toml");
    let global_body = "[workflow]\nbase_branc = 'stale'\nbase_branch = 'main'\n";
    let workspace_body = "# keep this note\n[workflow]\nbase_branch = 'agent-main'\n[future]\n";
    fs::write(&global, global_body).expect("global config");
    fs::write(&workspace, workspace_body).expect("workspace config");
    let diagnosis = rows(&doctor(&fixture, &[]));
    assert!(
        diagnosis.iter().any(
            |row| mentions_key_and_file(row, "workflow.base_branc", &global)
                && row["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("workflow.base_branch"))
        ),
        "{diagnosis:?}"
    );
    assert!(
        diagnosis
            .iter()
            .any(|row| mentions_key_and_file(row, "future", &workspace)),
        "{diagnosis:?}"
    );
    let repaired = rows(&doctor(&fixture, &["--fix-retired-config-keys"]));
    assert_eq!(row(&repaired, "fix-retired-config-keys")["status"], "ok");
    assert_eq!(row(&repaired, "retired-config-keys")["status"], "ok");
    assert_eq!(
        fs::read_to_string(global_dir.join("config.toml.bak")).expect("global backup"),
        global_body
    );
    assert_eq!(
        fs::read_to_string(fixture.work.join(".orbit/config.toml.bak")).expect("workspace backup"),
        workspace_body
    );
    let updated: toml::Value = fs::read_to_string(&global)
        .expect("global")
        .parse()
        .expect("TOML");
    assert_eq!(updated["workflow"]["base_branch"].as_str(), Some("main"));
    assert!(updated["workflow"].get("base_branc").is_none());
    let updated = fs::read_to_string(&workspace).expect("workspace");
    assert!(updated.contains("# keep this note"));
    assert!(
        updated
            .parse::<toml::Value>()
            .expect("TOML")
            .get("future")
            .is_none()
    );
}
