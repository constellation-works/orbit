#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

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
                "--host-name",
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

fn assert_sweep_used_custom_root(outcome: &Value) {
    assert_eq!(outcome["host_id"], "sweep-root-host");
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
    run_git(repo, &["init", "--quiet"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# sweep root test\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "--quiet", "-m", "initial"]);
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
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
