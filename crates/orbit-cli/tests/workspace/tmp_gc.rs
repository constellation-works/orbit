//! Scratch collection through the real CLI, with disposable workspace state.

use std::fs;
use std::path::PathBuf;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use rusqlite::{Connection, params};
use serde_json::Value;

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
            .args(["workspace", "init", "--name", "scratch"])
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
        command.timeout(std::time::Duration::from_secs(30));
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }

    fn tmp(&self) -> PathBuf {
        self.work.join(".orbit/tmp")
    }

    fn populate(&self) {
        fs::create_dir_all(self.tmp().join("step-recovery/run/slot")).unwrap();
        fs::write(self.tmp().join("operator.log"), b"abcd").unwrap();
        fs::write(
            self.tmp().join("step-recovery/run/slot/result.json"),
            b"123456",
        )
        .unwrap();
    }

    fn workspace_id(&self) -> String {
        let config: Value =
            serde_yaml::from_slice(&fs::read(self.work.join(".orbit/config.yaml")).unwrap())
                .unwrap();
        config["workspace_id"].as_str().unwrap().to_owned()
    }
}

#[test]
fn previews_and_confirmation_report_bytes_and_preserve_the_directory() {
    let fixture = Fixture::new();
    let empty = fixture.json(&["gc", "tmp", "--confirm", "--json"]);
    assert_eq!(empty["entries_removed"], 0);
    assert_eq!(empty["bytes_reclaimed"], 0);
    assert!(fixture.tmp().is_dir());
    fixture.populate();
    let state_sentinel = fixture.work.join(".orbit/state/operator-note");
    fs::create_dir_all(state_sentinel.parent().unwrap()).unwrap();
    fs::write(&state_sentinel, b"outside scratch").unwrap();
    for args in [
        vec!["gc", "tmp", "--json"],
        vec!["gc", "tmp", "--dry-run", "--json"],
    ] {
        let report = fixture.json(&args);
        assert_eq!(report["dry_run"], true);
        assert_eq!(report["entries_removed"], 0);
        assert_eq!(report["bytes_reclaimable"], 10);
        assert_eq!(report["bytes_reclaimed"], 0);
        let entries = report["reports"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for (entry, (name, bytes)) in entries
            .iter()
            .zip([("operator.log", 4), ("step-recovery", 6)])
        {
            assert_eq!(
                entry["path"],
                fixture
                    .tmp()
                    .canonicalize()
                    .unwrap()
                    .join(name)
                    .to_str()
                    .unwrap()
            );
            assert_eq!(entry["action"], "would_remove");
            assert_eq!(entry["bytes_reclaimable"], bytes);
        }
        assert_eq!(
            fs::read(fixture.tmp().join("operator.log")).unwrap(),
            b"abcd"
        );
        assert_eq!(
            fs::read(fixture.tmp().join("step-recovery/run/slot/result.json")).unwrap(),
            b"123456"
        );
    }
    fixture
        .orbit()
        .args(["gc", "tmp"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "operator.log action=would_remove bytes_reclaimable=4 bytes_reclaimed=0",
        ))
        .stdout(predicates::str::contains(
            "step-recovery action=would_remove bytes_reclaimable=6 bytes_reclaimed=0",
        ));
    fixture
        .orbit()
        .args(["gc", "tmp", "--confirm", "--dry-run"])
        .assert()
        .failure();
    fixture
        .orbit()
        .args(["gc", "tmp", "--confirm", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("capability_denied"));
    assert_eq!(fs::read_dir(fixture.tmp()).unwrap().count(), 2);
    let removed = fixture.json(&["gc", "tmp", "--confirm", "--json"]);
    assert_eq!(removed["dry_run"], false);
    assert_eq!(removed["entries_removed"], 2);
    assert_eq!(removed["bytes_reclaimed"], 10);
    for entry in removed["reports"].as_array().unwrap() {
        assert_eq!(entry["action"], "removed");
        assert_eq!(entry["bytes_reclaimed"], entry["bytes_reclaimable"]);
    }
    assert!(fixture.tmp().is_dir());
    assert_eq!(fs::read_dir(fixture.tmp()).unwrap().count(), 0);
    assert_eq!(fs::read(&state_sentinel).unwrap(), b"outside scratch");
    fixture.populate();
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["gc", "tmp", "--confirm"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "entries_removed=2 total_bytes_reclaimable=10 total_bytes_reclaimed=10",
        ));
    assert_eq!(fs::read_dir(fixture.tmp()).unwrap().count(), 0);
    assert_eq!(
        fixture.json(&["gc", "tmp", "--confirm", "--json"])["entries_removed"],
        0
    );
}

#[test]
fn active_runs_refuse_without_reconciliation_and_workspace_selection_is_scoped() {
    // SQL seeding is mutable Orbit state, so execute this fixture only in a
    // child whose managed-run authority has been removed.
    const TEST: &str =
        "tmp_gc::active_runs_refuse_without_reconciliation_and_workspace_selection_is_scoped";
    if std::env::var("ORBIT_TMP_GC_TEST_CHILD").as_deref() != Ok(TEST) {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        let output = child
            .env("ORBIT_TMP_GC_TEST_CHILD", TEST)
            .args(["--exact", TEST, "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "child must exercise one boundary test"
        );
        return;
    }
    let fixture = Fixture::new();
    fixture
        .orbit()
        .args(["run", "history", "--no-reconcile", "--json"])
        .assert()
        .success();
    let other = fixture._temp.path().join("other");
    crate::git_repo::init(&other);
    fixture
        .orbit()
        .current_dir(&other)
        .args(["workspace", "init", "--name", "other"])
        .assert()
        .success();
    fs::create_dir_all(other.join(".orbit/tmp")).unwrap();
    fs::write(other.join(".orbit/tmp/other.log"), b"other").unwrap();
    fixture.populate();
    let db = Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    // `retrying` is live: the engine sleeps between attempts, then runs again.
    for (run_id, state) in [
        ("jrun-tmp-pending", "pending"),
        ("jrun-tmp-retrying", "retrying"),
        ("jrun-tmp-running", "running"),
    ] {
        db.execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,created_at,pid) VALUES (?1,?2,'fixture',1,?3,'2020-01-01T00:00:00Z','2020-01-01T00:00:00Z',999999)", params![run_id, fixture.workspace_id(), state]).unwrap();
    }
    let failed = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["gc", "tmp", "--confirm", "--json"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let error: Value = serde_json::from_slice(&failed).unwrap();
    assert_eq!(error["code"], "tmp_gc_active_runs");
    assert_eq!(
        error["run_ids"],
        serde_json::json!(["jrun-tmp-pending", "jrun-tmp-retrying", "jrun-tmp-running"])
    );
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["gc", "tmp", "--confirm"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("jrun-tmp-pending"))
        .stderr(predicates::str::contains("jrun-tmp-retrying"))
        .stderr(predicates::str::contains("jrun-tmp-running"));
    let mut statement = db
        .prepare("SELECT state FROM job_runs ORDER BY run_id")
        .unwrap();
    let states: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        states,
        ["pending", "retrying", "running"],
        "GC must never reconcile active rows to make deletion eligible"
    );
    // The retrying run alone must refuse: its next attempt still needs scratch.
    db.execute(
        "UPDATE job_runs SET state = 'success' WHERE run_id IN ('jrun-tmp-pending', 'jrun-tmp-running')",
        [],
    )
    .unwrap();
    let retrying_only = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["gc", "tmp", "--confirm", "--json"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let error: Value = serde_json::from_slice(&retrying_only).unwrap();
    assert_eq!(error["run_ids"], serde_json::json!(["jrun-tmp-retrying"]));
    assert_eq!(
        fs::read(fixture.tmp().join("operator.log")).unwrap(),
        b"abcd"
    );
    assert_eq!(
        fixture.json(&["gc", "tmp", "--dry-run", "--json"])["bytes_reclaimable"],
        10
    );
    let selected = fixture.json(&["gc", "tmp", "--workspace", "other", "--confirm", "--json"]);
    assert_eq!(selected["entries_removed"], 1);
    assert_eq!(selected["bytes_reclaimed"], 5);
    assert_eq!(fs::read_dir(other.join(".orbit/tmp")).unwrap().count(), 0);
    assert_eq!(
        fs::read(fixture.tmp().join("operator.log")).unwrap(),
        b"abcd"
    );
    assert_eq!(
        fs::read(fixture.tmp().join("step-recovery/run/slot/result.json")).unwrap(),
        b"123456"
    );
}

#[test]
fn symlinks_are_unlinked_without_traversal_and_a_linked_tmp_root_refuses() {
    assert_symlink_gc_handles_non_utf8_name(|path| fs::write(path, b"bytes"));
}

#[test]
fn symlink_gc_still_checks_symlinks_when_non_utf8_name_is_unsupported() {
    assert_symlink_gc_handles_non_utf8_name(|_| {
        Err(std::io::Error::from_raw_os_error(libc::EILSEQ))
    });
}

fn assert_symlink_gc_handles_non_utf8_name(
    create_non_utf8_entry: impl FnOnce(&PathBuf) -> std::io::Result<()>,
) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = fixture._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"preserve target").unwrap();
    fs::create_dir_all(fixture.tmp().join("nested")).unwrap();
    symlink(&outside, fixture.tmp().join("escape")).unwrap();
    symlink(
        outside.join("sentinel"),
        fixture.tmp().join("nested/file-link"),
    )
    .unwrap();
    symlink(outside.join("missing"), fixture.tmp().join("dangling")).unwrap();
    let byte_name = std::ffi::OsStr::from_bytes(b"non-utf8-\xff");
    let byte_path = fixture.tmp().join(byte_name);
    let has_non_utf8_entry = match create_non_utf8_entry(&byte_path) {
        Ok(()) => true,
        Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => false,
        Err(error) => panic!("failed to probe non-UTF-8 filename support: {error}"),
    };
    let expected_bytes: u64 = ["escape", "nested/file-link", "dangling"]
        .iter()
        .map(|name| {
            fs::symlink_metadata(fixture.tmp().join(name))
                .unwrap()
                .len()
        })
        .sum::<u64>()
        + if has_non_utf8_entry { 5 } else { 0 };
    let preview = fixture.json(&["gc", "tmp", "--json"]);
    assert_eq!(preview["bytes_reclaimable"], expected_bytes);
    assert_eq!(
        preview["reports"].as_array().unwrap().iter().any(|report| {
            report["path"] == byte_path.to_string_lossy().as_ref()
                && report["bytes_reclaimable"] == 5
        }),
        has_non_utf8_entry
    );
    let removed = fixture.json(&["gc", "tmp", "--confirm", "--json"]);
    assert_eq!(
        removed["entries_removed"],
        if has_non_utf8_entry { 4 } else { 3 }
    );
    assert_eq!(removed["bytes_reclaimed"], expected_bytes);
    assert_eq!(fs::read_dir(fixture.tmp()).unwrap().count(), 0);
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"preserve target"
    );
    // Move the legitimate empty directory aside; the root itself must never
    // be followed, even when it points to an otherwise valid directory.
    fs::rename(fixture.tmp(), fixture.work.join(".orbit/empty-tmp")).unwrap();
    symlink(&outside, fixture.tmp()).unwrap();
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["gc", "tmp", "--confirm", "--json"])
        .assert()
        .failure();
    fixture
        .orbit()
        .args(["gc", "tmp", "--dry-run"])
        .assert()
        .failure();
    assert!(
        fs::symlink_metadata(fixture.tmp())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"preserve target"
    );
}

