#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let created = run_orbit_json(
        &orbit_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Orbit-only task",
            "--description",
            "Must stay in the orbit workspace",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let orbit_task_id = created["id"].as_str().expect("created id").to_string();

    let by_name = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "orbit",
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    assert!(
        task_ids(&by_name).contains(&orbit_task_id),
        "name selector from a foreign cwd must list orbit tasks: {by_name}"
    );

    let post_subcommand = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "task",
            "list",
            "--limit",
            "10",
            "--json",
            "--workspace",
            "orbit",
        ],
    );
    assert_eq!(
        post_subcommand, by_name,
        "a post-subcommand --workspace selector must match the top-level form"
    );

    let by_id = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "ws_orbit",
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    assert!(
        task_ids(&by_id).contains(&orbit_task_id),
        "logical id selector from a foreign cwd must list orbit tasks: {by_id}"
    );

    let by_path = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            orbit_repo.to_str().expect("utf8"),
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    assert!(
        task_ids(&by_path).contains(&orbit_task_id),
        "absolute checkout path must list orbit tasks: {by_path}"
    );

    let other_cwd = run_orbit_json(
        &other_repo,
        &home,
        &["task", "list", "--limit", "10", "--json"],
    );
    assert!(
        !task_ids(&other_cwd).contains(&orbit_task_id),
        "cwd discovery without --workspace must keep binding the other checkout: {other_cwd}"
    );

    let selected_by_post_subcommand = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Selected workspace task",
            "--complexity",
            "low",
            "--workspace",
            "orbit",
            "--json",
        ],
    );
    let selected_task_id = selected_by_post_subcommand["id"]
        .as_str()
        .expect("selected task id")
        .to_string();
    let selected_from_workspace = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "task",
            "list",
            "--limit",
            "10",
            "--json",
            "--workspace",
            "orbit",
        ],
    );
    assert!(
        task_ids(&selected_from_workspace).contains(&selected_task_id),
        "post-subcommand --workspace on task add must select the workspace: {selected_from_workspace}"
    );
}

#[test]
fn global_workspace_flag_fails_closed_on_unknown_selector() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&home).expect("home");
    init_git_repo(&repo);
    run_orbit(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(&repo, &home, &["workspace", "init", "--name", "orbit"]).success();

    let assert = run_orbit(
        &repo,
        &home,
        &[
            "--workspace",
            "no-such-workspace",
            "task",
            "list",
            "--limit",
            "1",
        ],
    )
    .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("no-such-workspace"),
        "unknown selector must be named: {stderr}"
    );
}

#[test]
fn tool_run_workspace_selection_uses_global_flag_and_input_precedence() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let shown = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "orbit",
            "--format",
            "json",
            "workspace",
            "show",
        ],
    );
    assert_eq!(
        Path::new(shown["checkout"]["repo_root"].as_str().expect("repo root")),
        fs::canonicalize(&orbit_repo).expect("canonical orbit repo")
    );
    assert_eq!(
        Path::new(shown["checkout"]["orbit_dir"].as_str().expect("orbit dir")),
        fs::canonicalize(orbit_repo.join(".orbit")).expect("canonical orbit dir")
    );

    let flag_only = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "orbit",
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            r#"{"title":"Selected by global flag","description":"Global selector","complexity":"low","model":"codex"}"#,
        ],
    );
    assert_eq!(flag_only["title"], "Selected by global flag");

    let input_only = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            r#"{"title":"Selected by input","description":"Input selector","workspace":"other","complexity":"low","model":"codex"}"#,
        ],
    );
    assert_eq!(input_only["title"], "Selected by input");

    let both_supplied = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "other",
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            r#"{"title":"Input wins","description":"Explicit input selector","workspace":"orbit","complexity":"low","model":"codex"}"#,
        ],
    );
    assert_eq!(both_supplied["title"], "Input wins");

    let orbit_tasks = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "orbit",
            "tool",
            "run",
            "orbit.task.list",
            "--input",
            r#"{"limit":10,"model":"codex"}"#,
        ],
    );
    let orbit_titles = task_titles(&orbit_tasks);
    assert!(orbit_titles.contains(&"Selected by global flag".to_string()));
    assert!(orbit_titles.contains(&"Input wins".to_string()));
    assert!(!orbit_titles.contains(&"Selected by input".to_string()));

    let other_tasks = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "tool",
            "run",
            "orbit.task.list",
            "--input",
            r#"{"workspace":"other","limit":10,"model":"codex"}"#,
        ],
    );
    let other_titles = task_titles(&other_tasks);
    assert!(other_titles.contains(&"Selected by input".to_string()));
    assert!(!other_titles.contains(&"Selected by global flag".to_string()));
    assert!(!other_titles.contains(&"Input wins".to_string()));
}

