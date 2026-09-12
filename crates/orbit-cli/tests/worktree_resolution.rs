#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command as AssertCommand;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let main_repo = temp.path().join("repo");
    let linked_worktree = temp.path().join("repo-worktree");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&main_repo).expect("create main repo");

    run_git(&main_repo, &["init"]);
    run_git(&main_repo, &["config", "user.name", "Orbit Test"]);
    run_git(
        &main_repo,
        &["config", "user.email", "orbit-test@example.com"],
    );
    run_git(&main_repo, &["config", "commit.gpgsign", "false"]);
    fs::write(main_repo.join("README.md"), "# orbit\n").expect("write readme");
    run_git(&main_repo, &["add", "README.md"]);
    run_git(&main_repo, &["commit", "-m", "initial"]);
    run_git(
        &main_repo,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-worktree-resolution",
            linked_worktree.to_str().expect("utf8 worktree path"),
        ],
    );

    run_orbit_success(&main_repo, &home, &["workspace", "init"], None);

    let main_repo = fs::canonicalize(&main_repo).expect("canonicalize main repo");
    let linked_worktree = fs::canonicalize(&linked_worktree).expect("canonicalize linked worktree");
    let main_orbit = main_repo.join(".orbit");
    let linked_orbit = linked_worktree.join(".orbit");

    let from_main = run_orbit_json(&main_repo, &home, &["config", "show", "--json"], None);
    assert_root_fields(&from_main, &main_orbit, &main_orbit);

    let from_worktree =
        run_orbit_json(&linked_worktree, &home, &["config", "show", "--json"], None);
    assert_root_fields(&from_worktree, &main_orbit, &linked_orbit);
    assert!(
        !linked_orbit.exists(),
        "linked worktree local root should be resolved but not created"
    );

    let explicit_root = main_orbit.to_string_lossy().to_string();
    let from_root_override = run_orbit_json(
        &linked_worktree,
        &home,
        &["--root", &explicit_root, "config", "show", "--json"],
        None,
    );
    assert_root_fields(&from_root_override, &main_orbit, &main_orbit);

    let from_env = run_orbit_json(
        &linked_worktree,
        &home,
        &["config", "show", "--json"],
        Some(&main_orbit),
    );
    assert_root_fields(&from_env, &main_orbit, &main_orbit);
    assert!(
        !linked_orbit.exists(),
        "resolution should not materialize a linked-worktree .orbit directory"
    );
}

/// The real detached worker must bootstrap, claim, and finish its run while an
/// unrelated WAL writer holds the already-current task registry. The parent
/// keeps supervising the child; no fixture claims the run on its behalf.
#[test]
fn detached_worker_bootstraps_and_claims_while_registry_writer_is_held() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let custom_root = temp.path().join("custom-orbit");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);

    run_orbit_success(
        &repo,
        &home,
        &[
            "--root",
            custom_root.to_str().expect("utf8 custom root"),
            "workspace",
            "init",
        ],
        None,
    );
    pin_default_crew_for_isolated_root(&custom_root);

    let registry_path = custom_root.join("tasks/registry.db");
    let registry_writer = rusqlite::Connection::open(&registry_path).expect("open registry writer");
    registry_writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold registry WAL writer");

    let job_path = repo.join("root-override-smoke.yaml");
    fs::write(
        &job_path,
        r#"schemaVersion: 2
kind: Job
metadata:
  name: root_override_smoke
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      default_input:
        seconds: 0
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("write smoke job");

    let custom_root_arg = custom_root.to_string_lossy().into_owned();
    let job_path_arg = job_path.to_string_lossy().into_owned();
    let submitted_at = Instant::now();
    let submitted = run_orbit_json(
        &repo,
        &home,
        &[
            "--root",
            &custom_root_arg,
            "run",
            "job",
            &job_path_arg,
            "--json",
        ],
        None,
    );
    assert!(
        submitted_at.elapsed() < Duration::from_secs(4),
        "current-schema parent/worker bootstrap waited for the registry writer"
    );
    let run_id = submitted["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected run_id in {submitted}"))
        .to_string();
    assert_eq!(submitted["state"].as_str(), Some("submitted"));
    registry_writer
        .execute_batch("ROLLBACK")
        .expect("release registry writer");

    let shown = wait_for_run_terminal_state(&repo, &home, &custom_root, &run_id);
    let state = shown["run"]["state"].as_str().unwrap_or("missing");
    assert_eq!(
        state,
        "success",
        "{}",
        detached_worker_diagnostic(&custom_root, &run_id, &shown)
    );
    assert!(
        shown["run"]["pid"].as_u64().is_some(),
        "the real worker must claim its persisted run: {shown}"
    );
}

