#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, SystemTime};

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
fn routine_list_preserves_unchanged_registry_modification_time() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();
    let registry_path = fixture.root.join("workspaces.json");
    let contents = fs::read(&registry_path).expect("read initialized registry");
    // A rewrite must get a different timestamp, even on coarse-resolution filesystems.
    fs::File::options()
        .write(true)
        .open(&registry_path)
        .expect("open registry to set modification time")
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000))
        .expect("set registry modification time");
    let modified = fs::metadata(&registry_path)
        .expect("registry metadata before listing")
        .modified()
        .expect("registry modification time before listing");

    let list = run_json(
        &fixture.repo,
        &fixture.home,
        &["--root", &root_arg, "routine", "list", "--format", "json"],
        None,
    );

    assert!(
        list["routines"]
            .as_array()
            .is_some_and(|routines| !routines.is_empty()),
        "routine status discovery must visit the registered workspace: {list}"
    );
    assert_eq!(
        fs::metadata(&registry_path)
            .expect("registry metadata after listing")
            .modified()
            .expect("registry modification time after listing"),
        modified,
        "read-only routine discovery must preserve the registry runtime cache stamp"
    );
    assert_eq!(
        fs::read(&registry_path).expect("read registry after listing"),
        contents,
        "read-only routine discovery must preserve the registry contents"
    );
    assert_home_empty(&fixture.home);
}

/// NEXT DUE is computed in the host zone while a fire is recorded in UTC; the
/// table reads both in the host zone [ORB-14709].
#[test]
fn routine_list_prints_next_due_and_last_fire_in_one_zone() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();
    let args = ["--root", root_arg.as_str(), "routine", "list"];
    let list = run_json(
        &fixture.repo,
        &fixture.home,
        &[args.as_slice(), &["--format", "json"]].concat(),
        None,
    );
    let scheduled = list["routines"]
        .as_array()
        .and_then(|routines| {
            routines
                .iter()
                .find(|routine| routine["next_due"].is_string())
        })
        .unwrap_or_else(|| panic!("a cron-scheduled seeded routine: {list}"))["name"]
        .as_str()
        .expect("routine name")
        .to_string();
    let database =
        rusqlite::Connection::open(fixture.root.join("orbit.db")).expect("host database");
    database
        .execute(
            "INSERT INTO routine_fires (routine_name, slot, attempt, state, run_id, source_workspace, created_at, updated_at) \
             VALUES (?1, '2026-01-15T08:00:00+00:00', 1, 'succeeded', 'jrun-zone-fixture', 'routine-root', \
                     '2026-01-15T08:00:01+00:00', '2026-01-15T08:01:00+00:00')",
            [&scheduled],
        )
        .expect("record a UTC fire");
    drop(database);

    // A zone without daylight saving, so one offset names the whole row.
    let text = run_text_with_env(
        &fixture.repo,
        &fixture.home,
        &args,
        None,
        &[("TZ", "Asia/Kolkata")],
    );
    let row = text
        .lines()
        .find(|line| line.contains(&scheduled))
        .unwrap_or_else(|| panic!("a row for {scheduled}:\n{text}"));
    let instants = regex::Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[+-]\d\d:\d\d")
        .expect("instant pattern")
        .find_iter(row)
        .map(|found| found.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        instants.len(),
        2,
        "NEXT DUE and LAST FIRE are both set:\n{row}"
    );
    assert!(
        instants.iter().all(|instant| instant.ends_with("+05:30")),
        "both instants read in the host zone:\n{row}"
    );
    assert!(
        row.contains("succeeded @ 2026-01-15T13:30:00+05:30"),
        "the UTC fire reads as the same instant in the host zone:\n{row}"
    );
    assert_home_empty(&fixture.home);
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
        "store-gc-",
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

