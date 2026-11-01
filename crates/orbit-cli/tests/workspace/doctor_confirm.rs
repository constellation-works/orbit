use std::fs;
use std::path::PathBuf;

use assert_cmd::cargo::cargo_bin_cmd;
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
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "doctor-confirm"])
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
            .env_remove("ORBIT_FORMAT")
            .env("RUST_LOG", "off");
        command.timeout(std::time::Duration::from_secs(30));
        command
    }
}

fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn dead child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}

#[test]
fn doctor_refuses_orphan_task_stores_without_confirm_before_mutating_locks() {
    let fixture = Fixture::new();
    let lock_path = fixture.work.join(".orbit/state/stale_candidate.lock");
    fs::create_dir_all(fixture.work.join(".orbit/state")).expect("create state dir");

    let holder_json = serde_json::json!({
        "pid": dead_pid(),
        "acquired_at": "2026-10-09T00:00:00Z",
        "label": "stale candidate lock",
    });
    let holder_bytes = serde_json::to_vec(&holder_json).expect("serialize holder");
    fs::write(&lock_path, &holder_bytes).expect("write stale lock");

    // Stale lock holder is confirmed present before running doctor.
    assert!(
        orbit_common::fs::file_lock::read_file_lock_holder(&lock_path).is_some(),
        "stale lock holder must be readable before doctor runs"
    );

    // Running doctor with --fix-stale-locks and --fix-orphan-task-stores without --confirm
    // must fail with the confirmation error.
    let output = fixture
        .orbit()
        .args(["doctor", "--fix-stale-locks", "--fix-orphan-task-stores"])
        .output()
        .expect("spawn orbit doctor");

    assert!(
        !output.status.success(),
        "doctor must fail when --fix-orphan-task-stores is requested without --confirm"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--fix-orphan-task-stores") && stderr.contains("--confirm"),
        "stderr must contain the confirmation error, got: {stderr}"
    );

    // Stale lock holder must still be present and unchanged.
    assert_eq!(
        fs::read(&lock_path).expect("read stale lock after refused repair"),
        holder_bytes,
        "stale lock holder must remain unchanged when doctor refuses unconfirmed repair"
    );

    // The same combination with --confirm runs every requested repair and reports each in the result.
    let confirmed_output = fixture
        .orbit()
        .args([
            "doctor",
            "--fix-stale-locks",
            "--fix-orphan-task-stores",
            "--confirm",
            "--json",
        ])
        .output()
        .expect("spawn orbit doctor with --confirm");

    assert!(
        confirmed_output.status.success(),
        "doctor with --confirm must succeed: {}",
        String::from_utf8_lossy(&confirmed_output.stderr)
    );

    let rows: Vec<Value> = serde_json::from_slice(&confirmed_output.stdout).unwrap_or_else(|err| {
        panic!(
            "doctor JSON parse error: {err}; stdout:\n{}",
            String::from_utf8_lossy(&confirmed_output.stdout)
        )
    });

    let fix_stale = rows
        .iter()
        .find(|row| row["check"] == "fix-stale-locks")
        .expect("result must report fix-stale-locks check");
    assert_eq!(fix_stale["status"], "ok");

    let fix_orphan = rows
        .iter()
        .find(|row| row["check"] == "fix-orphan-task-stores")
        .expect("result must report fix-orphan-task-stores check");
    assert_eq!(fix_orphan["status"], "ok");

    // After running with --confirm, the stale lock record was cleared.
    assert!(
        orbit_common::fs::file_lock::read_file_lock_holder(&lock_path).is_none(),
        "stale lock holder must be cleared after confirmed repair"
    );
}

#[test]
fn doctor_reports_a_misplaced_host_plugin_enablement_table_that_blocks_startup() {
    let fixture = Fixture::new();
    let config = fixture.home.join(".orbit/config.toml");
    let mut host_config = fs::read_to_string(&config).unwrap_or_default();
    host_config.push_str("\n[plugin_enablement]\ngraph = false\n");
    fs::create_dir_all(config.parent().expect("config parent")).expect("create host root");
    fs::write(&config, host_config).expect("write host config");

    // A normal command still fails closed on the misplaced table.
    let refused = fixture
        .orbit()
        .args(["task", "list", "--json"])
        .output()
        .expect("spawn orbit task list");
    assert!(!refused.status.success(), "task list must refuse the table");
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(refusal.contains("plugin_enablement"), "{refusal}");

    // `orbit doctor` still reports the finding, naming the host and file with
    // the exact remediation, and exits nonzero.
    let output = fixture
        .orbit()
        .args(["doctor", "--json"])
        .output()
        .expect("spawn orbit doctor");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "doctor JSON parse error: {err}; stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let row = rows
        .iter()
        .find(|row| row["check"] == "plugin-enablement")
        .unwrap_or_else(|| panic!("plugin-enablement row missing: {rows:?}"));
    assert_eq!(row["status"], "error");
    let message = row["message"].as_str().expect("message");
    assert!(message.contains("this host"), "{message}");
    assert!(message.contains("config.toml"), "{message}");
    let remediation = row["remediation"].as_str().expect("remediation");
    assert!(
        remediation.contains("Remove [plugin_enablement] from"),
        "{remediation}"
    );
    assert!(remediation.contains("--scope workspace"), "{remediation}");

    // A repair flag needs a runtime, so it keeps failing closed.
    let repair = fixture
        .orbit()
        .args(["doctor", "--fix-stale-locks", "--json"])
        .output()
        .expect("spawn orbit doctor --fix-stale-locks");
    assert!(!repair.status.success());
    assert!(
        serde_json::from_slice::<Vec<Value>>(&repair.stdout).is_err(),
        "a repair must not report a degraded health check"
    );
}