#[test]
fn doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let main_repo = temp.path().join("repo");
    let linked_worktree = temp.path().join("repo-doctor");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&main_repo).expect("create main repo");

    init_git_repo(&main_repo);
    run_git(
        &main_repo,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-worktree-doctor",
            linked_worktree.to_str().expect("utf8 worktree path"),
        ],
    );
    run_orbit_success(&main_repo, &home, &["workspace", "init"], None);

    let local_marker = linked_worktree.join(".orbit/graph/local.db");
    let shared_marker = main_repo.join(".orbit/knowledge/graph/shared.db");
    for marker in [&local_marker, &shared_marker] {
        fs::create_dir_all(marker.parent().expect("graph parent")).expect("create graph parent");
        fs::write(marker, b"retired").expect("write graph marker");
    }

    let ordinary = run_orbit_json(&linked_worktree, &home, &["doctor", "--json"], None);
    assert!(local_marker.exists());
    assert!(shared_marker.exists());
    assert!(
        ordinary
            .as_array()
            .expect("doctor rows")
            .iter()
            .all(|row| row["check"] != "graph-index")
    );

    let cleaned = run_orbit_json(
        &linked_worktree,
        &home,
        &["doctor", "--remove-graph", "--json"],
        None,
    );
    assert!(!local_marker.exists());
    assert!(!shared_marker.exists());
    assert!(
        cleaned
            .as_array()
            .expect("doctor rows")
            .iter()
            .all(|row| row["check"] != "graph-index")
    );

    // Parsing succeeds again with no cleanup prose mixed into stdout, and
    // absence remains a successful no-op.
    run_orbit_json(
        &linked_worktree,
        &home,
        &["doctor", "--remove-graph", "--json"],
        None,
    );
}

/// ORB-10668: the operator path the tool surface could not serve — an ADR
/// authored inside a job worktree, carried proposed -> accepted with `orbit adr`
/// alone from that worktree, while the same command run from the hub still
/// fails closed on the federation guard.
fn assert_root_fields(value: &Value, shared_root: &Path, local_root: &Path) {
    let shared = shared_root.to_string_lossy();
    let local = local_root.to_string_lossy();
    assert_eq!(string_field(value, "shared_root"), shared.as_ref());
    assert_eq!(string_field(value, "local_root"), local.as_ref());
    assert!(
        value.get("workspace_root").is_none(),
        "legacy `workspace_root` alias must be removed from `config show --json` output (use `shared_root`)"
    );
    assert!(
        value.get("root").is_none(),
        "legacy `root` alias must be removed from `config show --json` output (use `shared_root`)"
    );
    assert!(
        value.get("selected_root").is_none(),
        "legacy `selected_root` alias must be removed from `config show --json` output (use `shared_root`)"
    );
}

