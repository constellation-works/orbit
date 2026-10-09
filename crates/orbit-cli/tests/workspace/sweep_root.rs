#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn initialized() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("empty-home");
        let repo = temp.path().join("repo");
        let root = temp.path().join("custom-root");
        fs::create_dir_all(&home).expect("create empty home");
        fs::create_dir_all(&repo).expect("create repo");
        init_git_repo(&repo);

        let root_arg = root.to_string_lossy().into_owned();
        run_success(
            &repo,
            &home,
            &[
                "--root",
                &root_arg,
                "init",
                "--non-interactive",
                "--machine-name",
                "sweep-root-host",
                "--task-prefix",
                "SR",
            ],
            None,
        );
        run_success(
            &repo,
            &home,
            &[
                "--root",
                &root_arg,
                "workspace",
                "init",
                "--name",
                "sweep-root",
            ],
            None,
        );
        assert_home_empty(&home);

        Self {
            _temp: temp,
            home,
            repo,
            root,
        }
    }
}

#[test]
fn sweep_honors_explicit_root_over_uninitialized_home() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();

    let outcome = run_json(
        &fixture.repo,
        &fixture.home,
        &[
            "sweep",
            "--root",
            &root_arg,
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );

    assert_sweep_used_custom_root(&outcome);
    assert_home_empty(&fixture.home);
}

#[test]
fn sweep_honors_orbit_root_over_uninitialized_home() {
    let fixture = Fixture::initialized();

    let outcome = run_json(
        &fixture.repo,
        &fixture.home,
        &["sweep", "--dry-run", "--format", "json"],
        Some(&fixture.root),
    );

    assert_sweep_used_custom_root(&outcome);
    assert_home_empty(&fixture.home);
}

#[test]
fn sweep_alias_and_clock_tick_have_identical_json_output() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();
    let shared = ["--root", root_arg.as_str(), "--dry-run", "--format", "json"];

    let sweep = run_json(
        &fixture.repo,
        &fixture.home,
        &[
            "sweep", shared[0], shared[1], shared[2], shared[3], shared[4],
        ],
        None,
    );
    let tick = run_json(
        &fixture.repo,
        &fixture.home,
        &[
            "clock", "tick", shared[0], shared[1], shared[2], shared[3], shared[4],
        ],
        None,
    );

    assert_eq!(sweep, tick);
}

#[test]
fn routine_clock_is_not_a_compatibility_alias() {
    let fixture = Fixture::initialized();

    command(&fixture.repo, &fixture.home, &["routine", "clock"], None)
        .args(["routine", "clock"])
        .assert()
        .failure();
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_flags_an_installed_clock_without_deadline_or_descendant_cleanup() {
    let fixture = Fixture::initialized();
    let unit_dir = fixture.home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).unwrap();
    let program = env!("CARGO_BIN_EXE_orbit")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    for settings in ["", "TimeoutStartSec=infinity\nKillMode=process\n"] {
        fs::write(
            unit_dir.join("orbit-sweep.service"),
            format!("[Service]\nType=oneshot\n{settings}ExecStart=\"{program}\" clock tick\n"),
        )
        .unwrap();
        let root_arg = fixture.root.to_string_lossy();
        let args = ["--root", root_arg.as_ref(), "doctor", "--format", "json"];
        let output = command(&fixture.repo, &fixture.home, &args, None)
            .args(args)
            .output()
            .unwrap();
        let rows: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "doctor output: {error}; {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let clock = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["check"] == "clock-unit")
            .expect("doctor includes installed clock check");
        assert_eq!(
            clock["status"], "warning",
            "unsafe unit must be actionable: {clock}"
        );
        assert!(
            clock["remediation"].is_string(),
            "unsafe unit must offer repair: {clock}"
        );
    }
}

fn assert_sweep_used_custom_root(outcome: &Value) {
    assert_eq!(outcome["machine_name"], "sweep-root-host");
    assert_eq!(outcome["dry_run"], true);
    assert!(
        outcome["reports"]
            .as_array()
            .is_some_and(|reports| !reports.is_empty()),
        "expected seeded routines from the custom root: {outcome}"
    );
}

fn run_success(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) {
    command(cwd, home, args, orbit_root)
        .args(args)
        .assert()
        .success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) -> Value {
    let output = command(cwd, home, args, orbit_root)
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap_or_else(|error| {
        panic!(
            "parse JSON from `orbit {}`: {error}\nstdout:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output)
        )
    })
}

fn command(
    cwd: &Path,
    home: &Path,
    args: &[&str],
    orbit_root: Option<&Path>,
) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    if let Some(root) = orbit_root {
        command.env("ORBIT_ROOT", root);
    }
    if let Some(root) = managed_registry_root(args, orbit_root) {
        command
            .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
            .env("ORBIT_RUN_ID", "sweep-root-test")
            .env("ORBIT_REGISTRY_ROOT", root);
    }
    command
}

fn managed_registry_root(args: &[&str], orbit_root: Option<&Path>) -> Option<PathBuf> {
    args.windows(2)
        .find_map(|pair| (pair[0] == "--root").then(|| PathBuf::from(pair[1])))
        .or_else(|| orbit_root.map(Path::to_path_buf))
}

fn assert_home_empty(home: &Path) {
    assert!(
        fs::read_dir(home)
            .expect("read isolated home")
            .next()
            .is_none(),
        "sweep touched isolated HOME at {}",
        home.display()
    );
}

fn init_git_repo(repo: &Path) {
    crate::git_repo::init(repo);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# sweep root test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = crate::git_repo::command()
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
