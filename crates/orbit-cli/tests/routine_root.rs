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
    routine_name: String,
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
                "routine-root-host",
                "--task-prefix",
                "RR",
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
                "routine-root",
            ],
            None,
        );
        assert_home_empty(&home);

        let list = run_json(
            &repo,
            &home,
            &["--root", &root_arg, "routine", "list", "--format", "json"],
            None,
        );
        let routine_name = list["routines"]
            .as_array()
            .and_then(|routines| routines.first())
            .and_then(|routine| routine["name"].as_str())
            .unwrap_or_else(|| panic!("seeded routine name in custom-root list: {list}"))
            .to_string();

        Self {
            _temp: temp,
            home,
            repo,
            root,
            routine_name,
        }
    }
}

#[test]
fn routine_list_honors_explicit_root_over_uninitialized_home_and_environment() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();
    let uninitialized_env_root = fixture.home.join(".orbit");

    let list = run_json(
        &fixture.repo,
        &fixture.home,
        &["--root", &root_arg, "routine", "list", "--format", "json"],
        Some(&uninitialized_env_root),
    );

    assert_eq!(list["host_id"], "routine-root-host");
    let routines = list["routines"].as_array().expect("routine list array");
    assert!(
        routines.len() >= 6,
        "expected the seeded routines from the custom root: {list}"
    );
    assert!(
        routines.iter().any(|routine| {
            routine["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("ci-failure-sweep-"))
        }),
        "custom-root routine list omitted ci_failure_sweep: {list}"
    );
    assert_home_empty(&fixture.home);
}

#[test]
fn routine_commands_honor_orbit_root_and_mutate_only_the_selected_root() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();

    let list = run_json(
        &fixture.repo,
        &fixture.home,
        &["routine", "list", "--format", "json"],
        Some(&fixture.root),
    );
    assert_eq!(list["host_id"], "routine-root-host");

    run_success(
        &fixture.repo,
        &fixture.home,
        &[
            "--root",
            &root_arg,
            "routine",
            "pause",
            &fixture.routine_name,
        ],
        None,
    );
    let paused = run_json(
        &fixture.repo,
        &fixture.home,
        &["routine", "show", &fixture.routine_name, "--format", "json"],
        Some(&fixture.root),
    );
    assert!(
        paused["paused_at"].is_string(),
        "routine was not paused: {paused}"
    );

    run_success(
        &fixture.repo,
        &fixture.home,
        &["routine", "resume", &fixture.routine_name],
        Some(&fixture.root),
    );
    let resumed = run_json(
        &fixture.repo,
        &fixture.home,
        &[
            "--root",
            &root_arg,
            "routine",
            "show",
            &fixture.routine_name,
            "--format",
            "json",
        ],
        None,
    );
    assert!(
        resumed["paused_at"].is_null(),
        "routine stayed paused: {resumed}"
    );
    assert_home_empty(&fixture.home);
}

fn run_success(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) {
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
            .env("ORBIT_RUN_ID", "routine-root-test")
            .env("ORBIT_REGISTRY_ROOT", root);
    }
    command.args(args).assert().success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) -> Value {
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
            .env("ORBIT_RUN_ID", "routine-root-test")
            .env("ORBIT_REGISTRY_ROOT", root);
    }
    let output = command
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
        "routine command touched isolated HOME at {}",
        home.display()
    );
}

fn init_git_repo(repo: &Path) {
    run_git(repo, &["init", "--quiet"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# routine root test\n").expect("write readme");
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