#[test]
fn routine_pause_rejects_unknown_names_and_reports_json_for_known_ones() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();

    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .args(["--root", &root_arg, "routine", "pause", "no-such-routine"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no routine named 'no-such-routine'",
        ));

    let paused = run_json(
        &fixture.repo,
        &fixture.home,
        &[
            "--root",
            &root_arg,
            "routine",
            "pause",
            &fixture.routine_name,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(paused["routine"], fixture.routine_name.as_str());
    assert_eq!(paused["paused"], true);
    assert_eq!(paused["changed"], true);

    let resumed = run_json(
        &fixture.repo,
        &fixture.home,
        &[
            "--root",
            &root_arg,
            "routine",
            "resume",
            &fixture.routine_name,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(resumed["paused"], false);
    assert_eq!(resumed["changed"], true);
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
    run_text_with_env(cwd, home, args, orbit_root, &[])
}

fn run_text_with_env(
    cwd: &Path,
    home: &Path,
    args: &[&str],
    orbit_root: Option<&Path>,
    envs: &[(&str, &str)],
) -> String {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .envs(envs.iter().copied());
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

/// A routine and an auto-task seeded by a plugin this host never enabled are
/// hidden from both listings (JSON and text), listed marked inactive on
/// request, and still resolve through `show` with the reason.
#[test]
fn plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name() {
    let fixture = Fixture::initialized();
    let root_arg = fixture.root.to_string_lossy().into_owned();
    let orbit = |args: &[&str]| {
        let mut full = vec!["--root", root_arg.as_str()];
        full.extend_from_slice(args);
        full.into_iter().map(str::to_string).collect::<Vec<_>>()
    };
    let json = |args: &[&str]| {
        let args = orbit(args);
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        run_json(&fixture.repo, &fixture.home, &args, None)
    };
    let text = |args: &[&str]| {
        let args = orbit(args);
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        run_text(&fixture.repo, &fixture.home, &args, None)
    };

    let seeded_routine = json(&["routine", "show", &fixture.routine_name, "--format", "json"]);
    let routines_dir = PathBuf::from(seeded_routine["path"].as_str().expect("routine path"))
        .parent()
        .expect("routines dir")
        .to_path_buf();
    fs::write(
        routines_dir.join("ghost-refresh.yaml"),
        "# provenance: plugin:ghost@1.0.0\nschemaVersion: 1\nname: ghost-refresh\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:ghost_refresh_pipeline\n",
    )
    .expect("write seeded routine");
    // Copy a shipped auto-task under the plugin's name and provenance.
    let defaults = json(&["auto-task", "list", "--format", "json"]);
    let template = defaults
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["name"].as_str())
        .unwrap_or_else(|| panic!("a default auto-task: {defaults}"))
        .to_string();
    let shown = json(&["auto-task", "show", &template, "--format", "json"]);
    let source = PathBuf::from(
        shown["definition_source"]["path"]
            .as_str()
            .expect("definition path"),
    );
    let body = fs::read_to_string(&source).expect("read default auto-task");
    fs::write(
        source.with_file_name("ghost-reindex.yaml"),
        format!(
            "# provenance: plugin:ghost@1.0.0\n{}",
            body.replace(&format!("name: {template}"), "name: ghost-reindex")
        ),
    )
    .expect("write seeded auto-task");

    let names = |value: &Value, key: Option<&str>| -> Vec<String> {
        key.map_or(value, |key| &value[key])
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|item| item["name"].as_str().map(str::to_string))
            .collect()
    };

    // Default listings hide both, in JSON and in text.
    let auto_tasks = json(&["auto-task", "list", "--format", "json"]);
    assert!(!names(&auto_tasks, None).contains(&"ghost-reindex".to_string()));
    assert!(names(&auto_tasks, None).contains(&template));
    assert!(!text(&["auto-task", "list"]).contains("ghost-reindex"));
    let routines = json(&["routine", "list", "--format", "json"]);
    assert!(!names(&routines, Some("retired")).contains(&"ghost-refresh".to_string()));
    assert!(!names(&routines, Some("routines")).contains(&"ghost-refresh".to_string()));
    assert!(!text(&["routine", "list"]).contains("ghost-refresh"));

    // The opt-in lists them, marked inactive with the reason.
    let all = json(&[
        "auto-task",
        "list",
        "--include-inactive-plugins",
        "--format",
        "json",
    ]);
    let ghost = all
        .as_array()
        .expect("array")
        .iter()
        .find(|item| item["name"] == "ghost-reindex")
        .unwrap_or_else(|| panic!("listed on request: {all}"));
    assert_eq!(ghost["plugin_inactive"], true);
    assert!(
        ghost["skipped_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit plugin enable ghost")),
        "{ghost}"
    );
    let listed = text(&["auto-task", "list", "--all"]);
    assert!(
        listed.contains("ghost-reindex") && listed.contains("inactive"),
        "{listed}"
    );
    let all_routines = json(&["routine", "list", "--all", "--format", "json"]);
    let parked = all_routines["retired"]
        .as_array()
        .expect("retired")
        .iter()
        .find(|item| item["name"] == "ghost-refresh")
        .unwrap_or_else(|| panic!("listed on request: {all_routines}"));
    assert_eq!(parked["plugin_inactive"], true);
    assert!(
        parked["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("plugin:ghost@1.0.0")),
        "{parked}"
    );

    // `show` resolves each by name and says why it is inactive.
    let auto_task = json(&["auto-task", "show", "ghost-reindex", "--format", "json"]);
    assert_eq!(auto_task["plugin_inactive"], true);
    assert!(auto_task["skipped_reason"].is_string(), "{auto_task}");
    assert!(
        text(&["auto-task", "show", "ghost-reindex"]).contains("inactive: seeded by plugin:ghost")
    );
    let routine = json(&["routine", "show", "ghost-refresh", "--format", "json"]);
    assert_eq!(routine["plugin_inactive"], true);
    assert_eq!(routine["effective"], false);
    assert!(
        text(&["routine", "show", "ghost-refresh"]).contains("Inactive: seeded by plugin:ghost")
    );
    assert_home_empty(&fixture.home);
}
