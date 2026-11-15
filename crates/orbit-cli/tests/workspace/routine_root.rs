#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
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
        crate::git_repo::SeededRepo::new(&repo)
            .readme("# routine root test\n")
            .build();

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
fn removed_checkout_validation_preserves_each_command_persistence_policy() {
    if crate::isolated_test::run_in_child(
        "routine_root::removed_checkout_validation_preserves_each_command_persistence_policy",
    ) {
        return;
    }
    let fixture = Fixture::initialized();
    let root = fixture.root.to_str().expect("root path");
    let registry_path = fixture.root.join("workspaces.json");
    let before = fs::read(&registry_path).expect("registry before checkout removal");
    fs::rename(&fixture.repo, fixture.repo.with_extension("removed")).expect("move checkout");

    let list = run_json(
        &fixture.root,
        &fixture.home,
        &["--root", root, "routine", "list", "--json"],
        None,
    );
    assert_eq!(list["routines"], serde_json::json!([]), "{list}");
    assert_eq!(fs::read(&registry_path).unwrap(), before);

    let listed = run_json(
        &fixture.root,
        &fixture.home,
        &["--root", root, "workspace", "list", "--json"],
        None,
    );
    assert_eq!(listed[0]["status"], "invalid", "{listed}");
    let persisted: Value = serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    assert_eq!(persisted["workspaces"][0]["status"], "invalid");

    // Clock discovery persists validation even when no workspace can be swept.
    fs::write(&registry_path, &before).expect("restore recorded active status");
    let sweep = run_json(
        &fixture.root,
        &fixture.home,
        &["--root", root, "clock", "tick", "--dry-run", "--json"],
        None,
    );
    assert_eq!(sweep["reports"], serde_json::json!([]), "{sweep}");
    let persisted: Value = serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    assert_eq!(persisted["workspaces"][0]["status"], "invalid");
}

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
    assert!(
        routines
            .iter()
            .any(|routine| routine["name"] == fixture.routine_name),
        "custom-root routine list omitted its fixture-captured seeded routine: {list}"
    );
    assert_home_empty(&fixture.home);
}

fn routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let root_repo = temp.path().join("root-repo");
    fs::create_dir_all(&home).expect("create isolated home");
    fs::create_dir_all(&root_repo).expect("create root repo");
    crate::git_repo::SeededRepo::new(&root_repo)
        .readme("# routine root test\n")
        .build();
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
    crate::git_repo::SeededRepo::new(&other_repo)
        .readme("# routine root test\n")
        .build();
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
    crate::git_repo::SeededRepo::new(&empty_repo)
        .readme("# routine root test\n")
        .build();
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