/// A workspace whose ID equals another workspace's name must still be reachable
/// from its checkout and by absolute path. The colliding token stays fail-closed
/// only when the operator types it as an id-or-name selector [ORB-11805].
#[test]
fn checkout_path_and_cwd_resolve_an_id_that_collides_with_another_workspace_name() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let alpha_repo = temp.path().join("alpha");
    let shadow_repo = temp.path().join("ws-alpha");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&alpha_repo);
    init_git_repo(&shadow_repo);

    run_orbit(
        &alpha_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &alpha_repo,
        &home,
        &["workspace", "init", "--name", "alpha"],
    )
    .success();
    run_orbit(
        &shadow_repo,
        &home,
        &["workspace", "init", "--name", "ws_alpha"],
    )
    .success();

    let shown = run_orbit_json(
        &alpha_repo,
        &home,
        &["--format", "json", "workspace", "show"],
    );
    assert_eq!(shown["workspace"]["id"], "ws_alpha");
    assert_eq!(shown["workspace"]["name"], "alpha");

    let listed = run_orbit_json(
        &alpha_repo,
        &home,
        &["--format", "json", "task", "list", "--limit", "10"],
    );
    assert!(
        listed.as_array().is_some() || listed.get("tasks").and_then(Value::as_array).is_some(),
        "task list from the id-owning checkout must succeed: {listed}"
    );

    let by_path = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            alpha_repo.to_str().expect("utf8"),
            "--format",
            "json",
            "workspace",
            "show",
        ],
    );
    assert_eq!(by_path["workspace"]["id"], "ws_alpha");
    assert_eq!(by_path["workspace"]["name"], "alpha");

    let path_list = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            alpha_repo.to_str().expect("utf8"),
            "--format",
            "json",
            "task",
            "list",
            "--limit",
            "10",
        ],
    );
    assert!(
        path_list.as_array().is_some()
            || path_list.get("tasks").and_then(Value::as_array).is_some(),
        "absolute checkout path must list the id-owning workspace: {path_list}"
    );

    for args in [
        ["workspace", "remove", "ws_alpha"].as_slice(),
        ["workspace", "role", "ws_alpha", "owner"].as_slice(),
    ] {
        let assert = run_orbit(&alpha_repo, &home, args).failure();
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
        let combined = format!("{stdout}{stderr}");
        assert!(
            combined.contains("ws_alpha"),
            "ambiguous selector must be named for {args:?}: {combined}"
        );
        assert!(
            combined.contains("ambiguous workspace selector"),
            "typed selector {args:?} must fail closed: {combined}"
        );
    }
}

