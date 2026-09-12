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
fn workspace_remove_deregisters_deleted_checkout_by_name_id_and_path() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let survivor_repo = temp.path().join("survivor");
    let deleted_by_name = temp.path().join("deleted-by-name");
    let deleted_by_id = temp.path().join("deleted-by-id");
    let deleted_by_path = temp.path().join("deleted-by-path");
    fs::create_dir_all(&home).expect("home");

    for repo in [
        &survivor_repo,
        &deleted_by_name,
        &deleted_by_id,
        &deleted_by_path,
    ] {
        init_git_repo(repo);
    }

    run_orbit(
        &survivor_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "remove-host",
            "--task-prefix",
            "REM",
        ],
    )
    .success();
    run_orbit(
        &survivor_repo,
        &home,
        &["workspace", "init", "--name", "survivor"],
    )
    .success();

    for (repo, name) in [
        (&deleted_by_name, "deleted-by-name"),
        (&deleted_by_id, "deleted-by-id"),
        (&deleted_by_path, "deleted-by-path"),
    ] {
        run_orbit(repo, &home, &["workspace", "init", "--name", name]).success();
    }

    fs::remove_dir_all(&deleted_by_name).expect("delete name-selected checkout");
    fs::remove_dir_all(&deleted_by_id).expect("delete id-selected checkout");
    fs::remove_dir_all(&deleted_by_path).expect("delete path-selected checkout");

    run_orbit_as_operator(
        &survivor_repo,
        &home,
        &["workspace", "remove", "deleted-by-name"],
    )
    .success();
    run_orbit_as_operator(
        &survivor_repo,
        &home,
        &["workspace", "remove", "ws_deleted-by-id"],
    )
    .success();
    run_orbit_as_operator(
        &survivor_repo,
        &home,
        &[
            "workspace",
            "remove",
            deleted_by_path.to_str().expect("deleted path is utf8"),
        ],
    )
    .success();

    let registry: Value = serde_json::from_slice(
        &fs::read(home.join(".orbit/workspaces.json")).expect("read workspace registry"),
    )
    .expect("parse workspace registry");
    let workspace_names = registry["workspaces"]
        .as_array()
        .expect("workspace array")
        .iter()
        .filter_map(|workspace| workspace["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(workspace_names, vec!["survivor"]);
}

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
fn global_workspace_flag_on_deleted_checkout_reports_inactive_status_and_recorded_path() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let survivor_repo = temp.path().join("survivor");
    let deleted_repo = temp.path().join("deleted");
    fs::create_dir_all(&home).expect("home");
    init_git_repo(&survivor_repo);
    init_git_repo(&deleted_repo);

    run_orbit(
        &survivor_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "selector-host",
            "--task-prefix",
            "DEL",
        ],
    )
    .success();
    run_orbit(
        &survivor_repo,
        &home,
        &["workspace", "init", "--name", "survivor"],
    )
    .success();
    run_orbit(
        &deleted_repo,
        &home,
        &["workspace", "init", "--name", "deleted-ws"],
    )
    .success();

    let created = run_orbit_json(
        &deleted_repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Task in deleted workspace",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let task_id = created["id"].as_str().expect("task id").to_string();

    let deleted_path_str = deleted_repo.to_str().expect("utf8");

    // Delete checkout directory without teardown
    fs::remove_dir_all(&deleted_repo).expect("delete checkout directory");

    let selectors = [
        ("registered name", "deleted-ws"),
        ("logical id", "ws_deleted-ws"),
        ("checkout path", deleted_path_str),
    ];

    for (label, selector) in selectors {
        // Read-only verbs across task list, task show, workspace show, and doctor
        let commands: &[&[&str]] = &[
            &["--workspace", selector, "task", "list"],
            &["--workspace", selector, "task", "show", &task_id],
            &["--workspace", selector, "workspace", "show"],
            &["--workspace", selector, "doctor"],
        ];

        for cmd in commands {
            let assert = run_orbit(&survivor_repo, &home, cmd).failure();
            let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
            assert!(
                stderr
                    .contains("workspace 'deleted-ws' (ws_deleted-ws) is invalid on this machine"),
                "command {:?} with {label} selector must report name, id, and invalid status: {stderr}",
                cmd
            );
            assert!(
                stderr.contains(deleted_path_str),
                "command {:?} with {label} selector must report recorded checkout path: {stderr}",
                cmd
            );
            assert!(
                !stderr.contains("unknown workspace selector"),
                "command {:?} with {label} selector must not report unknown workspace selector: {stderr}",
                cmd
            );
        }
    }

    // Contrast with an unknown workspace selector
    let unknown_assert = run_orbit(
        &survivor_repo,
        &home,
        &["--workspace", "unknown-workspace", "task", "list"],
    )
    .failure();
    let unknown_stderr = String::from_utf8_lossy(&unknown_assert.get_output().stderr);
    assert!(
        unknown_stderr.contains("unknown workspace selector 'unknown-workspace'"),
        "unknown selector must report unknown workspace selector: {unknown_stderr}"
    );
    assert!(
        !unknown_stderr.contains("is invalid on this machine"),
        "unknown selector must not report invalid workspace: {unknown_stderr}"
    );
}

