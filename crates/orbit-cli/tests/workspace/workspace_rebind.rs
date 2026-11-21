//! Pending checkout rebind keeps every runtime open closed, including the
//! observational open used by routine status and other read-only callers.
//!
//! The decision is produced by `workspace rebind` itself. A file at the
//! history directory path stops finish while it archives that decision, so
//! the pending record stays in place. Removing the file and repeating the
//! command is the documented recovery.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::test_env;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;
use tempfile::tempdir;

const WORKSPACE: &str = "rebind-observe";
const RECOVERY: &str = "interrupted checkout rebind needs recovery";

#[test]
fn observational_open_refuses_a_pending_checkout_rebind_until_recovery() {
    if crate::isolated_test::run_in_child(
        "workspace_rebind::observational_open_refuses_a_pending_checkout_rebind_until_recovery",
    ) {
        return;
    }

    let temp = tempdir().expect("fixture root");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&home).expect("home");
    crate::git_repo::SeededRepo::new(&repo)
        .readme("# rebind observe\n")
        .build();
    crate::git_repo::run(&repo, &["config", "maintenance.auto", "false"]);
    crate::git_repo::run(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/rebind-observe.git",
        ],
    );

    succeed(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "rebind-observe-host",
            "--task-prefix",
            "RB",
        ],
    );
    succeed(&repo, &home, &["workspace", "init", "--name", WORKSPACE]);
    // The rebind command refuses to start until the host store exists.
    succeed(
        &repo,
        &home,
        &["tool", "run", "orbit.task.list", "--input", "{}"],
    );

    let moved = temp.path().join("moved-repo");
    fs::rename(&repo, &moved).expect("move the checkout");
    let global = home.join(".orbit");
    let pending = global.join("workspace-rebind-pending.json");
    let history = global.join("workspace-rebinds");
    assert!(
        !history.exists(),
        "a fresh host has no rebind history path yet"
    );
    fs::write(&history, "not a directory\n").expect("block rebind history creation");

    let interrupted = run(
        &home,
        &home,
        &[
            "--workspace",
            WORKSPACE,
            "workspace",
            "rebind",
            "--path",
            &moved.to_string_lossy(),
            "--json",
        ],
    );
    assert!(
        !interrupted.status.success(),
        "the blocked finish must leave the rebind pending\n{}",
        text(&interrupted)
    );
    assert!(
        pending.is_file(),
        "the rebind decision must be durable before finish fails\n{}",
        text(&interrupted)
    );

    let pending_message = refusal_message(&global);
    assert!(
        pending_message.contains(RECOVERY),
        "observational refusal must name recovery: {pending_message}"
    );

    let listed = succeed_json(
        &home,
        &home,
        &[
            "--workspace",
            WORKSPACE,
            "routine",
            "list",
            "--format",
            "json",
        ],
    );
    let load_errors = listed["load_errors"]
        .as_array()
        .expect("routine list load_errors");
    assert!(
        load_errors.iter().any(|error| error["message"]
            .as_str()
            .is_some_and(|message| message.contains(&pending_message))),
        "routine list must report the observational refusal: {listed}"
    );
    assert_eq!(
        listed["routines"],
        serde_json::json!([]),
        "a refused observational open must not list that workspace's routines: {listed}"
    );

    let tasks = run(&moved, &home, &["--workspace", WORKSPACE, "task", "list"]);
    assert!(
        !tasks.status.success(),
        "a sibling runtime open must refuse while the rebind is pending\n{}",
        text(&tasks)
    );
    assert!(
        text(&tasks).contains(&pending_message),
        "task list must return the same recovery error\nobservational: {pending_message}\n{}",
        text(&tasks)
    );

    fs::remove_file(&history).expect("remove the history blocker");
    succeed(
        &home,
        &home,
        &[
            "--workspace",
            WORKSPACE,
            "workspace",
            "rebind",
            "--path",
            &moved.to_string_lossy(),
            "--json",
        ],
    );
    assert!(
        !pending.exists(),
        "recovery must retire the pending rebind decision"
    );
    assert!(history.is_dir(), "recovery must keep the rebind decision");

    open_normally(&global);
    let recovered = succeed_json(
        &home,
        &home,
        &[
            "--workspace",
            WORKSPACE,
            "routine",
            "list",
            "--format",
            "json",
        ],
    );
    assert!(
        recovered["load_errors"]
            .as_array()
            .is_some_and(|errors| errors.is_empty()),
        "routine list must open the workspace after recovery: {recovered}"
    );
    assert!(
        recovered["routines"]
            .as_array()
            .is_some_and(|routines| !routines.is_empty()),
        "routine list must see the recovered workspace: {recovered}"
    );
    succeed(&moved, &home, &["--workspace", WORKSPACE, "task", "list"]);
}

