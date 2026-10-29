//! Doctor behavior through the CLI, with all Orbit state confined to disposable roots.
use crate::git_repo::WorkCheckout;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_config::{ConfigRoots, OperationLayerSource, ResolvedConfig};
use orbit_core::DOCTOR_FINDINGS_MESSAGE_PREFIX;
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    process::Output,
    time::{Duration, Instant},
};
fn command(fixture: &WorkCheckout) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command.current_dir(&fixture.work);
    command.env("HOME", &fixture.home);
    command.env("USERPROFILE", &fixture.home);
    command.env_remove("ORBIT_FORMAT");
    command
}
fn run(fixture: &WorkCheckout, args: &[&str]) -> Output {
    command(fixture).args(args).output().expect("orbit command")
}
fn run_ok(fixture: &WorkCheckout, args: &[&str]) -> Output {
    let out = run(fixture, args);
    assert!(out.status.success(), "{out:?}");
    out
}
fn doctor(fixture: &WorkCheckout, extra: &[&str]) -> Output {
    let mut cmd = command(fixture);
    cmd.args(["doctor", "--json"]).args(extra);
    cmd.output().expect("doctor report")
}
fn rows(output: &Output) -> Vec<Value> {
    let parsed = serde_json::from_slice(&output.stdout);
    parsed.unwrap_or_else(|error| panic!("doctor JSON: {error}; {output:?}"))
}
fn row<'a>(rows: &'a [Value], name: &str) -> &'a Value {
    let found = rows.iter().find(|row| row["check"] == name);
    found.expect("diagnostic row")
}
fn in_child(name: &str) -> bool {
    crate::isolated_test::run_in_child(name)
}
fn global_dir(fixture: &WorkCheckout) -> std::path::PathBuf {
    let dir = fixture.home.join(".orbit");
    mkdir(&dir);
    dir
}
type ConfigPaths = (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf);
fn write_configs(fixture: &WorkCheckout, global: &str, workspace: &str) -> ConfigPaths {
    let dir = global_dir(fixture);
    let global_path = dir.join("config.toml");
    let workspace_path = fixture.work.join(".orbit/config.toml");
    fs::write(&global_path, global).expect("global config");
    fs::write(&workspace_path, workspace).expect("workspace config");
    (dir, global_path, workspace_path)
}
fn fix_rows(fixture: &WorkCheckout) -> Vec<Value> {
    rows(&doctor(fixture, &["--fix-retired-config-keys"]))
}
fn assert_fix_ok(rows: &[Value]) {
    expect_status(rows, "fix-retired-config-keys", "ok");
}
fn config_value(fixture: &WorkCheckout, key: &str) -> Value {
    let output = run(fixture, &["config", "get", key, "--json"]);
    let ok = output.status.success();
    assert!(ok, "config get {key}: {output:?}");
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["value"].clone()
}
fn read_toml(path: &std::path::Path) -> toml::Value {
    toml::from_str(&fs::read_to_string(path).expect("read config")).expect("config TOML")
}
fn expect_mentions(rows: &[&Value], key: &str, file: &std::path::Path) {
    let found = rows.iter().any(|r| mentions_key_and_file(r, key, file));
    assert!(found, "missing {key} in {}: {rows:?}", file.display());
}
fn expect_no_mentions(rows: &[&Value], key: &str, file: &std::path::Path) {
    let found = rows.iter().any(|r| mentions_key_and_file(r, key, file));
    assert!(!found, "unexpected {key} in {}: {rows:?}", file.display());
}
fn retired_rows(rows: &[Value]) -> Vec<&Value> {
    rows.iter()
        .filter(|r| r["check"] == "retired-config-keys")
        .collect()
}
fn assert_backup(directory: &std::path::Path, original: &str) {
    assert_eq!(backup_names(directory), vec!["config.toml.bak".to_string()]);
    expect_file(&directory.join("config.toml.bak"), original);
}
fn assert_repair_settled(
    fixture: &WorkCheckout,
    paths: &[&std::path::Path],
    dirs: &[&std::path::Path],
) -> Vec<Value> {
    let settled: Vec<Vec<u8>> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
    let backups: Vec<Vec<String>> = dirs.iter().map(|dir| backup_names(dir)).collect();
    let again = fix_rows(fixture);
    assert_fix_ok(&again);
    for (path, before) in paths.iter().zip(settled) {
        expect_bytes(path, &before);
    }
    for (dir, before) in dirs.iter().zip(backups) {
        assert_eq!(backup_names(dir), before, "{}", dir.display());
    }
    again
}
fn expect_bytes(path: &std::path::Path, expected: &[u8]) {
    assert_eq!(fs::read(path).unwrap(), expected, "{}", path.display());
}
fn expect_review(fixture: &WorkCheckout, expected: &(Value, Value), ctx: &str) {
    assert_eq!(effective_review(fixture), *expected, "{ctx}");
}
fn expect_status(rows: &[Value], name: &str, status: &str) {
    let actual = &row(rows, name)["status"];
    assert_eq!(actual, status, "{name}: {}", row(rows, name));
}
fn msg_has(row: &Value, needle: &str) {
    let found = row["message"].as_str().is_some_and(|m| m.contains(needle));
    assert!(found, "{row}: missing {needle}");
}
fn fix_has(row: &Value, needle: &str) {
    let found = row["remediation"]
        .as_str()
        .is_some_and(|m| m.contains(needle));
    assert!(found, "{row}: missing fix {needle}");
}
#[cfg(unix)]
fn expect_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let actual = fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(actual, mode, "{}", path.display());
}
#[cfg(unix)]
fn expect_link(link: &std::path::Path, target: &std::path::Path) {
    assert_eq!(fs::read_link(link).unwrap(), target, "{}", link.display());
}
fn read_str(path: &std::path::Path) -> String {
    fs::read_to_string(path).expect("read fixture file")
}
fn mkdir(path: &std::path::Path) {
    fs::create_dir_all(path).expect("create fixture dir");
}
fn put(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("write fixture file");
}
fn has(haystack: &str, needle: &str) {
    assert!(haystack.contains(needle), "{haystack}: missing {needle}");
}
fn expect_file(path: &std::path::Path, contents: &str) {
    assert_eq!(read_str(path), contents, "{}", path.display());
}
fn utf8(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("UTF-8 output")
}
#[cfg(unix)]
fn link(target: &std::path::Path, path: &std::path::Path) {
    std::os::unix::fs::symlink(target, path).expect("create link");
}
#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
}
/// Admission receipts: expose storage and compaction without broad checks.
#[test]
fn doctor_admission_receipts_reports_storage_and_compacts() {
    if in_child("doctor::doctor_admission_receipts_reports_storage_and_compacts") {
        return;
    }
    let f = WorkCheckout::new();
    run_ok(&f, &["workspace", "init", "--name", "receipt-maintenance"]);
    for extra in [vec![], vec!["--compact"]] {
        let mut cmd = command(&f);
        cmd.args(["doctor", "admission-receipts", "--json"])
            .args(extra);
        let out = cmd.output().expect("receipt diagnostic");
        assert!(out.status.success(), "{out:?}");
        let report: Value = serde_json::from_slice(&out.stdout).expect("receipt JSON");
        assert_eq!(report["compacted_receipts"], 0, "{report}");
        assert_eq!(report["storage"]["full_receipts"]["count"], 0, "{report}");
        let v = report["storage"]["tombstones"]["logical_payload_bytes"].clone();
        assert_eq!(v, 0, "{report}");
        let v = report["storage"]["claim_mutation_receipts"]["count"].clone();
        assert_eq!(v, 0, "{report}");
    }
}
/// ORB-16351: `clock-unit: ok` requires an enabled, active systemd timer.
#[cfg(target_os = "linux")]
#[test]
fn doctor_clock_unit_requires_an_enabled_active_systemd_timer() {
    if in_child("doctor::doctor_clock_unit_requires_an_enabled_active_systemd_timer") {
        return;
    }
    for mode in ["disabled", "inactive", "enabled"] {
        let f = WorkCheckout::new();
        install_matching_sweep_unit(&f.home);
        let bin = write_substitute_systemctl(f.temp.path());
        let path_var = {
            let mut e = vec![bin.clone()];
            if let Some(p) = std::env::var_os("PATH") {
                e.extend(std::env::split_paths(&p));
            }
            std::env::join_paths(e).expect("join PATH")
        };
        let mut cmd = command(&f);
        cmd.env("PATH", path_var)
            .env("ORBIT_TEST_CLOCK_MANAGER", mode)
            .args(["doctor", "--json"]);
        let out = cmd.output().expect("doctor");
        let parsed = rows(&out);
        let clock = row(&parsed, "clock-unit");
        if mode == "enabled" {
            assert_eq!(clock["status"], "ok", "{mode}: {clock}");
        } else {
            assert_ne!(clock["status"], "ok", "{mode}: {clock}");
            msg_has(clock, "orbit clock enable");
        }
    }
}
#[cfg(target_os = "linux")]
fn install_matching_sweep_unit(home: &std::path::Path) {
    let unit_dir = home.join(".config/systemd/user");
    mkdir(&unit_dir);
    let program = env!("CARGO_BIN_EXE_orbit")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let unit = format!(
        "[Service]\nType=oneshot\nTimeoutStartSec=10min\nKillMode=mixed\nExecStart=\"{program}\" clock tick\n"
    );
    put(&unit_dir.join("orbit-sweep.service"), &unit);
}
#[cfg(target_os = "linux")]
fn write_substitute_systemctl(root: &std::path::Path) -> std::path::PathBuf {
    let bin = root.join("bin");
    mkdir(&bin);
    let path = bin.join("systemctl");
    let script = "#!/bin/sh\nmode=${ORBIT_TEST_CLOCK_MANAGER:-enabled}\ncase \" $* \" in *\" is-enabled \"*) if [ \"$mode\" = \"disabled\" ]; then echo disabled; exit 1; fi; echo enabled; exit 0;; esac\nfor arg in \"$@\"; do case \"$arg\" in orbit-sweep.timer) if [ \"$mode\" = \"disabled\" ] || [ \"$mode\" = \"inactive\" ]; then printf '%s\\n' LoadState=loaded ActiveState=inactive; else printf '%s\\n' LoadState=loaded ActiveState=active 'NextElapseUSecRealtime=Mon 2026-10-19 12:00:00 UTC' NextElapseUSecMonotonic=1000000; fi; exit 0;; orbit-sweep.service) printf '%s\\n' ActiveState=inactive Result=success TimeoutStartUSec=10min KillMode=mixed; exit 0;; esac; done\nexit 0\n";
    put(&path, script);
    set_mode(&path, 0o755);
    bin
}
#[cfg(unix)]
#[test]
fn doctor_warns_about_selected_root_skill_links_without_changing_discovery_entries() {
    use std::path::PathBuf;
    let f = WorkCheckout::new();
    let base = f.temp.path().join("selected");
    let global = base.join(".orbit");
    let live = base.join("live-skill");
    mkdir(&live);
    put(&live.join("SKILL.md"), "live skill bytes");
    let roots = [base.join(".agents/skills"), base.join(".claude/skills")];
    for root in &roots {
        mkdir(&root.join("regular-skill"));
        put(&root.join("regular-skill/notes.txt"), "operator bytes");
        link(&live, &root.join("live-skill"));
        // Only immediate discovery links are in scope; nested content is operator-owned.
        link(
            &base.join("missing-nested"),
            &root.join("regular-skill/nested"),
        );
    }
    let home_link = f.home.join(".claude/skills/unselected-skill");
    mkdir(home_link.parent().unwrap());
    let home_target = f.home.join("missing-skill");
    link(&home_target, &home_link);
    let invoke = |args: &[&str]| {
        command(&f)
            .arg("--root")
            .arg(&global)
            .args(args)
            .output()
            .expect("doctor")
    };
    let mut init = command(&f);
    init.arg("--root")
        .arg(&global)
        .args(
            "init --non-interactive --machine-name doctor-fixture --task-prefix DF"
                .split_whitespace(),
        )
        .assert()
        .success();
    let initial = rows(&invoke(&["doctor", "--json"]));
    expect_status(&initial, "artifacts-skills", "ok");
    let links = [roots[0].join("leaked-skill"), roots[1].join("leaked-skill")];
    let targets = [
        base.join("missing-skill"),
        PathBuf::from("../../missing-skill"),
    ];
    for (l, t) in links.iter().zip(&targets) {
        link(t, l);
    }
    let plain = invoke(&["doctor", "--format", "plain"]);
    let stdout = utf8(plain.stdout);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("artifacts-skills\t"))
        .expect("skills row");
    assert_eq!(line.split('\t').nth(1), Some("warning"), "{line}");
    for (l, t) in links.iter().zip(&targets) {
        has(line, &l.display().to_string());
        let is_link = fs::symlink_metadata(l).unwrap().file_type().is_symlink();
        assert!(is_link, "preserve links: {l:?}");
        expect_link(l, t);
    }
    let diagnostics = rows(&invoke(&["doctor", "--json"]));
    expect_status(&diagnostics, "artifacts-skills", "warning");
    let warning = row(&diagnostics, "artifacts-skills");
    let message = warning["message"].as_str().expect("skills warning message");
    let fix = warning["remediation"].as_str().expect("manual fix-it");
    assert!(!fix.is_empty() && fix.lines().count() == 1, "{fix}");
    has(line, message);
    has(line, fix);
    for l in &links {
        has(message, &l.display().to_string());
        has(fix, &l.display().to_string());
    }
    let skill_rows = rows(&invoke(&["skill", "doctor", "--json"]));
    for (l, t) in links.iter().zip(&targets) {
        let found = skill_rows.iter().find(|r| {
            r["message"]
                .as_str()
                .unwrap()
                .contains(&l.display().to_string())
        });
        let found = found.expect("skill doctor link diagnostic");
        assert_eq!(found["status"], "warning", "{found}");
        expect_link(l, t);
    }
    for root in &roots {
        expect_link(&root.join("live-skill"), &live);
        assert!(root.join("regular-skill").is_dir());
        expect_file(&root.join("regular-skill/notes.txt"), "operator bytes");
        expect_link(
            &root.join("regular-skill/nested"),
            &base.join("missing-nested"),
        );
    }
    expect_file(&live.join("SKILL.md"), "live skill bytes");
    expect_link(&home_link, &home_target);
    for l in &links {
        fs::remove_file(l).expect("remove dangling link");
    }
    let healthy = rows(&invoke(&["doctor", "--json"]));
    expect_status(&healthy, "artifacts-skills", "ok");
    let healthy_plain = invoke(&["doctor", "--format", "plain"]);
    let text = utf8(healthy_plain.stdout);
    let healthy_line = text
        .lines()
        .any(|l| l.starts_with("artifacts-skills\tok\t"));
    assert!(healthy_line, "{text}");
}
/// Corrupt an unrelated B-tree page, leaving the header/schema ledger readable.
fn corrupt_unrelated_data_page(fixture: &WorkCheckout) {
    let database = fixture.home.join(".orbit/orbit.db");
    let offset = {
        let db = Connection::open(&database).expect("fixture DB");
        db.execute_batch("CREATE TABLE doctor_probe(value TEXT); INSERT INTO doctor_probe VALUES('data'); PRAGMA wal_checkpoint(TRUNCATE);").expect("fixture data page");
        let page: u64 = db
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = 'doctor_probe'",
                [],
                |r| r.get(0),
            )
            .expect("table root page");
        let page_size: u64 = db
            .query_row("PRAGMA page_size", [], |r| r.get(0))
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
}
#[test]
fn default_doctor_checks_schema_and_deep_detects_corrupt_data() {
    if in_child("doctor::default_doctor_checks_schema_and_deep_detects_corrupt_data") {
        return;
    }
    let f = WorkCheckout::new();
    let initial = rows(&doctor(&f, &[]));
    expect_status(&initial, "database", "ok");
    corrupt_unrelated_data_page(&f);
    let cheap = rows(&doctor(&f, &[]));
    expect_status(&cheap, "database", "ok");
    let deep = doctor(&f, &["--deep"]);
    let code = deep.status.code();
    assert_eq!(code, Some(1), "deep failure exit: {deep:?}");
    let deep_rows = rows(&deep);
    expect_status(&deep_rows, "database", "error");
    for diagnostics in [&cheap, &deep_rows] {
        assert!(!diagnostics.is_empty());
        for d in diagnostics {
            let has_duration = d["duration_ms"].as_u64().is_some();
            assert!(has_duration, "duration_ms missing: {d}");
        }
    }
    let listed = run(&f, &["audit", "list", "--limit", "50", "--json"]);
    let events: Vec<Value> = serde_json::from_slice(&listed.stdout)
        .unwrap_or_else(|e| panic!("audit JSON: {e}; {listed:?}"));
    let failed = events
        .iter()
        .find(|e| e["command"] == "doctor" && e["exit_code"] == 1);
    let failed = failed.unwrap_or_else(|| panic!("failed doctor audit row missing: {events:?}"));
    let message = failed["error_message"].as_str().unwrap_or_default();
    // Provider checks also fail without crews' CLIs, so name `database` among failures.
    let names = message
        .strip_prefix(DOCTOR_FINDINGS_MESSAGE_PREFIX)
        .and_then(|x| x.split_once('('))
        .and_then(|(_, r)| r.split_once(')'))
        .map(|(n, _)| n)
        .unwrap_or_default();
    let named = names.split(", ").any(|n| n == "database");
    assert!(named, "audit must name database: {failed}");
    let table = run(&f, &["doctor", "--deep", "--format", "table"]);
    assert_eq!(table.status.code(), Some(1));
    let stdout = utf8(table.stdout);
    let (_, findings) = stdout.split_once("Findings:").expect("CLI findings");
    // An `info` row is a fact about the setup, not a finding.
    for d in deep_rows
        .iter()
        .filter(|d| d["status"] != "ok" && d["status"] != "info")
    {
        has(findings, d["check"].as_str().unwrap());
        if let Some(fix) = d["remediation"].as_str() {
            has(findings, &format!("Fix: {fix}"));
        }
    }
    let db_line = |out: Output| {
        utf8(out.stdout)
            .lines()
            .find(|l| l.starts_with("database\t"))
            .unwrap()
            .to_string()
    };
    let piped = run(&f, &["doctor", "--deep"]);
    let explicit = run(&f, &["doctor", "--deep", "--format", "plain"]);
    assert_eq!(db_line(piped), db_line(explicit));
}
#[cfg(unix)]
#[test]
fn permission_remediation_repairs_legacy_state_and_preserves_excluded_trees() {
    use std::os::unix::fs::symlink;
    let f = WorkCheckout::new();
    doctor(&f, &[]);
    let global = f.home.join(".orbit");
    let workspace = f.work.join(".orbit");
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
        mkdir(path);
        set_mode(path, 0o775);
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
        f.temp.path().join("outside"),
    ];
    for path in &excluded {
        mkdir(path);
        set_mode(path, 0o777);
    }
    let cache = global.join("cache");
    if cache.exists() {
        fs::rename(&cache, f.temp.path().join("retained-cache")).expect("retain cache");
    }
    symlink(excluded.last().unwrap(), &cache).expect("linked scan subtree root");
    symlink(excluded.last().unwrap(), global.join("state/linked")).expect("external link");
    let file = global.join("state/legacy-file");
    put(&file, "retained");
    set_mode(&file, 0o666);
    let before = rows(&doctor(&f, &[]));
    expect_status(&before, "state-directory-permissions", "warning");
    let warning = row(&before, "state-directory-permissions");
    let remediation = warning["remediation"]
        .as_str()
        .expect("runnable remediation");
    let runnable = remediation.split('`').nth(1).expect("repair command");
    let args: Vec<&str> = runnable
        .strip_prefix("orbit ")
        .expect("Orbit repair")
        .split_whitespace()
        .collect();
    let mut repair_args = args.clone();
    repair_args.push("--json");
    let repaired = run(&f, &repair_args);
    let after = rows(&repaired);
    expect_status(&after, "fix-state-directory-permissions", "ok");
    expect_status(&after, "state-directory-permissions", "ok");
    for path in &legacy {
        expect_mode(path, 0o700);
    }
    for path in &excluded {
        expect_mode(path, 0o777);
    }
    expect_mode(&file, 0o666);
    expect_file(&file, "retained");
    let repeated = rows(&doctor(&f, &["--fix-state-directory-permissions"]));
    expect_status(&repeated, "state-directory-permissions", "ok");
    expect_status(&repeated, "fix-state-directory-permissions", "ok");
    for path in &excluded {
        expect_mode(path, 0o777);
    }
}
/// Manual performance fixture: 1 GiB SQLite table and 50k directories; keep out of routine CI.
#[test]
#[ignore = "large doctor performance fixture; run with --ignored --nocapture"]
fn default_doctor_large_store_and_worktrees_under_five_seconds() {
    if in_child("doctor::default_doctor_large_store_and_worktrees_under_five_seconds") {
        return;
    }
    let f = WorkCheckout::new();
    let initial = rows(&doctor(&f, &[]));
    expect_status(&initial, "database", "ok");
    let database = f.home.join(".orbit/orbit.db");
    {
        let db = Connection::open(&database).expect("fixture DB");
        let inserts = "INSERT INTO doctor_large VALUES(zeroblob(268435456)); ".repeat(4);
        db.execute_batch(&format!("PRAGMA synchronous=OFF; PRAGMA wal_autocheckpoint=0; CREATE TABLE doctor_large(value BLOB); BEGIN; {inserts} COMMIT; PRAGMA wal_checkpoint(TRUNCATE);")).expect("real 1 GiB database pages");
    }
    let bytes = fs::metadata(&database).expect("database size").len();
    assert!(bytes >= 1 << 30, "{bytes}");
    // Restrict only the small Orbit-owned fixture tree before adding writable excluded contents.
    #[cfg(unix)]
    for root in [f.home.join(".orbit"), f.work.join(".orbit")] {
        restrict_fixture_directories(&root);
    }
    let worktrees = f.work.join(".orbit/state/worktrees");
    let target = worktrees.join("run/target");
    for index in 0..50_000 {
        let path = if index < 10_000 {
            target.join(index.to_string())
        } else {
            worktrees.join(format!("checkout-{index}"))
        };
        mkdir(&path);
        #[cfg(unix)]
        set_mode(&path, 0o777);
    }
    #[cfg(unix)]
    for container in [
        &f.work.join(".orbit/state"),
        &worktrees,
        &worktrees.join("run"),
    ] {
        set_mode(container, 0o700);
    }
    // Initialize the settled generation so timing measures doctor, not rehashing a debug image.
    let init = run(&f, &["config", "set", "review.timing", "off"]);
    let init_ok = init.status.success();
    assert!(init_ok, "fixture config writer failed: {init:?}");
    let start = Instant::now();
    let output = doctor(&f, &[]);
    let elapsed = start.elapsed();
    let diagnostics = rows(&output);
    expect_status(&diagnostics, "database", "ok");
    expect_status(&diagnostics, "state-directory-permissions", "ok");
    if let Some(scratch) = std::env::var_os("ORBIT_SCRATCH_DIR") {
        let report = serde_json::json!({"database_bytes": bytes, "worktree_directories": 50000, "target_directories": 10000, "elapsed_ms": elapsed.as_millis(), "checks": diagnostics});
        fs::write(
            std::path::PathBuf::from(scratch).join("doctor-performance.json"),
            serde_json::to_vec_pretty(&report).expect("benchmark JSON"),
        )
        .expect("record benchmark");
    }
    let fast = elapsed < Duration::from_secs(5);
    assert!(
        fast,
        "default doctor took {elapsed:?} with 1 GiB DB and 50k dirs"
    );
}
#[cfg(unix)]
fn restrict_fixture_directories(path: &std::path::Path) {
    set_mode(path, 0o700);
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
    let stdout = utf8(output.stdout);
    let after = stdout.split_once("BEGIN_DOCTOR\n").expect("start marker").1;
    let (body, _) = after.split_once("END_DOCTOR").expect("end marker");
    body.to_string()
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
    let make =
        |check: &str, status: Status, message: &str, fix: Option<&str>| WorkspaceDoctorResult {
            check_name: check.into(),
            status,
            message: message.into(),
            remediation: fix.map(Into::into),
            duration_ms: 0,
        };
    let mut rows = vec![
        make(
            "index",
            Status::Warning,
            WARNING_MESSAGE,
            Some("orbit index refresh"),
        ),
        make("database", Status::Ok, "Database is healthy", None),
        make("review", Status::Error, ERROR_MESSAGE, Some(FIX)),
    ];
    if case == "ok" {
        for r in &mut rows {
            r.status = Status::Ok;
            r.remediation = None;
        }
    }
    let mut table = Table::new(vec![
        Column::new("CHECK").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("DETAILS"),
    ]);
    for r in &rows {
        let status = match r.status {
            Status::Ok => "OK",
            Status::Warning => "WARN",
            Status::Error => "ERROR",
            Status::Skipped => "SKIP",
            Status::Info => "INFO",
        };
        let detail = r.remediation.as_ref().map_or_else(
            || r.message.clone(),
            |fix| format!("{} | Action: {fix}", r.message),
        );
        table.add_row(vec![
            comfy_table::Cell::new(&r.check_name),
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
    let columns = if case == "wide" {
        "240"
    } else if case == "narrow" {
        "32"
    } else {
        "48"
    };
    let sink = OutputSink::resolve(
        true,
        &SinkEnv {
            columns: Some(columns.into()),
            no_color: (case != "plain").then(|| "1".into()),
            ..SinkEnv::default()
        },
        None,
        Some(mode),
        false,
    );
    if case == "plain" {
        assert!(!sink.color_allowed(), "explicit plain must disable styling");
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
    let n = table.lines().count();
    assert_eq!(n, 4, "one-line rows: {table}");
    has(table, "database  OK");
    assert!(table.contains('…'), "fixture must truncate: {table}");
    let ordered = findings.find("review  ERROR").unwrap() < findings.find("index  WARN").unwrap();
    assert!(ordered, "{findings}");
    let collapsed = findings.split_whitespace().collect::<Vec<_>>().join(" ");
    for message in [ERROR_MESSAGE, WARNING_MESSAGE] {
        has(&collapsed, message);
    }
    has(findings, "Fix: orbit index refresh");
    has(
        findings,
        "Fix: orbit auto-task recover\n  delivery-code-review",
    );
    assert!(!findings.contains("database"), "{findings}");
    for line in findings.lines().filter(|l| !l.starts_with("Table details")) {
        assert!(line.chars().count() <= 48, "line exceeds width: {line}");
    }
    has(findings, "--format plain or --json");
    let narrow = render_fixture("narrow");
    assert!(
        !narrow.lines().any(|l| l == format!("Fix: {FIX}")),
        "over-width action must wrap: {narrow}"
    );
    has(
        &narrow,
        "Fix: orbit auto-task recover\n  delivery-code-review",
    );
    for line in narrow
        .lines()
        .filter(|l| !l.starts_with("Findings:") && !l.starts_with("Table details"))
    {
        assert!(
            line.chars().count() <= 32,
            "line exceeds narrow width: {line}"
        );
    }
    let wide = render_fixture("wide");
    has(&wide, ERROR_MESSAGE);
    assert!(
        wide.lines().any(|l| l == format!("Fix: {FIX}")),
        "sufficient width keeps action on one line: {wide}"
    );
    assert!(!wide.contains("Table details shortened"), "{wide}");
    let ok = render_fixture("ok");
    assert_eq!(ok.lines().count(), 4, "{ok}");
    assert!(
        !ok.contains("Findings:") && !ok.contains("--format plain"),
        "{ok}"
    );
}
#[test]
fn findings_leave_plain_and_json_documents_unchanged() {
    let plain = render_fixture("plain");
    assert_eq!(plain.lines().count(), 3, "{plain}");
    has(
        &plain,
        &format!("review\tERROR\t{ERROR_MESSAGE} | Action: {FIX}"),
    );
    assert!(!plain.contains("Findings:"), "{plain}");
    let json: Value = serde_json::from_str(&render_fixture("json")).expect("JSON document");
    assert_eq!(json[2]["message"], ERROR_MESSAGE, "{json}");
    assert_eq!(json[2]["remediation"], FIX, "{json}");
    assert_eq!(json.as_array().unwrap().len(), 3, "{json}");
}
/// Doctor names the worker CPU ceiling, or says there is none [ORB-15196].
#[cfg(target_os = "linux")]
#[test]
fn doctor_reports_the_worker_cpu_quota() {
    let f = WorkCheckout::new();
    let unset_rows = rows(&doctor(&f, &[]));
    let unset = row(&unset_rows, "worker-containment");
    assert_eq!(unset["status"], "info", "{unset}");
    msg_has(unset, "CPUQuota is not set");
    let global = global_dir(&f);
    put(
        &global.join("config.toml"),
        "[machine]\nworker_cpu_quota = 300\n",
    );
    let set_rows = rows(&doctor(&f, &[]));
    let set = row(&set_rows, "worker-containment");
    assert_eq!(set["status"], "ok", "{set}");
    msg_has(set, "CPUQuota=300%");
}
/// A retired key is reported with its file; the fix removes only it, backs up once, and is idempotent.
#[test]
fn doctor_lists_a_retired_config_key_and_the_fix_removes_only_that_key() {
    let f = WorkCheckout::new();
    let global_body = "[workflow]\nbase_branch = \"main\"\n";
    let workspace_body = "# keep this note\n[workflow]\nbase_branch = \"agent-main\"\nsystem_crew = \"sol\"\n\n[plugin]\nlegacy_callback_identity = true\n\n[scoring]\nenabled = false\n\n[task.approval]\ndelegate_approval = true\n\n[workflow.future]\nenabled = true\n\n[plugins.future]\narbitrary = { nested = true }\n\n[future]\n";
    let (_, gc, wc) = write_configs(&f, global_body, workspace_body);
    let reported = doctor(&f, &[]);
    expect_file(&wc, workspace_body);
    expect_file(&gc, global_body);
    assert!(
        backup_names(&f.work.join(".orbit")).is_empty(),
        "diagnosis must not write a backup"
    );
    let reported_rows = rows(&reported);
    let retired: Vec<&Value> = retired_rows(&reported_rows)
        .into_iter()
        .filter(|r| r["status"] == "warning")
        .collect();
    for key in [
        "workflow.system_crew",
        "scoring.enabled",
        "task.approval",
        "workflow.future",
        "future",
    ] {
        assert!(
            retired.iter().any(|r| mentions_key_and_file(r, key, &wc)),
            "doctor names {key} and its file: {reported_rows:?}"
        );
    }
    let callback = retired.iter().any(|r| {
        mentions_key_and_file(r, "plugin.legacy_callback_identity", &wc)
            && r["message"].as_str().is_some_and(|m| m.contains("removed"))
    });
    assert!(
        callback,
        "retired callback reported as removed: {reported_rows:?}"
    );
    expect_no_mentions(&retired, "plugins.future", &wc);
    for r in &retired {
        fix_has(r, "--fix-retired-config-keys");
    }
    assert!(
        !reported_rows
            .iter()
            .any(|r| r["check"] == "fix-retired-config-keys"),
        "report does not run repair"
    );
    let fixed = fix_rows(&f);
    assert_fix_ok(&fixed);
    let repair = row(&fixed, "fix-retired-config-keys");
    for needle in [
        "workflow.system_crew",
        "scoring.enabled",
        "plugin.legacy_callback_identity",
    ] {
        msg_has(repair, needle);
    }
    msg_has(repair, &wc.display().to_string());
    let updated = read_str(&wc);
    has(&updated, "# keep this note");
    has(&updated, "base_branch = \"agent-main\"");
    for gone in [
        "legacy_callback_identity",
        "system_crew",
        "enabled",
        "[future]",
        "delegate_approval",
    ] {
        assert!(
            !updated.contains(gone),
            "repair removes only retired keys ({gone}): {updated}"
        );
    }
    let parsed: Value = serde_json::to_value(read_toml(&wc)).expect("json config");
    let nested = parsed["plugins"]["future"]["arbitrary"]["nested"].clone();
    assert_eq!(nested, true, "plugin config survives");
    expect_file(&gc, global_body);
    assert_backup(&f.work.join(".orbit"), workspace_body);
    expect_status(&fixed, "retired-config-keys", "ok");
    let again = assert_repair_settled(&f, &[&wc, &gc], &[&f.work.join(".orbit")]);
    let again_repair = row(&again, "fix-retired-config-keys");
    msg_has(
        again_repair,
        "No retired, deprecated or unknown config keys",
    );
    let bak = f.work.join(".orbit/config.toml.bak");
    expect_bytes(&bak, workspace_body.as_bytes());
}
/// Live `[[review.host_evidence]]` rules survive repair in each layer; only neighbouring unknown keys are removed.
#[test]
fn doctor_retired_repair_keeps_host_evidence_rules_in_each_layer() {
    if in_child("doctor::doctor_retired_repair_keeps_host_evidence_rules_in_each_layer") {
        return;
    }
    const SEATBELT: &str = "[[review.host_evidence]]\nkind = \"host_sandbox_test\"\nname = \"Seatbelt\"\npaths = [\"crates/orbit-exec/**\"]\nos = \"macos\"\ncommand = \"cargo test -p orbit-exec --test seatbelt\"\nartifact = \"evidence/seatbelt-macos.json\"\n";
    const CODEQL: &str = "[[review.host_evidence]]\nkind = \"codeql\"\nname = \"Rust CodeQL (Linux)\"\npaths = [\"**/*.rs\"]\nos = \"linux\"\ncommand = \"scripts/codeql-rust-local.sh\"\nartifact = \"evidence/codeql.json\"\n";
    for (workspace_body, expected) in [
        (
            format!("[review]\nreviewers = 3\n{CODEQL}"),
            vec!["Rust CodeQL (Linux)"],
        ),
        (
            "[review]\nhost_evidence = []\nreviewers = 3\n".to_string(),
            vec![],
        ),
    ] {
        let f = WorkCheckout::new();
        let global_body = format!("[review]\nreviewers = 2\n{SEATBELT}");
        let (gd, gc, wc) = write_configs(&f, &global_body, &workspace_body);
        let wd = f.work.join(".orbit");
        let before = effective_host_evidence(&f);
        assert_eq!(before.0, OperationLayerSource::Workspace, "before repair");
        assert_eq!(before.1, expected, "before repair");
        let reported = rows(&doctor(&f, &[]));
        let retired = retired_rows(&reported);
        assert!(
            retired.iter().all(|r| !r["message"]
                .as_str()
                .unwrap_or_default()
                .contains("review.host_evidence")),
            "live rules not unknown: {reported:?}"
        );
        for config in [&gc, &wc] {
            expect_mentions(&retired, "review.reviewers", config);
        }
        let fixed = fix_rows(&f);
        assert_fix_ok(&fixed);
        assert_eq!(effective_host_evidence(&f), before, "survive repair");
        for (path, original) in [(&gc, &global_body), (&wc, &workspace_body)] {
            let original_doc: toml::Value = toml::from_str(original).expect("original TOML");
            assert_eq!(
                read_toml(path)["review"]["host_evidence"],
                original_doc["review"]["host_evidence"],
                "{}",
                path.display()
            );
            assert!(
                read_toml(path)["review"].get("reviewers").is_none(),
                "{}",
                path.display()
            );
        }
        assert_backup(&gd, &global_body);
        assert_backup(&wd, &workspace_body);
        let again = assert_repair_settled(&f, &[&gc, &wc], &[&gd, &wd]);
        assert_fix_ok(&again);
        assert_eq!(effective_host_evidence(&f), before, "repeat repair");
    }
}
/// The repair preserves translated review values in their original layer and keeps the first backup.
#[test]
#[allow(clippy::print_stderr)] // Name each scenario and its elapsed time in isolated child failure output.
fn doctor_retired_review_repair_preserves_effective_policy() {
    if in_child("doctor::doctor_retired_review_repair_preserves_effective_policy") {
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
        let f = WorkCheckout::new();
        let (gd, gp, wp) = write_configs(&f, global, workspace);
        let wd = f.work.join(".orbit");
        let expected = (Value::from(timing), Value::from(minutes));
        expect_review(&f, &expected, "before repair");
        let fixed = fix_rows(&f);
        assert_fix_ok(&fixed);
        expect_review(&f, &expected, "after repair");
        for (path, original) in [(&gp, global), (&wp, workspace)] {
            let doc = read_toml(path);
            for key in ["review_policy", "review_minutes"] {
                assert!(
                    doc.get("operation").and_then(|v| v.get(key)).is_none(),
                    "{key} removed: {}",
                    path.display()
                );
            }
            if original.contains("[operation]") {
                expect_file(&path.with_extension("toml.bak"), original);
            } else {
                expect_file(path, original);
            }
        }
        let again = assert_repair_settled(&f, &[&gp, &wp], &[&gd, &wd]);
        assert_fix_ok(&again);
        assert_eq!(effective_review(&f), expected, "after repeat repair");
        for (path, original) in [(&gp, global), (&wp, workspace)] {
            if original.contains("[operation]") {
                expect_file(&path.with_extension("toml.bak"), original);
            }
        }
        eprintln!("scenario elapsed: {:?}", started.elapsed());
    }
}
/// An after-landing policy requires operator migration; refusal precedes every write and backup.
#[test]
#[allow(clippy::print_stderr)] // Name each scenario and its elapsed time in isolated child failure output.
fn doctor_retired_review_repair_refuses_after_landing_before_any_write() {
    if in_child("doctor::doctor_retired_review_repair_refuses_after_landing_before_any_write") {
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
        let f = WorkCheckout::new();
        let (gd, gp, wp) = write_configs(&f, global, workspace);
        let before = effective_review(&f);
        let output = doctor(&f, &["--fix-retired-config-keys"]);
        assert!(!output.status.success(), "repair must refuse: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        has(&error, "orbit auto-task toggle delivery-code-review");
        expect_file(&gp, global);
        expect_file(&wp, workspace);
        assert!(
            backup_names(&gd).is_empty(),
            "global={global:?} workspace={workspace:?}"
        );
        assert!(
            backup_names(&f.work.join(".orbit")).is_empty(),
            "global={global:?} workspace={workspace:?}"
        );
        assert_eq!(effective_review(&f), before, "refused scenario");
        eprintln!("scenario elapsed: {:?}", started.elapsed());
    }
}
/// [ORB-16775] Consolidated families are rewritten in the file that set them; effective values are unchanged.
#[test]
fn doctor_retired_repair_rewrites_consolidated_key_families() {
    if in_child("doctor::doctor_retired_repair_rewrites_consolidated_key_families") {
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
    let f = WorkCheckout::new();
    let global = "[workflow]\nfinal_recovery_crews = [\"sol\"]\n\n[workflow.validation_env]\nlogin_shell = false\n\n[machine]\nworker_containment = false\n";
    let workspace = "# keep this note\n[operation]\nreview_crew = [\"sol\", \"grok\"]\n\n[review]\nbefore_landing = false\n\n[workflow.purpose_crews]\nci_failure_sweep = [\"sol\"]\nmerged_head_repair = [\"sol\"]\n\n[workflow.validation_env]\ninteractive = false\n";
    let (gd, gp, wp) = write_configs(&f, global, workspace);
    let effective = || KEYS.map(|key| config_value(&f, key));
    let before = effective();
    assert_eq!(
        before,
        [
            serde_json::json!(["grok", "sol"]),
            serde_json::json!(["sol"]),
            serde_json::json!(["sol"]),
            serde_json::json!("off"),
            serde_json::json!("off"),
            serde_json::json!("off")
        ],
        "deprecated spellings load"
    );
    let fixed = fix_rows(&f);
    assert_fix_ok(&fixed);
    expect_status(&fixed, "retired-config-keys", "ok");
    assert_eq!(effective(), before, "repair keeps every effective value");
    let g = read_toml(&gp);
    let v = g["workflow"]["purpose_crews"]["final_recovery"].clone();
    assert_eq!(v, toml::Value::Array(vec!["sol".into()]));
    assert!(g["workflow"].get("final_recovery_crews").is_none());
    let v = g["workflow"]["validation_env"]["shell_probe"].as_str();
    assert_eq!(v, Some("off"));
    assert!(g["workflow"]["validation_env"].get("login_shell").is_none());
    let v = g["machine"]["worker_containment"].as_str();
    assert_eq!(v, Some("off"));
    let w = read_toml(&wp);
    let v = w["workflow"]["purpose_crews"]["review"].clone();
    assert_eq!(v, toml::Value::Array(vec!["sol".into(), "grok".into()]));
    let v = w["workflow"]["purpose_crews"]["sweep_tasks"].clone();
    assert_eq!(v, toml::Value::Array(vec!["sol".into()]));
    for legacy in ["ci_failure_sweep", "merged_head_repair"] {
        assert!(
            w["workflow"]["purpose_crews"].get(legacy).is_none(),
            "{legacy}"
        );
    }
    assert!(
        w.get("operation")
            .and_then(|v| v.get("review_crew"))
            .is_none()
    );
    let v = w["review"]["timing"].as_str();
    assert_eq!(v, Some("off"));
    assert!(w["review"].get("before_landing").is_none());
    let v = w["workflow"]["validation_env"]["shell_probe"].as_str();
    assert_eq!(v, Some("off"));
    has(&read_str(&wp), "# keep this note");
    assert_backup(&gd, global);
    assert_backup(&f.work.join(".orbit"), workspace);
}
fn effective_review(fixture: &WorkCheckout) -> (Value, Value) {
    (
        config_value(fixture, "review.timing"),
        config_value(fixture, "review.minutes"),
    )
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
    let mut names = Vec::new();
    for entry in fs::read_dir(directory).expect("config directory") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        if name.starts_with("config.toml.bak") {
            names.push(name);
        }
    }
    names.sort();
    names
}
/// Doctor reports mid-run steering's resolved settings from both config layers.
#[test]
fn doctor_reports_the_resolved_mid_run_steering_settings() {
    let f = WorkCheckout::new();
    let global = global_dir(&f);
    put(
        &global.join("config.toml"),
        "[workflow.mid_run_steering]\nmax_rounds = 3\n",
    );
    put(
        &f.work.join(".orbit/config.toml"),
        "[workflow.mid_run_steering]\nenabled = true\n",
    );
    let all = rows(&doctor(&f, &[]));
    let steering = row(&all, "mid-run-steering");
    assert_eq!(steering["status"], "ok", "{steering}");
    msg_has(steering, "enabled = true");
    msg_has(steering, "max_rounds = 3");
}
#[cfg(unix)]
#[test]
fn doctor_reports_git_protection_refusals_with_their_fix() {
    let f = WorkCheckout::new();
    let git = f.work.join(".git");
    let initial = rows(&doctor(&f, &[]));
    expect_status(&initial, "git-protection", "ok");
    // Git's own interrupted write: a temporary name beside the final one.
    let object = git.join("objects/61/5bfcb2ad167392037266f1f2dfec4546b37dd6");
    mkdir(object.parent().unwrap());
    put(&object, "loose object");
    fs::hard_link(&object, object.with_file_name("tmp_obj_Ab12Cd")).expect("temp leftover");
    let paired = rows(&doctor(&f, &[]));
    expect_status(&paired, "git-protection", "ok");
    // A hook aliased to a file outside `.git` stays refused.
    let outside = f.temp.path().join("outside-hook");
    put(&outside, "#!/bin/sh\n");
    let hook = git.join("hooks/pre-commit");
    fs::hard_link(&outside, &hook).expect("aliased hook");
    let report = rows(&doctor(&f, &[]));
    let refused = row(&report, "git-protection");
    assert_eq!(refused["status"], "warning", "{refused}");
    msg_has(refused, &hook.display().to_string());
    fix_has(refused, "git fsck");
}
#[test]
fn fs_access_reports_the_shipped_policy_verdicts_for_workspace_relative_paths() {
    let f = WorkCheckout::new();
    for (path, read, modify) in [
        ("src/main.rs", true, true),
        (".orbit/tmp/x", true, true),
        (".orbit/tasks/x", true, false),
        ("src/.env", false, false),
    ] {
        let out = run_ok(&f, &["doctor", "fs-access", "implementer", path, "--json"]);
        let json: Value = serde_json::from_slice(&out.stdout).expect("fs-access JSON");
        assert_eq!(json["policy"], "default", "{json}");
        assert_eq!(json["profile"], "implementer", "{json}");
        assert_eq!(json["path"], path, "{json}");
        assert_eq!(json["read"]["allowed"], read, "{path}: {json}");
        assert_eq!(json["modify"]["allowed"], modify, "{path}: {json}");
        assert!(json["read"]["matched_rule"].is_string(), "{json}");
        let human = run_ok(&f, &["doctor", "fs-access", "implementer", path]);
        let expected = format!("modify:  {}", if modify { "allowed" } else { "denied" });
        has(&String::from_utf8_lossy(&human.stdout), &expected);
    }
}
/// An enabled hourly routine warns when its last success is past three hours; a newer skip does not refresh it.
#[test]
fn doctor_warns_when_routine_last_success_is_stale() {
    if in_child("doctor::doctor_warns_when_routine_last_success_is_stale") {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("empty-home");
    let repo = temp.path().join("repo");
    let root = temp.path().join("custom-root");
    mkdir(&home);
    mkdir(&repo);
    crate::git_repo::SeededRepo::new(&repo)
        .readme("# routine freshness\n")
        .build();
    let root_arg = root.to_string_lossy().into_owned();
    let run = |args: &[&str]| {
        let mut cmd = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            cmd.env_remove(name);
        });
        cmd.current_dir(&repo)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ORBIT_FORMAT")
            .args(args)
            .output()
            .expect("orbit")
    };
    let run_root = |tail: &[&str]| {
        let mut a = vec!["--root", root_arg.as_str()];
        a.extend_from_slice(tail);
        run(&a)
    };
    for tail in [
        "init --non-interactive --machine-name freshness-host --task-prefix FR",
        "workspace init --name routine-freshness",
    ] {
        let args: Vec<&str> = tail.split_whitespace().collect();
        let out = run_root(&args);
        let ok = out.status.success();
        assert!(
            ok,
            "orbit {tail} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let list_routines = || -> Vec<Value> {
        let list = run_root(&["routine", "list", "--format", "json"]);
        let ok = list.status.success();
        assert!(ok, "{}", String::from_utf8_lossy(&list.stderr));
        let parsed: Value = serde_json::from_slice(&list.stdout).expect("routine list JSON");
        parsed["routines"].as_array().expect("routines").clone()
    };
    let routines = list_routines();
    let routine = routines
        .iter()
        .find(|r| {
            r["target"]
                .as_str()
                .is_some_and(|t| t.contains("worktree_gc"))
        })
        .expect("seeded worktree-gc routine");
    let name = routine["name"].as_str().expect("name").to_string();
    let now = chrono::Utc::now();
    let rfc = |ago: chrono::Duration| (now - ago).to_rfc3339();
    let db = Connection::open(root.join("orbit.db")).expect("host database");
    db.execute("INSERT INTO routine_cursors (routine_name, baseline_at, last_slot, updated_at) VALUES (?1, ?2, ?2, ?2)", rusqlite::params![name, rfc(chrono::Duration::hours(5))]).expect("cursor");
    db.execute("INSERT INTO routine_fires (routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at) VALUES (?1, ?2, 1, 'succeeded', 'jrun-fresh', 'routine-freshness', NULL, ?3, ?4)", rusqlite::params![name, rfc(chrono::Duration::hours(5)), rfc(chrono::Duration::hours(5)), rfc(chrono::Duration::hours(4))]).expect("old success");
    let before = rows(&run_root(&["doctor", "--json"]));
    expect_status(&before, "routine-freshness", "ok");
    // With `--root`, this host keeps seeded routines in the global catalog.
    let definition = root.join("routines/worktree_gc.yaml");
    let yaml = read_str(&definition);
    has(&yaml, "enabled: false");
    let enabled_yaml = yaml.replacen("enabled: false", "enabled: true", 1);
    put(&definition, &enabled_yaml);
    let stale = rows(&run_root(&["doctor", "--json"]));
    expect_status(&stale, "routine-freshness", "warning");
    msg_has(row(&stale, "routine-freshness"), &name);
    db.execute("INSERT INTO routine_fires (routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at) VALUES (?1, ?2, 1, 'skipped', NULL, 'routine-freshness', 'missed_run: skip', ?2, ?2)", rusqlite::params![name, rfc(chrono::Duration::minutes(30))]).expect("newer skip");
    let still = rows(&run_root(&["doctor", "--json"]));
    expect_status(&still, "routine-freshness", "warning");
    msg_has(row(&still, "routine-freshness"), &name);
    let routines2 = list_routines();
    let last = routines2
        .iter()
        .find(|r| r["name"] == name)
        .and_then(|r| r.get("last_fire"))
        .expect("last fire");
    assert_eq!(last["detail"], "missed_run: skip", "{last}");
    assert_eq!(last["state"], "skipped", "{last}");
    db.execute(
        "UPDATE routine_fires SET updated_at = ?1 WHERE routine_name = ?2 AND state = 'succeeded'",
        rusqlite::params![rfc(chrono::Duration::hours(1)), name],
    )
    .expect("refresh success");
    let fresh = rows(&run_root(&["doctor", "--json"]));
    expect_status(&fresh, "routine-freshness", "ok");
}
/// A home whose global config routes the default lane to `crew`, on `provider`.
fn routed_crew_fixture(crew: &str, provider: &str) -> WorkCheckout {
    let f = WorkCheckout::new();
    let mut cmd = command(&f);
    cmd.env("ORBIT_SKIP_HOST_PREREQUISITES", "1").args(
        "init --non-interactive --machine-name doctor-host-os --task-prefix DH".split_whitespace(),
    );
    let init = cmd.output().expect("init");
    assert!(init.status.success(), "{init:?}");
    let global = global_dir(&f);
    let routed = format!(
        "[workflow]\ndefault_crew = \"{crew}\"\n\n[crews.{crew}]\nprovider = \"{provider}\"\nmodel = \"fixture\"\n"
    );
    put(&global.join("config.toml"), &routed);
    f
}
fn edit_executor(
    f: &WorkCheckout,
    provider: &str,
    edit: impl FnOnce(&mut serde_yaml::Value),
) -> std::path::PathBuf {
    let path = f
        .home
        .join(".orbit/resources/executors")
        .join(format!("{provider}.yaml"));
    let mut executor: serde_yaml::Value =
        serde_yaml::from_str(&read_str(&path)).expect("seeded executor YAML");
    edit(&mut executor);
    fs::write(
        &path,
        serde_yaml::to_string(&executor).expect("pinned executor YAML"),
    )
    .expect("pinned executor");
    path
}
#[cfg(not(target_os = "macos"))]
#[test]
fn doctor_skips_a_crew_whose_executor_host_os_excludes_this_host() {
    if in_child("doctor::doctor_skips_a_crew_whose_executor_host_os_excludes_this_host") {
        return;
    }
    let f = routed_crew_fixture("muse-wonder", "muse");
    pin_executor_host_os(&f, "muse", &["macos"]);
    let parsed = rows(&doctor(&f, &[]));
    let crew = row(&parsed, "provider:muse-wonder");
    assert_eq!(crew["status"], "skipped", "{crew}");
    msg_has(crew, "unsupported on this OS");
    assert!(
        crew["remediation"].is_null(),
        "unsupported crew gets no install advice: {crew}"
    );
}
/// Set `host_os` on `provider`'s seeded executor, leaving every other field as shipped.
#[cfg(not(target_os = "macos"))]
fn pin_executor_host_os(fixture: &WorkCheckout, provider: &str, host_os: &[&str]) {
    edit_executor(fixture, provider, |executor| {
        executor["spec"]["host_os"] = serde_yaml::Value::Sequence(
            host_os
                .iter()
                .map(|os| serde_yaml::Value::String((*os).to_string()))
                .collect(),
        );
    });
}
/// Point `provider`'s executor at an absent absolute launcher under the fixture root; returns that command.
fn pin_missing_launcher(fixture: &WorkCheckout, provider: &str) -> String {
    let launcher = fixture
        .temp
        .path()
        .join("absent-launchers")
        .join(provider)
        .display()
        .to_string();
    let owned = launcher.clone();
    edit_executor(fixture, provider, move |executor| {
        executor["spec"]["command"] = serde_yaml::Value::String(owned);
    });
    launcher
}
/// A missing CLI is an error with install advice for supported and unrestricted executors alike.
#[test]
fn doctor_keeps_a_missing_cli_an_error_on_a_host_the_executor_supports() {
    if in_child("doctor::doctor_keeps_a_missing_cli_an_error_on_a_host_the_executor_supports") {
        return;
    }
    // Rows: (case, crew, provider). Each row gets a fresh fixture.
    for (case, crew, provider) in [
        ("supported-host", "muse-wonder", "muse"),
        ("unrestricted", "codex-wonder", "codex"),
    ] {
        let f = routed_crew_fixture(crew, provider);
        let launcher = pin_missing_launcher(&f, provider);
        let mut cmd = command(&f);
        cmd.env("PATH", f.temp.path()).args(["doctor", "--json"]);
        let out = cmd.output().expect("doctor");
        let parsed = rows(&out);
        let crew_row = row(&parsed, &format!("provider:{crew}"));
        assert_eq!(
            crew_row["status"], "error",
            "{case} {crew}/{provider}: {crew_row}"
        );
        msg_has(crew_row, &format!("CLI '{launcher}' was not found"));
        fix_has(crew_row, &format!("Install the '{launcher}' CLI"));
    }
}
/// Unknown-only files must enter the existing locked, backed-up repair flow.
#[test]
fn doctor_fixes_unknown_config_without_retired_keys() {
    let f = WorkCheckout::new();
    let global_body = "[workflow]\nbase_branc = 'stale'\nbase_branch = 'main'\n";
    let workspace_body = "# keep this note\n[workflow]\nbase_branch = 'agent-main'\n[future]\n";
    let (gd, gc, wc) = write_configs(&f, global_body, workspace_body);
    let diagnosis = rows(&doctor(&f, &[]));
    let typo = diagnosis.iter().any(|r| {
        mentions_key_and_file(r, "workflow.base_branc", &gc)
            && r["message"]
                .as_str()
                .is_some_and(|m| m.contains("workflow.base_branch"))
    });
    assert!(typo, "{diagnosis:?}");
    assert!(
        diagnosis
            .iter()
            .any(|r| mentions_key_and_file(r, "future", &wc)),
        "{diagnosis:?}"
    );
    let repaired = fix_rows(&f);
    expect_status(&repaired, "fix-retired-config-keys", "ok");
    expect_status(&repaired, "retired-config-keys", "ok");
    assert_backup(&gd, global_body);
    assert_backup(&f.work.join(".orbit"), workspace_body);
    let updated = read_toml(&gc);
    assert_eq!(updated["workflow"]["base_branch"].as_str(), Some("main"));
    assert!(updated["workflow"].get("base_branc").is_none());
    let updated_text = read_str(&wc);
    has(&updated_text, "# keep this note");
    assert!(read_toml(&wc).get("future").is_none());
}

fn severity_order(text: &str) -> (bool, bool) {
    let (mut max_rank, mut warn, mut ok_after) = (0, false, false);
    for line in text.lines().filter(|l| l.contains('\t')) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let status = parts[1].to_lowercase();
        let ranks = ["error", "warning", "ok", "info", "skipped"];
        let Some(rank) = ranks.iter().position(|r| r == &status) else {
            continue;
        };
        let in_order = rank >= max_rank;
        assert!(
            in_order,
            "status {} after rank {max_rank}: {line}",
            parts[1]
        );
        max_rank = rank;
        warn = warn || status == "warning";
        ok_after = ok_after || (status == "ok" && warn);
    }
    (warn, ok_after)
}
/// ORB-16960: count on stderr, severity order, `--problems` filters human rows, JSON stays full.
#[test]
fn doctor_prints_summary_count_orders_by_severity_and_filters_problems() {
    if in_child("doctor::doctor_prints_summary_count_orders_by_severity_and_filters_problems") {
        return;
    }
    let f = WorkCheckout::new();
    let is_summary = |line: &str| line.contains(" failure(s), ") && line.ends_with(" warning(s).");
    // 1. Plain stdout stays tab-separated records; the count is on stderr.
    let plain = run(&f, &["doctor", "--format", "plain"]);
    let stdout = utf8(plain.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| l.contains('\t')).collect();
    assert!(!lines.is_empty(), "doctor output must not be empty");
    assert!(
        !stdout.lines().any(is_summary),
        "summary must not be a plain stdout record: {stdout}"
    );
    assert!(
        utf8(plain.stderr).lines().any(is_summary),
        "summary must be on stderr"
    );
    // 2. Introduce a warning via a dangling skill symlink.
    let base = f.work.join(".orbit/skills");
    mkdir(&base);
    #[cfg(unix)]
    link(&base.join("nonexistent"), &base.join("dangling-probe"));
    let table_out = run(&f, &["doctor", "--format", "table"]);
    let table_str = utf8(table_out.stdout);
    let first = table_str.lines().next().unwrap_or("");
    assert!(
        first.starts_with("CHECK") && !is_summary(first),
        "table stdout must start with header: {first}"
    );
    assert!(
        utf8(table_out.stderr).lines().any(is_summary),
        "table path must report count on stderr"
    );
    // Row ordering in plain output: failures, then warnings, then ok rows.
    let plain_warn = run(&f, &["doctor", "--format", "plain"]);
    let plain_str = utf8(plain_warn.stdout);
    assert!(
        !plain_str.lines().any(is_summary),
        "summary must stay off plain stdout: {plain_str}"
    );
    let (seen_warn, seen_ok_after) = severity_order(&plain_str);
    #[cfg(unix)]
    {
        assert!(seen_warn, "should have a warning row: {plain_str}");
        assert!(seen_ok_after, "ok rows after warnings: {plain_str}");
    }
    // 3. --problems keeps only errors and warnings in human output.
    let problems = run(&f, &["doctor", "--problems", "--format", "plain"]);
    for line in utf8(problems.stdout).lines().filter(|l| l.contains('\t')) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2 {
            let problem = matches!(parts[1], "ERROR" | "warning" | "error");
            assert!(problem, "--problems only errors/warnings: {line}");
        }
    }
    // 4. --json with and without --problems returns the full array.
    let normal = rows(&run(&f, &["doctor", "--json"]));
    let problems_json = rows(&run(&f, &["doctor", "--problems", "--json"]));
    let same_len = normal.len() == problems_json.len();
    assert!(same_len, "--problems --json must return all rows");
    for (n, pr) in normal.iter().zip(&problems_json) {
        assert_eq!(n["check"], pr["check"]);
        assert_eq!(n["status"], pr["status"]);
        if n["check"] != "disk-space" {
            assert_eq!(n["message"], pr["message"]);
        }
        assert_eq!(n["remediation"], pr["remediation"]);
    }
}
