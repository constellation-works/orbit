//! Deployment, executable-backup and store-backup retention through the real
//! CLI, against a disposable Orbit home.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use sha2::{Digest, Sha256};

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        crate::git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
            outside,
        };
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "deployments"])
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
        command.timeout(Duration::from_secs(30));
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

    fn orbit_home(&self) -> PathBuf {
        self.home.join(".orbit").canonicalize().unwrap()
    }
}

/// Write `bytes` to `path` and date it `age_hours` ago, then date its parent
/// directory the same so the entry's newest time is exactly that.
fn write_aged(path: &Path, bytes: &[u8], age_hours: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    age(path, age_hours);
}

fn age(path: &Path, age_hours: u64) {
    let time = SystemTime::now() - Duration::from_secs(age_hours * 3600);
    fs::File::open(path).unwrap().set_modified(time).unwrap();
}

fn deployment(root: &Path, name: &str, bytes: &[u8], age_hours: u64) -> PathBuf {
    let directory = root.join("deployments").join(name);
    write_aged(&directory.join("build.log"), bytes, age_hours);
    age(&directory, age_hours);
    directory
}

fn candidate_manifest(executable: &[u8]) -> Vec<u8> {
    let digest: String = Sha256::digest(executable)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "kind": "orbit-local-candidate",
        "trust": "operator_attested",
        "source_commit": "0123456789abcdef0123456789abcdef01234567",
        "target": "x86_64-unknown-linux-gnu",
        "executable_sha256": digest,
    }))
    .unwrap()
}