#[test]
fn a_checkout_without_orbit_directory_has_nothing_to_reclaim() {
    // State lives in an explicit root, so the checkout never gets `.orbit/`.
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("root");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).unwrap();
    crate::git_repo::init(&work);
    let fixture = Fixture {
        _temp: temp,
        home,
        work,
    };
    let root = root.to_str().unwrap();
    fixture
        .orbit()
        .args([
            "--root",
            root,
            "init",
            "--non-interactive",
            "--machine-name",
            "qa",
            "--task-prefix",
            "QA",
        ])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["--root", root, "workspace", "init", "--name", "scratch"])
        .assert()
        .success();
    assert!(!fixture.work.join(".orbit").exists());
    let rooted = |args: &[&str]| {
        let mut all = vec!["--root", root];
        all.extend_from_slice(args);
        fixture.json(&all)
    };
    let preview = rooted(&["gc", "tmp", "--dry-run", "--json"]);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["entries_removed"], 0);
    assert_eq!(preview["bytes_reclaimable"], 0);
    assert_eq!(preview["reports"], serde_json::json!([]));
    let removed = rooted(&["gc", "tmp", "--confirm", "--json"]);
    assert_eq!(removed["dry_run"], false);
    assert_eq!(removed["entries_removed"], 0);
    assert!(
        !fixture.work.join(".orbit").exists(),
        "confirm must not create .orbit as a side effect of gc"
    );
}