fn wait_for_run_terminal_state(cwd: &Path, home: &Path, custom_root: &Path, run_id: &str) -> Value {
    let custom_root_arg = custom_root.to_string_lossy();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let last = run_orbit_json(
            cwd,
            home,
            &[
                "--root",
                custom_root_arg.as_ref(),
                "run",
                "show",
                run_id,
                "--json",
            ],
            None,
        );
        if last["run"]["state"]
            .as_str()
            .is_some_and(|state| state != "pending" && state != "running")
        {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            detached_worker_diagnostic(custom_root, run_id, &last)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn detached_worker_diagnostic(custom_root: &Path, run_id: &str, last: &Value) -> String {
    let worker_log = custom_root
        .join("state/logs")
        .join(format!("{run_id}.worker.log"));
    let worker_log_text = fs::read_to_string(&worker_log)
        .unwrap_or_else(|error| format!("<missing worker log {}: {error}>", worker_log.display()));
    let config = fs::read_to_string(custom_root.join("config.toml")).unwrap_or_default();
    let state = last["run"]["state"].as_str().unwrap_or("missing");
    let pid = last["run"]["pid"].clone();
    let started_at = last["run"]["started_at"].clone();
    format!(
        "run {run_id} stayed non-terminal under --root {}; \
         state={state} pid={pid} started_at={started_at}; last show={last}; \
         config.toml:\n{config}\nworker log:\n{worker_log_text}",
        custom_root.display()
    )
}

/// `workspace init` only seeds `[crews]` / `default_crew` when it sees an agent
/// CLI on PATH. The stub planted by [`plant_agent_cli_stub`] covers the common
/// case; write the same contract into the isolated root so a detached worker
/// cannot lose crew resolution if detection did not stick.
fn pin_default_crew_for_isolated_root(custom_root: &Path) {
    let config_path = custom_root.join("config.toml");
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    if existing.contains("default_crew") && existing.contains("[crews.") {
        return;
    }
    fs::write(
        &config_path,
        r#"[workflow]
default_crew = "codex"

[crews.codex]
provider = "codex"
model = "gpt-5.4"
backend = "cli"
"#,
    )
    .expect("pin isolated-root default crew");
}

fn string_field<'a>(value: &'a Value, field: &str) -> &'a str {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected string field `{field}` in {value}"))
}

fn run_orbit_success(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) {
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(cwd)
        .env(
            "PATH",
            stub_first_path(&plant_agent_cli_stub(home, "codex")),
        )
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    clear_inherited_authority_env(&mut command);
    set_orbit_root_env(&mut command, orbit_root);
    command.assert().success();
}

fn run_orbit_json(cwd: &Path, home: &Path, args: &[&str], orbit_root: Option<&Path>) -> Value {
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(cwd)
        .env(
            "PATH",
            stub_first_path(&plant_agent_cli_stub(home, "codex")),
        )
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    clear_inherited_authority_env(&mut command);
    set_orbit_root_env(&mut command, orbit_root);
    let assert = command.assert().success();
    serde_json::from_slice(&assert.get_output().stdout).expect("orbit json output")
}

/// Prevent ambient managed-run authority from changing child command
/// behavior — or, worse, routing its writes (ORB-11300). Both runners below
/// apply this before `set_orbit_root_env`, so an explicit fixture root still
/// wins.
fn clear_inherited_authority_env(command: &mut AssertCommand) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    // Not Orbit authority: a coverage build hands every child the same profile
    // path, which the child would then clobber.
    command.env_remove("LLVM_PROFILE_FILE");
}

fn set_orbit_root_env(command: &mut AssertCommand, orbit_root: Option<&Path>) {
    match orbit_root {
        Some(path) => {
            command.env("ORBIT_ROOT", path);
        }
        None => {
            command.env_remove("ORBIT_ROOT");
        }
    }
}

/// Write an executable no-op named `name` into `<home>/stub-bin`, standing in
/// for an agent CLI during detection, and return that directory.
///
/// `orbit workspace init` freezes crew seeding to the agent CLIs it finds on
/// `PATH`, so a host with none installed — every CI runner — seeds an empty
/// `[crews]` table and leaves `[workflow].default_crew` unset. Any run that has
/// to resolve a crew then dies with `no crew selected`. Planting on every
/// invocation keeps the stub in place before `init` runs, whatever order the
/// tests call the helpers in; nothing here dispatches an agent, so the stub only
/// has to exist and be executable.
fn plant_agent_cli_stub(home: &Path, name: &str) -> PathBuf {
    let bin = home.join("stub-bin");
    fs::create_dir_all(&bin).expect("create stub CLI directory");
    let stub = bin.join(name);
    fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub agent CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
            .expect("mark the stub agent CLI executable");
    }
    bin
}

