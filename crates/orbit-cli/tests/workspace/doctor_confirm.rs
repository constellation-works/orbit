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
