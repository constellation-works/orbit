//! Forced init must validate its replacement identity before deleting the root.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_registry::load_machine_identity;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn initialized() -> Self {
        let temp = tempdir().expect("fixture tempdir");
        let fixture = Self {
            home: temp.path().join("home"),
            work: temp.path().join("work"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).expect("fixture home");
        crate::git_repo::seal_lookup_boundary(&fixture.work);
        let output = fixture.run(&[
            "--non-interactive",
            "--machine-name",
            "original-host",
            "--task-prefix",
            "QA",
        ]);
        assert!(output.status.success(), "initial init: {output:?}");
        fixture
    }

    fn root(&self) -> PathBuf {
        self.home.join(".orbit")
    }

    fn run(&self, flags: &[&str]) -> Output {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
            .env("PATH", self.work.join("empty-path"))
            .env_remove("RUST_LOG")
            .arg("init")
            .args(flags)
            .output()
            .expect("run orbit init")
    }
}

#[test]
fn force_refuses_missing_or_invalid_identity_flags_before_deleting_the_root() {
    let fixture = Fixture::initialized();
    let root = fixture.root();
    let config_path = root.join("config.toml");
    let before_config = fs::read(&config_path).expect("original config");
    let before_identity = load_machine_identity(&root).expect("valid original identity");
    let sentinel = root.join("state/operator-data");
    fs::create_dir_all(sentinel.parent().expect("state directory")).expect("state directory");
    fs::write(&sentinel, b"preserve existing state").expect("operator state");

    let cases: &[(&[&str], &str)] = &[
        (&[], "--machine-name and --task-prefix"),
        (&["--machine-name", "replacement"], "--task-prefix <PREFIX>"),
        (&["--task-prefix", "QA"], "--machine-name and --task-prefix"),
        (
            &["--machine-name", " ", "--task-prefix", "QA"],
            "machine name must not be empty",
        ),
        (
            &["--machine-name", "invalid/name", "--task-prefix", "QA"],
            "machine_name",
        ),
        (
            &["--machine-name", "replacement", "--task-prefix", "q"],
            "task prefix must be 2-5 uppercase ASCII letters",
        ),
    ];
    for (flags, cause) in cases {
        let mut reset_flags = vec!["--force", "--non-interactive"];
        reset_flags.extend_from_slice(flags);
        let output = fixture.run(&reset_flags);
        assert!(!output.status.success(), "reset must refuse: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(cause), "expected {cause:?}: {stderr}");
        assert_eq!(
            fs::read(&config_path).expect("config survives refusal"),
            before_config,
            "a rejected forced init must not replace config.toml"
        );
        assert_eq!(
            load_machine_identity(&root).expect("identity survives refusal"),
            before_identity,
            "a rejected forced init must preserve the valid machine identity"
        );
        assert_eq!(
            fs::read(&sentinel).expect("operator state survives refusal"),
            b"preserve existing state",
            "a rejected forced init must not delete existing state"
        );
    }
}

#[test]
fn force_with_valid_identity_flags_resets_the_root_and_creates_a_new_identity() {
    let fixture = Fixture::initialized();
    let root = fixture.root();
    let before = load_machine_identity(&root).expect("original identity");
    let sentinel = root.join("operator-data");
    fs::write(&sentinel, b"old state").expect("operator state");

    let output = fixture.run(&[
        "--force",
        "--non-interactive",
        "--machine-name",
        "replacement-host",
        "--task-prefix",
        "ZZ",
    ]);
    assert!(output.status.success(), "explicit force reset: {output:?}");
    let after = load_machine_identity(&root).expect("valid replacement identity");
    assert_ne!(after.id, before.id, "a full reset creates a new machine ID");
    assert_eq!(after.name, "replacement-host");
    assert_eq!(after.task_prefix, "ZZ");
    assert!(
        !sentinel.exists(),
        "an accepted full reset deletes old state"
    );
    assert!(root.join("resources/executors").is_dir());
}

#[test]
fn ordinary_reinit_still_ignores_identity_flags_and_preserves_the_identity() {
    let fixture = Fixture::initialized();
    let root = fixture.root();
    let before = load_machine_identity(&root).expect("original identity");
    let output = fixture.run(&[
        "--non-interactive",
        "--machine-name",
        "invalid/name",
        "--task-prefix",
        "q",
    ]);
    assert!(output.status.success(), "ordinary re-init: {output:?}");
    assert_eq!(
        load_machine_identity(&root).expect("existing identity"),
        before
    );
}