#[test]
fn torn_rebind_retry_survives_registry_normalization() {
    if crate::isolated_test::run_in_child(
        "workspace_rebind::torn_rebind_retry_survives_registry_normalization",
    ) {
        return;
    }

    let temp = tempdir().expect("fixture root");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&home).expect("home");
    crate::git_repo::SeededRepo::new(&repo)
        .readme("# rebind normalization\n")
        .build();
    crate::git_repo::run(&repo, &["config", "maintenance.auto", "false"]);
    crate::git_repo::run(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/rebind-observe.git",
        ],
    );
    succeed(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "rebind-observe-host",
            "--task-prefix",
            "RB",
        ],
    );
    succeed(&repo, &home, &["workspace", "init", "--name", WORKSPACE]);
    succeed(
        &repo,
        &home,
        &["tool", "run", "orbit.task.list", "--input", "{}"],
    );

    // Sorted under the old root, but the prefix rewrite to `moved-repo`
    // makes this order unsorted, so registry validation re-sorts it when
    // it saves the rebound checkout.
    let global = home.join(".orbit");
    let registry_path = global.join("workspaces.json");
    let outside = temp.path().join("p-outside");
    let inside = repo.join("aaa");
    let mut registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry before rebind"))
            .expect("parse registry before rebind");
    registry["checkouts"][0]["path_overrides"] =
        serde_json::json!([outside.to_string_lossy(), inside.to_string_lossy(),]);
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("registry JSON"),
    )
    .expect("write registry with path overrides");

    let moved = temp.path().join("moved-repo");
    fs::rename(&repo, &moved).expect("move the checkout");
    let pending = global.join("workspace-rebind-pending.json");
    let history = global.join("workspace-rebinds");
    fs::write(&history, "not a directory\n").expect("block rebind history creation");

    let interrupted = run(
        &home,
        &home,
        &[
            "--workspace",
            WORKSPACE,
            "workspace",
            "rebind",
            "--path",
            &moved.to_string_lossy(),
            "--json",
        ],
    );
    assert!(
        !interrupted.status.success(),
        "the blocked finish must leave the rebind pending\n{}",
        text(&interrupted)
    );
    assert!(
        pending.is_file(),
        "pending decision must survive the torn rebind"
    );

    // Restore the pre-validation form a pending decision recorded before
    // the validated snapshot existed: its `new` checkout still carries
    // the unsorted overrides the registry save canonicalized.
    let mut pending_json: Value =
        serde_json::from_slice(&fs::read(&pending).expect("pending decision"))
            .expect("parse pending decision");
    pending_json["new"]["path_overrides"] = serde_json::json!([
        outside.to_string_lossy(),
        moved.join("aaa").to_string_lossy(),
    ]);
    fs::write(
        &pending,
        serde_json::to_vec_pretty(&pending_json).expect("pending JSON"),
    )
    .expect("write pre-validation pending decision");
    let saved_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("saved registry"))
            .expect("parse saved registry");
    let pending_overrides = pending_json["new"]["path_overrides"]
        .as_array()
        .expect("pending path_overrides")
        .clone();
    let saved_overrides = saved_registry["checkouts"][0]["path_overrides"]
        .as_array()
        .expect("saved path_overrides")
        .clone();
    assert_ne!(
        pending_overrides, saved_overrides,
        "fixture must torn-save a checkout validation normalized"
    );
    let mut pending_sorted = pending_overrides.clone();
    pending_sorted.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(
        pending_sorted, saved_overrides,
        "saved checkout must differ only by validation sorting"
    );

    fs::remove_file(&history).expect("remove the history blocker");
    succeed(
        &home,
        &home,
        &[
            "--workspace",
            WORKSPACE,
            "workspace",
            "rebind",
            "--path",
            &moved.to_string_lossy(),
            "--json",
        ],
    );
    assert!(
        !pending.exists(),
        "recovery must retire the pending rebind decision"
    );
    open_normally(&global);
    succeed(&moved, &home, &["--workspace", WORKSPACE, "task", "list"]);
}

/// The observational and writable factory opens must fail as one error while
/// the decision is pending.
fn refusal_message(global: &Path) -> String {
    let selected = RegisteredRuntimeFactory::resolve_workspace_selector(global, WORKSPACE)
        .expect("resolve the workspace the rebind left registered");
    let observational = expect_refused(
        RegisteredRuntimeFactory::open_registered_checkout_observational(
            global,
            &selected.workspace,
            &selected.checkout,
        ),
        "observational open",
    );
    let registered = expect_refused(
        RegisteredRuntimeFactory::open_registered_checkout(
            global,
            &selected.workspace,
            &selected.checkout,
        ),
        "writable factory open",
    );
    assert_eq!(
        observational, registered,
        "observational open must refuse with the same error as the other factory opens"
    );
    observational
}

fn expect_refused(result: Result<OrbitRuntime, OrbitError>, label: &str) -> String {
    match result {
        Ok(_) => panic!("{label} opened a runtime while a checkout rebind is pending"),
        Err(error) => error.to_string(),
    }
}

fn open_normally(global: &Path) {
    let selected = RegisteredRuntimeFactory::resolve_workspace_selector(global, WORKSPACE)
        .expect("resolve the recovered workspace");
    expect_opened(
        RegisteredRuntimeFactory::open_registered_checkout_observational(
            global,
            &selected.workspace,
            &selected.checkout,
        ),
        "observational open after recovery",
    );
    expect_opened(
        RegisteredRuntimeFactory::open_registered_checkout(
            global,
            &selected.workspace,
            &selected.checkout,
        ),
        "writable factory open after recovery",
    );
}

fn expect_opened(result: Result<OrbitRuntime, OrbitError>, label: &str) {
    if let Err(error) = result {
        panic!("{label}: {error}");
    }
}

fn succeed(cwd: &Path, home: &Path, args: &[&str]) {
    let output = run(cwd, home, args);
    assert!(
        output.status.success(),
        "orbit {} failed\n{}",
        args.join(" "),
        text(&output)
    );
}

fn succeed_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let output = run(cwd, home, args);
    assert!(
        output.status.success(),
        "orbit {} failed\n{}",
        args.join(" "),
        text(&output)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "orbit {} did not print JSON: {error}\n{}",
            args.join(" "),
            text(&output)
        )
    })
}

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_ROOT")
        .args(args)
        .output()
        .expect("spawn orbit")
}

fn text(output: &Output) -> String {
    format!(
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