/// A workspace whose ID equals another workspace's name must still allow publication
/// binding, rebinding, showing, and removing when selected from its own checkout or
/// by its unambiguous name [ORB-11879].
#[test]
fn publication_lifecycle_survives_id_collision_when_selected_by_checkout_or_name() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let alpha_repo = temp.path().join("alpha");
    let shadow_repo = temp.path().join("ws-alpha");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&alpha_repo);
    run_git(
        &alpha_repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example/alpha.git",
        ],
    );
    init_git_repo(&shadow_repo);
    run_git(
        &shadow_repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:example/shadow.git",
        ],
    );

    run_orbit(
        &alpha_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &alpha_repo,
        &home,
        &["workspace", "init", "--name", "alpha"],
    )
    .success();
    run_orbit(
        &shadow_repo,
        &home,
        &["workspace", "init", "--name", "ws_alpha"],
    )
    .success();

    // 1. From alpha's checkout (selected by checkout), binding succeeds.
    let bound = run_orbit_json(
        &alpha_repo,
        &home,
        &[
            "workspace",
            "publication",
            "bind",
            "--remote",
            "git@github.com:example/pub.git",
            "--publication-id",
            "pub_alpha",
            "--json",
        ],
    );
    assert_eq!(bound["workspace_id"], "ws_alpha");
    assert_eq!(bound["publication_id"], "pub_alpha");

    // 2. From alpha's checkout, show succeeds.
    let shown = run_orbit_json(
        &alpha_repo,
        &home,
        &["workspace", "publication", "show", "--json"],
    );
    assert_eq!(shown["workspace_id"], "ws_alpha");
    assert_eq!(shown["bound"], true);

    // 3. From alpha's checkout, rebind succeeds.
    let rebound = run_orbit_json(
        &alpha_repo,
        &home,
        &[
            "workspace",
            "publication",
            "rebind",
            "--remote",
            "git@github.com:example/pub2.git",
            "--publication-id",
            "pub_alpha_v2",
            "--json",
        ],
    );
    assert_eq!(rebound["workspace_id"], "ws_alpha");
    assert_eq!(rebound["publication_id"], "pub_alpha_v2");

    // 4. From alpha's checkout, remove succeeds.
    let removed = run_orbit_json(
        &alpha_repo,
        &home,
        &["workspace", "publication", "remove", "--confirm", "--json"],
    );
    assert_eq!(removed["workspace_id"], "ws_alpha");
    assert_eq!(removed["removed"], true);

    // 5. From elsewhere, selected unambiguously by name (--workspace alpha), bind and remove succeed.
    let bound_by_name = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "alpha",
            "workspace",
            "publication",
            "bind",
            "--remote",
            "git@github.com:example/pub.git",
            "--publication-id",
            "pub_alpha",
            "--json",
        ],
    );
    assert_eq!(bound_by_name["workspace_id"], "ws_alpha");

    let removed_by_name = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "alpha",
            "workspace",
            "publication",
            "remove",
            "--confirm",
            "--json",
        ],
    );
    assert_eq!(removed_by_name["workspace_id"], "ws_alpha");
    assert_eq!(removed_by_name["removed"], true);

    // 6. Typing the ambiguous selector ws_alpha genuinely matches two workspaces and fails closed.
    let assert = run_orbit(
        &elsewhere,
        &home,
        &[
            "--workspace",
            "ws_alpha",
            "workspace",
            "publication",
            "show",
        ],
    )
    .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("ambiguous workspace selector"),
        "operator-typed colliding selector must fail closed: {stderr}"
    );
}

/// `task show` is the one verb whose target is a machine-global primary key, so
/// omitting `--workspace` follows the ID instead of the cwd [ORB-10797].
#[test]
fn task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let created = run_orbit_json(
        &orbit_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Globally addressable task",
            "--description",
            "Reachable by ID from anywhere",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = created["id"].as_str().expect("created id").to_string();

    // A foreign checkout, and a directory that is no workspace at all.
    for cwd in [&other_repo, &elsewhere] {
        let shown = run_orbit_json(cwd, &home, &["task", "show", &task_id, "--json"]);
        assert_eq!(
            shown["id"],
            Value::String(task_id.clone()),
            "task show from {} must follow the id: {shown}",
            cwd.display()
        );
        assert_eq!(shown["workspace"]["name"], "orbit");
        assert_eq!(shown["workspace"]["id"], "ws_orbit");
    }

    let human = run_orbit(&elsewhere, &home, &["task", "show", &task_id]).success();
    let stdout = String::from_utf8_lossy(&human.get_output().stdout).into_owned();
    assert!(
        stdout.contains("Workspace: orbit (ws_orbit)"),
        "human output must name the owning workspace: {stdout}"
    );

    // An explicit selector is a filter: the task is not in `other`, so the read
    // fails closed rather than falling back to the owner.
    let missed = run_orbit(
        &elsewhere,
        &home,
        &["--workspace", "other", "task", "show", &task_id],
    )
    .failure();
    let missed_stdout = String::from_utf8_lossy(&missed.get_output().stdout);
    assert!(
        !missed_stdout.contains("Globally addressable task"),
        "a foreign task must not be printed under `--workspace other`: {missed_stdout}"
    );
    let missed_stderr = String::from_utf8_lossy(&missed.get_output().stderr);
    assert!(
        missed_stderr.contains(&task_id),
        "the miss must name the task it looked for: {missed_stderr}"
    );
}