/// `routine enable`/`disable` select a routine host-wide but audit under the
/// workspace the command runs in, so a routine of another workspace is refused
/// without a write and a routine of the invoking workspace is audited there.
#[test]
fn routine_toggle_edits_and_audits_only_the_invoking_workspace() {
    if crate::isolated_test::run_in_child(
        "routine_root::routine_toggle_edits_and_audits_only_the_invoking_workspace",
    ) {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create isolated home");
    let mut repos = Vec::new();
    for (index, workspace) in ["toggle-a", "toggle-b"].into_iter().enumerate() {
        let repo = temp.path().join(workspace);
        fs::create_dir_all(&repo).expect("create repo");
        crate::git_repo::SeededRepo::new(&repo)
            .readme("# routine toggle test\n")
            .build();
        if index == 0 {
            run_success(
                &repo,
                &home,
                &[
                    "init",
                    "--non-interactive",
                    "--machine-name",
                    "routine-toggle-host",
                    "--task-prefix",
                    "RT",
                ],
                None,
            );
        }
        run_success(
            &repo,
            &home,
            &["workspace", "init", "--name", workspace],
            None,
        );
        let routine = format!("resident-cycle-{}", &workspace[7..]);
        fs::write(
            repo.join(format!(".orbit/routines/{routine}.yaml")),
            format!(
                "schemaVersion: 1\nname: {routine}\nenabled: false\ntrigger:\n  \
                 cron: \"17 5 * * 1\"\n  missed_run: skip\ntarget: job:store_gc_pipeline\n"
            ),
        )
        .expect("write the workspace's own routine");
        repos.push((workspace, repo, routine));
    }
    let (name_a, repo_a, routine_a) = &repos[0];
    let (name_b, repo_b, routine_b) = &repos[1];
    let definition = |repo: &Path, routine: &str| {
        fs::read_to_string(repo.join(format!(".orbit/routines/{routine}.yaml")))
            .expect("read routine definition")
    };
    let applied_toggles = |workspace_repo: &Path| -> Vec<String> {
        let id = workspace_id(workspace_repo);
        run_json(
            workspace_repo,
            &home,
            &["audit", "list", "--workspace-id", &id, "--json"],
            None,
        )
        .as_array()
        .expect("audit rows")
        .iter()
        // A refused attempt is audited as a failure under the invoking workspace.
        .filter(|row| row["command"] == "routine" && row["status"] == "success")
        .map(|row| {
            format!(
                "{}:{}",
                row["subcommand"].as_str().unwrap_or_default(),
                row["target_id"].as_str().unwrap_or_default()
            )
        })
        .collect()
    };
    // The toggle verbs are operator-only; the override is recorded in the audit.
    let operator = |cwd: &Path, args: &[&str]| {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("ORBIT_OPERATOR", "1")
            .args(args)
            .assert()
    };
    let refuse = |cwd: &Path, args: &[&str]| {
        let output = operator(cwd, args).failure().get_output().clone();
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let (before_a, before_b) = (definition(repo_a, routine_a), definition(repo_b, routine_b));

    // An explicit selector for A cannot reach B's routine, in either direction.
    let stderr = refuse(
        repo_a,
        &["--workspace", name_a, "routine", "enable", routine_b],
    );
    assert!(
        stderr.contains("workspace_mismatch:")
            && stderr.contains(name_b)
            && stderr.contains("--workspace"),
        "mismatch carries the shared code and names the routine's workspace: {stderr}"
    );
    // An unlisted name is the shared `routine_not_found` refusal.
    let stderr = refuse(repo_a, &["routine", "enable", "no-such-routine"]);
    assert!(
        stderr.contains("routine_not_found: routine 'no-such-routine' was not found"),
        "unlisted routine reports the shared refusal: {stderr}"
    );
    // Neither does the cwd: running from A cannot enable B's routine.
    refuse(repo_a, &["routine", "enable", routine_b]);
    refuse(
        repo_b,
        &["--workspace", name_b, "routine", "disable", routine_a],
    );
    assert_eq!(
        (definition(repo_a, routine_a), definition(repo_b, routine_b)),
        (before_a.clone(), before_b.clone()),
        "a refused toggle changes neither definition"
    );

    // Selecting the routine's own workspace works from any cwd, and the audit
    // row lands under that workspace rather than the cwd's.
    for (verb, enabled) in [("enable", true), ("disable", false)] {
        let stdout = operator(
            repo_a,
            &["--workspace", name_b, "routine", verb, routine_b, "--json"],
        )
        .success()
        .get_output()
        .stdout
        .clone();
        let out: Value = serde_json::from_slice(&stdout).expect("toggle JSON");
        assert_eq!(out["enabled"], enabled, "{out}");
        assert_eq!(out["changed"], true, "{out}");
        assert_eq!(
            definition(repo_b, routine_b).contains("enabled: true"),
            enabled,
            "B's definition follows `{verb}`"
        );
        assert_eq!(definition(repo_a, routine_a), before_a, "A is untouched");
    }
    operator(repo_a, &["routine", "enable", routine_a]).success();
    assert!(definition(repo_a, routine_a).contains("enabled: true"));
    // Re-enabling reports the shared outcome's `changed: false`.
    let stdout = operator(repo_a, &["routine", "enable", routine_a, "--json"])
        .success()
        .get_output()
        .stdout
        .clone();
    let out: Value = serde_json::from_slice(&stdout).expect("toggle JSON");
    assert_eq!(out["changed"], false, "{out}");
    assert_eq!(
        definition(repo_b, routine_b),
        before_b,
        "B is back to its disabled definition"
    );

    assert_eq!(
        applied_toggles(repo_b),
        [
            format!("disable:{routine_b}"),
            format!("enable:{routine_b}")
        ],
        "B's audit holds exactly the toggles applied to B, newest first"
    );
    assert_eq!(
        applied_toggles(repo_a),
        [format!("enable:{routine_a}"), format!("enable:{routine_a}")],
        "A's audit holds only the toggles applied to A (the second unchanged), not the ones aimed at B"
    );
}

fn workspace_id(repo: &Path) -> String {
    let config: serde_yaml::Value = serde_yaml::from_slice(
        &fs::read(repo.join(".orbit/config.yaml")).expect("read workspace config"),
    )
    .expect("parse workspace config");
    config["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_owned()
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
    let entries = crate::isolated_home::isolated_home_entries(home);
    assert!(
        entries.is_empty(),
        "routine command touched isolated HOME at {}; found:\n{}",
        home.display(),
        entries.join("\n")
    );
}

/// A routine and an auto-task seeded by a plugin this host never enabled are
/// hidden from both listings (JSON and text), listed marked inactive on
/// request, and still resolve through `show` with the reason.
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

/// STD-01 R31: a read-only routine command must not create the scheduler
/// database on a host whose store is absent. It fails with the setup remedy
/// and leaves the root tree unchanged, including when the root is unwritable.
#[test]
fn routine_status_reads_fail_without_creating_missing_scheduler_database() {
    if crate::isolated_test::run_in_child(
        "routine_root::routine_status_reads_fail_without_creating_missing_scheduler_database",
    ) {
        return;
    }
    let fixture = Fixture::initialized();
    let root = fixture.root.to_str().expect("root path");
    let database = fixture.root.join("orbit.db");
    assert!(
        database.exists(),
        "init must create the scheduler database this regression removes"
    );
    fs::remove_file(&database).expect("remove scheduler database to model a missing store");
    let before = tree_entries(&fixture.root);

    for args in [
        vec!["--root", root, "routine", "list", "--format", "json"],
        vec![
            "--root",
            root,
            "routine",
            "show",
            fixture.routine_name.as_str(),
        ],
    ] {
        let stderr = run_failure(&fixture.repo, &fixture.home, &args, None);
        assert!(
            stderr.contains("orbit init"),
            "`orbit {}` must name the setup remedy for a missing store: {stderr}",
            args.join(" ")
        );
        assert!(
            !database.exists(),
            "`orbit {}` created the scheduler database during observation",
            args.join(" ")
        );
        assert_eq!(
            tree_entries(&fixture.root),
            before,
            "`orbit {}` changed the root tree",
            args.join(" ")
        );
        assert_home_empty(&fixture.home);
    }

    #[cfg(unix)]
    assert_unwritable_root_read_fails_without_writes(&fixture, &database, &before);
}

/// Unwritable storage must fail the same way: the read reports the setup
/// requirement instead of attempting a write.
#[cfg(unix)]
fn assert_unwritable_root_read_fails_without_writes(
    fixture: &Fixture,
    database: &Path,
    before: &[String],
) {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture.root.to_str().expect("root path");
    let mut permissions = fs::metadata(&fixture.root)
        .expect("root metadata")
        .permissions();
    let writable = permissions.clone();
    permissions.set_mode(0o555);
    fs::set_permissions(&fixture.root, permissions).expect("make root unwritable");
    let stderr = run_failure(
        &fixture.repo,
        &fixture.home,
        &["--root", root, "routine", "list", "--format", "json"],
        None,
    );
    fs::set_permissions(&fixture.root, writable).expect("restore root permissions");
    assert!(
        stderr.contains("orbit init"),
        "unwritable missing-store list must name the setup remedy: {stderr}"
    );
    assert!(
        !database.exists(),
        "unwritable list created the scheduler database"
    );
    assert_eq!(tree_entries(&fixture.root), before);
}

/// The durable entries under `root`. `.generation-participants/` is excluded:
/// every CLI process records a process-admission entry there, and one that
/// exits through an error path leaves it behind. It holds no routine state.
fn tree_entries(root: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read directory") {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            if path
                .file_name()
                .is_some_and(|name| name == ".generation-participants")
            {
                continue;
            }
            let is_dir = entry.file_type().expect("entry type").is_dir();
            let relative = path.strip_prefix(root).expect("relative path");
            entries.push(format!("{} dir={is_dir}", relative.display()));
            if is_dir {
                pending.push(path);
            }
        }
    }
    entries.sort();
    entries
}

fn run_failure(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) -> String {
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
    let output = command.args(args).assert().failure().get_output().clone();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn routine_command_contracts_0() {
    for (name, check) in [
        (
            "routine_list_preserves_unchanged_registry_modification_time",
            routine_list_preserves_unchanged_registry_modification_time as fn(),
        ),
        (
            "routine_list_prints_next_due_and_last_fire_in_one_zone",
            routine_list_prints_next_due_and_last_fire_in_one_zone as fn(),
        ),
        (
            "routine_list_honors_explicit_root_over_uninitialized_home_and_environment",
            routine_list_honors_explicit_root_over_uninitialized_home_and_environment as fn(),
        ),
        (
            "routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces",
            routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}

#[test]
fn routine_command_contracts_1() {
    for (name, check) in [
        (
            "routine_commands_honor_orbit_root_and_mutate_only_the_selected_root",
            routine_commands_honor_orbit_root_and_mutate_only_the_selected_root as fn(),
        ),
        (
            "routine_pause_rejects_unknown_names_and_reports_json_for_known_ones",
            routine_pause_rejects_unknown_names_and_reports_json_for_known_ones as fn(),
        ),
        (
            "plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name",
            plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name
                as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}