#[test]
fn doctor_reports_misplaced_table_under_orbit_root_not_home() {
    let fixture = Fixture::new();
    let scratch = fixture
        .home
        .parent()
        .expect("temp root")
        .join("scratch-orbit");
    fs::create_dir_all(&scratch).expect("create scratch root");
    let scratch_config = scratch.join("config.toml");
    fs::write(&scratch_config, "[plugin_enablement]\ngraph = false\n")
        .expect("write scratch config");
    let home_config = fixture.home.join(".orbit/config.toml");
    let home_contents = fs::read_to_string(&home_config).unwrap_or_default();
    assert!(
        !home_contents.contains("[plugin_enablement]"),
        "home config must stay clean in this scenario"
    );

    let output = fixture
        .orbit()
        .env("ORBIT_ROOT", &scratch)
        .args(["doctor", "--json"])
        .output()
        .expect("spawn orbit doctor");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "doctor JSON parse error: {err}; stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let row = rows
        .iter()
        .find(|row| row["check"] == "plugin-enablement")
        .unwrap_or_else(|| panic!("plugin-enablement row missing: {rows:?}"));
    assert_eq!(row["status"], "error");
    let message = row["message"].as_str().expect("message");
    assert!(
        message.contains(&scratch_config.display().to_string()),
        "row must name the ORBIT_ROOT config, got: {message}"
    );
    assert!(
        !message.contains(&home_config.display().to_string()),
        "row must not name the home config, got: {message}"
    );
}

#[test]
fn doctor_reports_misplaced_table_under_root_checkout_orbit_child() {
    let fixture = Fixture::new();
    let checkout = fixture.home.parent().expect("temp root").join("checkout2");
    crate::git_repo::init(&checkout);
    let mut init = fixture.orbit();
    init.current_dir(&checkout);
    init.args(["workspace", "init", "--name", "doctor-root-checkout"])
        .assert()
        .success();
    let orbit_config = checkout.join(".orbit/config.toml");
    let mut contents = fs::read_to_string(&orbit_config).unwrap_or_default();
    contents.push_str("\n[plugin_enablement]\ngraph = false\n");
    fs::write(&orbit_config, contents).expect("write checkout orbit config");
    assert!(
        !checkout.join("config.toml").exists(),
        "checkout root must have no config.toml in this scenario"
    );

    let output = fixture
        .orbit()
        .args([
            "--root",
            &checkout.display().to_string(),
            "doctor",
            "--json",
        ])
        .output()
        .expect("spawn orbit doctor --root checkout");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "doctor JSON parse error: {err}; stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let row = rows
        .iter()
        .find(|row| row["check"] == "plugin-enablement")
        .unwrap_or_else(|| panic!("plugin-enablement row missing: {rows:?}"));
    assert_eq!(row["status"], "error");
    let message = row["message"].as_str().expect("message");
    assert!(
        message.contains(&orbit_config.display().to_string()),
        "row must name the .orbit child config, got: {message}"
    );
    let bare_checkout_config = format!("'{}'", checkout.join("config.toml").display());
    assert!(
        !message.contains(&bare_checkout_config),
        "row must not name <checkout>/config.toml, got: {message}"
    );
}

#[test]
fn doctor_keeps_bare_error_when_admission_config_is_clean_but_home_is_dirty() {
    let fixture = Fixture::new();
    // Dirty the home global config; admission under ORBIT_ROOT never loads it.
    let home_config = fixture.home.join(".orbit/config.toml");
    let mut host_config = fs::read_to_string(&home_config).unwrap_or_default();
    host_config.push_str("\n[plugin_enablement]\ngraph = false\n");
    fs::create_dir_all(home_config.parent().expect("config parent")).expect("create host root");
    fs::write(&home_config, host_config).expect("write host config");

    // Clean of [plugin_enablement], but invalid so startup still fails.
    let scratch = fixture
        .home
        .parent()
        .expect("temp root")
        .join("scratch-orbit-clean");
    fs::create_dir_all(&scratch).expect("create scratch root");
    fs::write(
        scratch.join("config.toml"),
        "this is = = not valid toml [\n",
    )
    .expect("write scratch config");

    let output = fixture
        .orbit()
        .env("ORBIT_ROOT", &scratch)
        .args(["doctor", "--json"])
        .output()
        .expect("spawn orbit doctor");
    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("plugin-enablement"),
        "doctor must not report the home table it never loaded, stdout: {stdout}"
    );
    assert!(
        serde_json::from_slice::<Vec<Value>>(&output.stdout).is_err()
            || serde_json::from_slice::<Vec<Value>>(&output.stdout)
                .expect("rows")
                .iter()
                .all(|row| row["check"] != "plugin-enablement"),
        "bare bootstrap error must stand, stdout: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "bare bootstrap error must be printed to stderr"
    );
}