/// `orbit tool run orbit.task.show` is the agent-facing twin of `orbit task show`
/// and must follow the ID from a foreign checkout and from no workspace at all
/// [ORB-10961]. An explicit `workspace` in the tool input stays a filter.
#[test]
fn tool_run_task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let orbit_repo = temp.path().join("orbit");
    let other_repo = temp.path().join("other");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");

    init_git_repo(&orbit_repo);
    init_git_repo(&other_repo);

    run_orbit(
        &orbit_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "SEL",
        ],
    )
    .success();
    run_orbit(
        &orbit_repo,
        &home,
        &["workspace", "init", "--name", "orbit"],
    )
    .success();
    run_orbit(
        &other_repo,
        &home,
        &["workspace", "init", "--name", "other"],
    )
    .success();

    let created = run_orbit_json(
        &orbit_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Globally addressable task",
            "--description",
            "Reachable by ID from anywhere",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = created["id"].as_str().expect("created id").to_string();
    let show_input = format!(r#"{{"id":"{task_id}","model":"codex"}}"#);

    for cwd in [&other_repo, &elsewhere] {
        let shown = run_orbit_json(
            cwd,
            &home,
            &["tool", "run", "orbit.task.show", "--input", &show_input],
        );
        assert_eq!(
            shown["id"],
            Value::String(task_id.clone()),
            "tool run task show from {} must follow the id: {shown}",
            cwd.display()
        );
        assert_eq!(shown["workspace"]["name"], "orbit");
        assert_eq!(shown["workspace"]["id"], "ws_orbit");
    }

    let filtered_input = format!(r#"{{"id":"{task_id}","workspace":"other","model":"codex"}}"#);
    let missed = run_orbit(
        &elsewhere,
        &home,
        &["tool", "run", "orbit.task.show", "--input", &filtered_input],
    )
    .failure();
    let missed_stdout = String::from_utf8_lossy(&missed.get_output().stdout);
    assert!(
        !missed_stdout.contains("Globally addressable task"),
        "a foreign task must not be printed under workspace other: {missed_stdout}"
    );
    let missed_stderr = String::from_utf8_lossy(&missed.get_output().stderr);
    assert!(
        missed_stderr.contains(&task_id),
        "the miss must name the task it looked for: {missed_stderr}"
    );

    let invalid = run_orbit(
        &elsewhere,
        &home,
        &[
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            &format!(r#"{{"id":"{task_id}","workspace":"no-such-workspace","model":"codex"}}"#),
        ],
    )
    .failure();
    let invalid_stderr = String::from_utf8_lossy(&invalid.get_output().stderr);
    assert!(
        invalid_stderr.contains("no-such-workspace"),
        "an invalid explicit selector must be named: {invalid_stderr}"
    );
}

fn task_ids(value: &Value) -> Vec<String> {
    let items = value
        .as_array()
        .cloned()
        .or_else(|| value.get("tasks").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|task| {
            task.get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn task_titles(value: &Value) -> Vec<String> {
    let items = value
        .as_array()
        .cloned()
        .or_else(|| value.get("tasks").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|task| {
            task.get("title")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: this fixture exercises selector resolution, so the inherited
    // `ORBIT_WORKSPACE`/`ORBIT_REGISTRY_ROOT` pair is exactly the input under
    // test. Take the whole shared list rather than a local subset.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    command.assert()
}

fn run_orbit_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let assert = run_orbit(cwd, home, args).success();
    serde_json::from_slice(&assert.get_output().stdout).expect("orbit json output")
}

fn init_git_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create repo");
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# repo\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "initial"]);
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
