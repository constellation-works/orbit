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
fn shared_explicit_root_keeps_task_bundles_isolated_by_selected_workspace() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let root = temp.path().join("shared-root");
    let alpha_repo = temp.path().join("alpha");
    let beta_repo = temp.path().join("beta");
    let elsewhere = temp.path().join("elsewhere");
    for directory in [&home, &elsewhere] {
        fs::create_dir_all(directory).expect("fixture directory");
    }
    init_git_repo(&alpha_repo);
    init_git_repo(&beta_repo);
    let root_arg = root.to_str().expect("utf8 root");

    run_orbit(
        &alpha_repo,
        &home,
        &[
            "--root",
            root_arg,
            "init",
            "--non-interactive",
            "--host-name",
            "shared-root-host",
            "--task-prefix",
            "SHR",
        ],
    )
    .success();
    run_orbit(
        &alpha_repo,
        &home,
        &["--root", root_arg, "workspace", "init", "--name", "alpha"],
    )
    .success();
    run_orbit(
        &beta_repo,
        &home,
        &["--root", root_arg, "workspace", "init", "--name", "beta"],
    )
    .success();

    let alpha = run_orbit_json(
        &alpha_repo,
        &home,
        &[
            "--root",
            root_arg,
            "task",
            "add",
            "--title",
            "Alpha task",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let alpha_id = alpha["id"].as_str().expect("alpha task id");
    let beta_from_cwd = run_orbit_json(
        &beta_repo,
        &home,
        &[
            "--root",
            root_arg,
            "task",
            "add",
            "--title",
            "Beta cwd task",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let beta_cwd_id = beta_from_cwd["id"].as_str().expect("beta cwd task id");
    let beta_from_path = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--root",
            root_arg,
            "--workspace",
            beta_repo.to_str().expect("utf8 beta repo"),
            "task",
            "add",
            "--title",
            "Beta path task",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let beta_path_id = beta_from_path["id"].as_str().expect("beta path task id");

    assert!(
        root.join("tasks/workspaces/ws_alpha")
            .join(alpha_id)
            .join("task.yaml")
            .is_file()
    );
    for task_id in [beta_cwd_id, beta_path_id] {
        assert!(
            root.join("tasks/workspaces/ws_beta")
                .join(task_id)
                .join("task.yaml")
                .is_file(),
            "beta bundle must be owned by the selected beta partition"
        );
        assert!(
            !root
                .join("tasks/workspaces/ws_alpha")
                .join(task_id)
                .exists(),
            "beta bundle must not leak into the alpha partition"
        );
    }

    let alpha_list = run_orbit_json(
        &alpha_repo,
        &home,
        &[
            "--root", root_arg, "task", "list", "--limit", "10", "--json",
        ],
    );
    assert_eq!(task_ids(&alpha_list), vec![alpha_id.to_string()]);

    let beta_list = run_orbit_json(
        &elsewhere,
        &home,
        &[
            "--root",
            root_arg,
            "--workspace",
            beta_repo.to_str().expect("utf8 beta repo"),
            "task",
            "list",
            "--limit",
            "10",
            "--json",
        ],
    );
    let beta_ids = task_ids(&beta_list);
    assert_eq!(beta_ids.len(), 2);
    assert!(beta_ids.contains(&beta_cwd_id.to_string()));
    assert!(beta_ids.contains(&beta_path_id.to_string()));
    assert!(!beta_ids.contains(&alpha_id.to_string()));

    for (repo, task_id, workspace_id, workspace_name) in [
        (&alpha_repo, alpha_id, "ws_alpha", "alpha"),
        (&beta_repo, beta_cwd_id, "ws_beta", "beta"),
        (&beta_repo, beta_path_id, "ws_beta", "beta"),
    ] {
        let shown = run_orbit_json(
            repo,
            &home,
            &["--root", root_arg, "task", "show", task_id, "--json"],
        );
        assert_eq!(shown["workspace"]["id"], workspace_id);
        assert_eq!(shown["workspace"]["name"], workspace_name);
    }

    for (workspace, foreign_task_id) in [(&alpha_repo, beta_path_id), (&beta_repo, alpha_id)] {
        run_orbit(
            &elsewhere,
            &home,
            &[
                "--root",
                root_arg,
                "--workspace",
                workspace.to_str().expect("utf8 workspace path"),
                "task",
                "show",
                foreign_task_id,
                "--json",
            ],
        )
        .failure();
    }
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

fn run_orbit(cwd: &Path, home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = cargo_bin_cmd!("orbit");
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
