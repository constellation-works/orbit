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
                "--machine-name",
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

    assert_eq!(list["machine_name"], "routine-root-host");
    let routines = list["routines"].as_array().expect("routine list array");
    let expected_prefixes = [
        "ci-failure-sweep-",
        "dependabot-alert-sweep-",
        "ship-sweep-",
        "task-pilot-",
        "worktree-gc-",
    ];
    assert_eq!(
        routines.len(),
        expected_prefixes.len(),
        "expected exactly the active seeded routines from the custom root: {list}"
    );
    for prefix in expected_prefixes {
        assert!(
            routines.iter().any(|routine| {
                routine["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(prefix))
            }),
            "custom-root routine list omitted {prefix}: {list}"
        );
    }
    assert!(!routines.iter().any(|routine| {
        routine["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("task-triage-"))
    }));
    assert_home_empty(&fixture.home);
}

#[test]
fn routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let root_repo = temp.path().join("root-repo");
    fs::create_dir_all(&home).expect("create isolated home");
    fs::create_dir_all(&root_repo).expect("create root repo");
    init_git_repo(&root_repo);
    run_success(
        &root_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "routine-filter-host",
            "--task-prefix",
            "RF",
        ],
        None,
    );
    run_success(
        &root_repo,
        &home,
        &["workspace", "init", "--name", "routine-root"],
        None,
    );

    let other_repo = temp.path().join("other-repo");
    fs::create_dir_all(&other_repo).expect("create other repo");
    init_git_repo(&other_repo);
    run_success(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other-workspace"],
        None,
    );
    let other_routines_dir = other_repo.join(".orbit/routines");
    let routine_paths = fs::read_dir(&other_routines_dir)
        .expect("read other workspace routines")
        .map(|entry| entry.expect("routine directory entry").path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("yaml"))
        })
        .collect::<Vec<_>>();
    assert!(routine_paths.len() > 1, "fixture seeds multiple routines");
    for path in routine_paths.iter().skip(1) {
        fs::remove_file(path).expect("remove unrelated seeded routine");
    }

    let empty_repo = temp.path().join("empty-repo");
    fs::create_dir_all(&empty_repo).expect("create empty repo");
    init_git_repo(&empty_repo);
    run_success(
        &empty_repo,
        &home,
        &["workspace", "init", "--name", "empty-workspace"],
        None,
    );
    let empty_routines_dir = empty_repo.join(".orbit/routines");
    if empty_routines_dir.exists() {
        fs::remove_dir_all(empty_routines_dir)
            .expect("remove the empty workspace's seeded routines");
    }

    let other_list = run_json(
        &root_repo,
        &home,
        &[
            "routine",
            "list",
            "--workspace",
            "other-workspace",
            "--format",
            "json",
        ],
        None,
    );
    let other_routines = other_list["routines"]
        .as_array()
        .expect("selected routine array");
    assert_eq!(other_routines.len(), 1, "{other_list}");
    assert_eq!(other_routines[0]["source"], "other-workspace");

    let empty_list = run_json(
        &root_repo,
        &home,
        &[
            "routine",
            "list",
            "--workspace",
            "empty-workspace",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(empty_list["routines"], serde_json::json!([]));
    let empty_text = run_text(
        &root_repo,
        &home,
        &["routine", "list", "--workspace", "empty-workspace"],
        None,
    );
    assert!(
        empty_text.contains("no routines found in workspace 'empty-workspace'"),
        "{empty_text}"
    );
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
    assert_eq!(list["machine_name"], "routine-root-host");

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

fn run_text(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) -> String {
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
    let output = command.args(args).assert().success().get_output().clone();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