/// `PATH` with the fixture's stub directory first, so agent detection sees the
/// stubs regardless of what the host has installed.
fn stub_first_path(bin: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin.to_path_buf()];
    entries.extend(std::env::split_paths(&inherited));
    std::env::join_paths(entries).expect("join PATH entries")
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

fn init_git_repo(main_repo: &Path) {
    run_git(main_repo, &["init"]);
    run_git(main_repo, &["config", "user.name", "Orbit Test"]);
    run_git(
        main_repo,
        &["config", "user.email", "orbit-test@example.com"],
    );
    run_git(main_repo, &["config", "commit.gpgsign", "false"]);
    fs::write(main_repo.join("README.md"), "# orbit\n").expect("write readme");
    run_git(main_repo, &["add", "README.md"]);
    run_git(main_repo, &["commit", "-m", "initial"]);
}

/// [ORB-10981] `orbit --root <data-dir> executor list` from a cwd that is not
/// the intended git checkout must not mint a checkout for `parent(data-dir)`.
#[test]
fn executor_list_with_explicit_root_does_not_bind_parent_of_data_dir() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let data_dir = temp.path().join("qa-root");
    let repo = temp.path().join("qa-ws");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);

    let data_dir_arg = data_dir.to_string_lossy().into_owned();
    run_orbit_success(
        &home,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "qa",
            "--task-prefix",
            "QAZ",
            "--root",
            &data_dir_arg,
        ],
        None,
    );
    run_orbit_success(
        &home,
        &home,
        &[
            "--root",
            &data_dir_arg,
            "executor",
            "list",
            "--format",
            "json",
        ],
        None,
    );

    let parent = canonicalize_or_original(data_dir.parent().expect("data dir parent"));
    for (workspace_id, repo_root, orbit_dir) in checkout_bindings(&data_dir) {
        assert_ne!(
            canonicalize_or_original(Path::new(&repo_root)),
            parent,
            "executor list bound parent(data-dir) as {workspace_id} repo_root={repo_root} orbit_dir={orbit_dir}"
        );
    }

    run_orbit_success(
        &repo,
        &home,
        &[
            "--root",
            &data_dir_arg,
            "workspace",
            "init",
            "--name",
            "qa",
            "--force",
            "--format",
            "json",
        ],
        None,
    );
    assert_workspace_commands_use_named_id(&repo, &home, &data_dir, "ws_qa");
}

/// [ORB-10981] Clean path: init + workspace init from the git checkout, no
/// extra command from another cwd first.
#[test]
fn clean_root_workspace_init_then_auto_task_list_succeeds() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let data_dir = temp.path().join("qa-root");
    let repo = temp.path().join("qa-ws");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);

    let data_dir_arg = data_dir.to_string_lossy().into_owned();
    run_orbit_success(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "qa",
            "--task-prefix",
            "QAZ",
            "--root",
            &data_dir_arg,
        ],
        None,
    );
    run_orbit_success(
        &repo,
        &home,
        &[
            "--root",
            &data_dir_arg,
            "workspace",
            "init",
            "--name",
            "qa",
            "--format",
            "json",
        ],
        None,
    );
    assert_workspace_commands_use_named_id(&repo, &home, &data_dir, "ws_qa");
}