fn actions(report: &Value) -> Vec<(String, String, String, u64)> {
    report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().unwrap().to_owned(),
                entry["action"].as_str().unwrap().to_owned(),
                entry["reason"].as_str().unwrap().to_owned(),
                entry["bytes"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn deployment_retention_reports_with_bytes_and_confirm_removes_only_what_it_listed() {
    let fixture = Fixture::new();
    let root = fixture.orbit_home();
    let installed = b"installed-orbit-v3".as_slice();
    let rollback = b"rollback-orbit-v2".as_slice();
    // The rollback copy is the oldest file in bin/ and must survive anyway.
    write_aged(&root.join("bin/orbit"), installed, 1);
    write_aged(&root.join("bin/orbit.previous"), rollback, 900);
    write_aged(&root.join("bin/orbit.bak-pre-1"), b"bak-one", 10);
    write_aged(&root.join("bin/orbit.before-abc"), b"before-22", 20);
    write_aged(&root.join("bin/orbit-graph.stale-2026"), b"stale-333", 30);
    write_aged(&root.join("bin/orbit-graph"), b"another tool", 800);
    write_aged(&root.join("bin/install-1.json"), b"{}", 800);

    let newest = deployment(&root, "prepare-c", b"cccc", 2);
    let second = deployment(&root, "prepare-b", b"bbbbb", 5);
    let old = deployment(&root, "prepare-a", b"aaaaaa", 50);
    // Built the rollback executable: kept although it is the oldest.
    let pinned = deployment(&root, "owner-rollback", b"p", 700);
    write_aged(
        &pinned.join("candidate-manifest.json"),
        &candidate_manifest(rollback),
        700,
    );
    age(&pinned, 700);
    // A link is unlinked, never followed into what it names.
    let sentinel = fixture.outside.join("keep.txt");
    fs::write(&sentinel, b"outside the orbit home").unwrap();
    std::os::unix::fs::symlink(&fixture.outside, root.join("deployments/linked")).unwrap();
    let linked_age = SystemTime::now() - Duration::from_secs(600 * 3600);
    // Date the link itself; set_modified on an opened path would date its target.
    let link = std::ffi::CString::new(
        root.join("deployments/linked")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    let seconds = linked_age
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let times = [libc::timespec {
        tv_sec: libc::time_t::try_from(seconds).unwrap(),
        tv_nsec: 0,
    }; 2];
    // SAFETY: link is NUL-terminated and times holds two initialized entries.
    assert_eq!(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                link.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        },
        0
    );

    write_aged(&root.join("orbit.db.bak-3"), b"db-3", 3);
    write_aged(&root.join("orbit.db.bak-2"), b"db-22", 40);
    write_aged(&root.join("orbit.db.bak-1"), b"db-111", 400);

    let link_bytes = fs::symlink_metadata(root.join("deployments/linked"))
        .unwrap()
        .len();
    let path = |relative: &str| root.join(relative).to_str().unwrap().to_owned();
    let expected = vec![
        (newest.to_str().unwrap().to_owned(), "kept", "newest", 4),
        (second.to_str().unwrap().to_owned(), "kept", "newest", 5),
        (
            old.to_str().unwrap().to_owned(),
            "would_remove",
            "beyond_retention",
            6,
        ),
        (
            path("deployments/linked"),
            "would_remove",
            "beyond_retention",
            link_bytes,
        ),
        (
            pinned.to_str().unwrap().to_owned(),
            "kept",
            "rollback_candidate",
            1 + u64::try_from(candidate_manifest(rollback).len()).unwrap(),
        ),
        (path("bin/orbit.bak-pre-1"), "kept", "newest", 7),
        (path("bin/orbit.before-abc"), "kept", "newest", 9),
        (
            path("bin/orbit-graph.stale-2026"),
            "would_remove",
            "beyond_retention",
            9,
        ),
        (path("bin/orbit.previous"), "kept", "rollback_target", 17),
        (path("orbit.db.bak-3"), "kept", "newest", 4),
        (path("orbit.db.bak-2"), "kept", "newest", 5),
        (
            path("orbit.db.bak-1"),
            "would_remove",
            "beyond_retention",
            6,
        ),
    ];
    let expected: Vec<_> = expected
        .into_iter()
        .map(|(path, action, reason, bytes)| (path, action.to_owned(), reason.to_owned(), bytes))
        .collect();
    let removable: u64 = 6 + link_bytes + 9 + 6;

    for args in [
        vec!["gc", "deployments", "--json"],
        vec!["gc", "deployments", "--dry-run", "--json"],
    ] {
        let report = fixture.json(&args);
        assert_eq!(report["dry_run"], true);
        assert_eq!(report["keep"], 2);
        assert_eq!(actions(&report), expected);
        assert_eq!(report["bytes_reclaimable"], removable);
        assert_eq!(report["bytes_reclaimed"], 0);
        assert_eq!(report["entries_removed"], 0);
        assert!(old.is_dir() && root.join("orbit.db.bak-1").is_file());
    }
    fixture
        .orbit()
        .args(["gc", "deployments"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "class=store_backup path={} action=would_remove reason=beyond_retention bytes=6",
            path("orbit.db.bak-1"),
        )));

    fixture
        .orbit()
        .args(["gc", "deployments", "--confirm"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "'gc deployments' requires the `operator` capability",
        ));
    assert!(
        old.is_dir() && root.join("orbit.db.bak-1").is_file(),
        "a denied confirm removes nothing"
    );

    let confirmed = fixture.json(&["gc", "deployments", "--confirm", "--json"]);
    assert_eq!(confirmed["dry_run"], false);
    assert_eq!(confirmed["entries_removed"], 4);
    assert_eq!(confirmed["bytes_reclaimed"], removable);
    let removed: Vec<_> = actions(&confirmed)
        .into_iter()
        .filter(|(_, action, ..)| action == "removed")
        .map(|(path, ..)| path)
        .collect();
    assert_eq!(
        removed,
        [
            old.to_str().unwrap().to_owned(),
            path("deployments/linked"),
            path("bin/orbit-graph.stale-2026"),
            path("orbit.db.bak-1"),
        ]
    );
    for gone in [
        &old,
        &root.join("bin/orbit-graph.stale-2026"),
        &root.join("orbit.db.bak-1"),
    ] {
        assert!(!gone.exists(), "{} should be removed", gone.display());
    }
    assert!(fs::symlink_metadata(root.join("deployments/linked")).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside the orbit home");
    for kept in [
        &newest,
        &second,
        &pinned,
        &root.join("bin/orbit.bak-pre-1"),
        &root.join("bin/orbit.before-abc"),
        &root.join("orbit.db.bak-3"),
        &root.join("orbit.db.bak-2"),
    ] {
        assert!(kept.exists(), "{} should be kept", kept.display());
    }
    assert_eq!(fs::read(root.join("bin/orbit")).unwrap(), installed);
    assert_eq!(fs::read(root.join("bin/orbit.previous")).unwrap(), rollback);
    assert_eq!(
        fs::read(root.join("bin/orbit-graph")).unwrap(),
        b"another tool"
    );
    assert!(root.join("bin/install-1.json").is_file());
    assert!(
        root.join("orbit.db").is_file(),
        "the live store is never a candidate"
    );

    let again = fixture.json(&["gc", "deployments", "--keep", "1", "--json"]);
    let selected: Vec<_> = actions(&again)
        .into_iter()
        .filter(|(_, action, ..)| action == "would_remove")
        .map(|(path, ..)| path)
        .collect();
    assert_eq!(
        selected,
        [
            second.to_str().unwrap().to_owned(),
            path("bin/orbit.before-abc"),
            path("orbit.db.bak-2"),
        ],
        "--keep 1 keeps only the newest of each class besides the rollback-pinned entries"
    );
    fixture
        .orbit()
        .args(["gc", "deployments", "--keep", "0"])
        .assert()
        .failure();
}