#[test]
fn migrate_dry_run_honors_selected_checkout_and_confirm_uses_the_same_one() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let alpha_repo = temp.path().join("alpha");
    let beta_repo = temp.path().join("beta");
    fs::create_dir_all(&home).expect("home");
    init_git_repo(&alpha_repo);
    init_git_repo(&beta_repo);

    run_orbit(
        &alpha_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "migration-selector-host",
            "--task-prefix",
            "MIG",
        ],
    )
    .success();
    run_orbit(
        &alpha_repo,
        &home,
        &["workspace", "init", "--name", "alpha"],
    )
    .success();
    run_orbit(&alpha_repo, &home, &["migrate", "--confirm"]).success();
    run_orbit(&beta_repo, &home, &["workspace", "init", "--name", "beta"]).success();
    run_orbit(&beta_repo, &home, &["migrate", "--confirm"]).success();

    let alpha_marker = alpha_repo.join(".orbit/state/layout.version");
    let beta_marker = beta_repo.join(".orbit/state/layout.version");
    fs::write(&alpha_marker, "3\n").expect("make alpha current");
    fs::write(&beta_marker, "1\n").expect("make beta pending");

    let alpha_path = alpha_repo.to_str().expect("alpha path");
    let preview = run_orbit(
        &beta_repo,
        &home,
        &["--workspace", alpha_path, "migrate", "--dry-run", "--json"],
    )
    .success();
    let preview: Value = serde_json::from_slice(&preview.get_output().stdout)
        .expect("selected migration preview JSON");
    assert_eq!(
        preview["orbit_dir"],
        alpha_repo.join(".orbit").to_string_lossy().as_ref()
    );
    assert_eq!(preview["up_to_date"], true);
    assert_eq!(
        fs::read_to_string(&beta_marker).expect("read beta marker after preview"),
        "1\n",
        "selected dry-run must not mutate the other workspace"
    );

    let pending = run_orbit(
        &beta_repo,
        &home,
        &["--workspace", "beta", "migrate", "--dry-run", "--json"],
    )
    .failure();
    let pending_stderr = String::from_utf8_lossy(&pending.get_output().stderr);
    assert!(pending_stderr.contains(&beta_repo.join(".orbit").display().to_string()));
    assert!(pending_stderr.contains("layout v2"), "{pending_stderr}");
    assert!(
        !pending_stderr.contains(&alpha_repo.join(".orbit").display().to_string()),
        "pending preview must not report alpha: {pending_stderr}"
    );
    assert_eq!(
        fs::read_to_string(&beta_marker).expect("read beta marker after pending preview"),
        "1\n",
        "pending dry-run must not advance the selected layout"
    );

    let confirmed = run_orbit(
        &beta_repo,
        &home,
        &["--workspace", "alpha", "migrate", "--confirm", "--json"],
    )
    .success();
    let confirmed: Value = serde_json::from_slice(&confirmed.get_output().stdout)
        .expect("selected migration confirmation JSON");
    assert_eq!(
        confirmed["orbit_dir"],
        alpha_repo.join(".orbit").to_string_lossy().as_ref()
    );
    assert_eq!(
        fs::read_to_string(&alpha_marker).expect("read alpha marker after confirm"),
        "3\n"
    );

    let unknown = run_orbit(
        &beta_repo,
        &home,
        &[
            "--workspace",
            "no-such-workspace",
            "migrate",
            "--dry-run",
            "--json",
        ],
    )
    .failure();
    let unknown_stderr = String::from_utf8_lossy(&unknown.get_output().stderr);
    assert!(
        unknown_stderr.contains("no-such-workspace"),
        "{unknown_stderr}"
    );
    assert_eq!(
        fs::read_to_string(&beta_marker).expect("read beta marker after unknown selector"),
        "1\n"
    );

    let config = alpha_repo.join(".orbit/config.yaml");
    let original_config = fs::read(&config).expect("read alpha workspace config");
    fs::write(&config, "schema_version: 1\nworkspace_id: [\n")
        .expect("corrupt alpha workspace config");
    let malformed = run_orbit(
        &beta_repo,
        &home,
        &["--workspace", "alpha", "migrate", "--dry-run", "--json"],
    )
    .failure();
    let malformed_stderr = String::from_utf8_lossy(&malformed.get_output().stderr);
    assert!(
        malformed_stderr.contains("invalid workspace config"),
        "{malformed_stderr}"
    );
    fs::write(&config, &original_config).expect("restore alpha workspace config");

    fs::remove_file(&config).expect("remove alpha workspace config");
    let missing = run_orbit(
        &beta_repo,
        &home,
        &["--workspace", "alpha", "migrate", "--dry-run", "--json"],
    )
    .failure();
    let missing_stderr = String::from_utf8_lossy(&missing.get_output().stderr);
    assert!(
        missing_stderr.contains("workspace config is missing"),
        "{missing_stderr}"
    );
    fs::write(&config, original_config).expect("restore alpha workspace config after missing");
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
        let assert = if args[1] == "remove" {
            run_orbit_as_operator(&alpha_repo, &home, args).failure()
        } else {
            run_orbit(&alpha_repo, &home, args).failure()
        };
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

fn run_orbit_as_operator(cwd: &Path, home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_OPERATOR", "1")
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