/// [ORB-10981] `--force` must rebind a leftover synthetic sqlite checkout so
/// later commands are not split-brain against `workspaces.json`.
#[test]
fn workspace_init_force_rebinds_synthetic_data_dir_checkout() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let data_dir = temp.path().join("qa-root");
    let repo = temp.path().join("qa-ws");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);

    let data_dir_arg = data_dir.to_string_lossy().into_owned();
    run_orbit_success(
        &home,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "qa",
            "--task-prefix",
            "QAZ",
            "--root",
            &data_dir_arg,
        ],
        None,
    );
    run_orbit_success(
        &home,
        &home,
        &[
            "--root",
            &data_dir_arg,
            "executor",
            "list",
            "--format",
            "json",
        ],
        None,
    );

    seed_synthetic_data_dir_checkout(&data_dir);
    run_orbit_success(
        &repo,
        &home,
        &[
            "--root",
            &data_dir_arg,
            "workspace",
            "init",
            "--name",
            "qa",
            "--force",
            "--format",
            "json",
        ],
        None,
    );

    let repo_canon = canonicalize_or_original(&repo);
    let data_canon = canonicalize_or_original(&data_dir);
    let bindings = checkout_bindings(&data_dir);
    assert!(
        bindings.iter().any(|(workspace_id, repo_root, orbit_dir)| {
            workspace_id == "ws_qa"
                && canonicalize_or_original(Path::new(repo_root)) == repo_canon
                && canonicalize_or_original(Path::new(orbit_dir)) == data_canon
        }),
        "expected ws_qa checkout for the git repo; got {bindings:?}"
    );
    assert!(
        bindings
            .iter()
            .all(|(workspace_id, _, orbit_dir)| workspace_id != "tmp-5b7149"
                || canonicalize_or_original(Path::new(orbit_dir)) != data_canon),
        "synthetic tmp-* row must not keep the data dir: {bindings:?}"
    );
    assert_workspace_commands_use_named_id(&repo, &home, &data_dir, "ws_qa");
}

fn assert_workspace_commands_use_named_id(repo: &Path, home: &Path, data_dir: &Path, id: &str) {
    let data_dir_arg = data_dir.to_string_lossy().into_owned();
    let listed = run_orbit_json(
        repo,
        home,
        &[
            "--root",
            &data_dir_arg,
            "workspace",
            "list",
            "--format",
            "json",
        ],
        None,
    );
    let workspaces = listed
        .as_array()
        .unwrap_or_else(|| panic!("workspace list array: {listed}"));
    assert!(
        workspaces
            .iter()
            .any(|workspace| workspace["id"].as_str() == Some(id)),
        "workspace list missing {id}: {listed}"
    );
    run_orbit_json(
        repo,
        home,
        &[
            "--root",
            &data_dir_arg,
            "auto-task",
            "list",
            "--format",
            "json",
        ],
        None,
    );
}

fn checkout_bindings(data_dir: &Path) -> Vec<(String, String, String)> {
    let db = data_dir.join("tasks").join("index.sqlite");
    if !db.is_file() {
        return Vec::new();
    }
    let conn = rusqlite::Connection::open(&db).expect("open task registry");
    let mut stmt = conn
        .prepare("SELECT workspace_id, repo_root, orbit_dir FROM workspace_checkout_bindings")
        .expect("prepare checkout query");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("query checkout bindings");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("collect checkout bindings")
}

fn seed_synthetic_data_dir_checkout(data_dir: &Path) {
    let db = data_dir.join("tasks").join("index.sqlite");
    let now = "2026-08-22T00:00:00+00:00";
    let parent = canonicalize_or_original(data_dir.parent().expect("data dir parent"));
    let data_canon = canonicalize_or_original(data_dir);
    let conn = rusqlite::Connection::open(&db).expect("open task registry");
    conn.execute(
        "INSERT INTO workspace_bindings (
            workspace_id, slug, repo_fingerprint, created_at, updated_at
        ) VALUES (?1, ?2, NULL, ?3, ?3)",
        rusqlite::params!["tmp-5b7149", "tmp", now],
    )
    .expect("insert synthetic workspace");
    conn.execute(
        "INSERT INTO workspace_checkout_bindings (
            workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        rusqlite::params![
            "tmp-5b7149",
            parent.to_string_lossy().as_ref(),
            parent.to_string_lossy().as_ref(),
            data_canon.to_string_lossy().as_ref(),
            now,
        ],
    )
    .expect("insert synthetic checkout");
    fs::write(
        data_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: tmp-5b7149\n",
    )
    .expect("write synthetic identity");
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
