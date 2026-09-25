//! Sibling tests for the `RuntimeHost` trait surface: worktree admission and
//! the roots a spawned agent is told about.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::task_automation::{approve_for_execution, test_runtime};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::application::workflow::ShipMode;
use crate::runtime::WorkspaceRuntimeBinding;

fn init_git_repo(repo: &Path) {
    git(repo, &["init"]);
    git(repo, &["checkout", "-b", "main"]);
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(repo.join("README.md"), "test repo\n").expect("write README");
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-m", "initial commit"]);
}

fn git(current_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_worktree_setup(runtime: &OrbitRuntime, task_ids: &[String], run_id: &str) -> Value {
    try_worktree_setup(runtime, task_ids, run_id).expect("run worktree setup")
}

fn try_worktree_setup(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    run_id: &str,
) -> Result<Value, orbit_common::OrbitError> {
    orbit_engine::execute_deterministic_action(
        runtime,
        "worktree_setup",
        &Value::Null,
        &json!({
            "run_id": run_id,
            "task_ids": task_ids,
            "base": "main",
            "base_sync": "local",
            "branch_prefix": "orbit-test"
        }),
        false,
        &HashMap::new(),
        None,
    )
}

/// ORB-10602: worktree setup no longer materializes sandbox mount anchors.
///
/// It only ever saw a snapshot of the task's context files and a policy profile
/// that was neither absolutized against the worktree nor augmented with the
/// host's run roots, so the grant set it computed could not match the one the
/// kernel would enforce. Anchors are now derived from the effective profile at
/// each spawn (`orbit_exec::prepare_linux_bwrap_write_grants`); setup must
/// leave the checkout's `.orbit` exactly as `git worktree add` produced it, and
/// must never touch the registered primary checkout.
#[cfg(target_os = "linux")]
#[test]
fn worktree_setup_materializes_no_orbit_targets_from_context_files() {
    let (root, runtime) = test_runtime();
    let repo = root.path().join("repo");
    let activities = root.path().join("global/resources/activities");
    fs::create_dir_all(&activities).expect("create global activities");
    fs::write(
        activities.join("agent_implement.yaml"),
        include_str!("../../../../../assets/activities/agent_implement.yaml"),
    )
    .expect("seed agent implementation activity");
    init_git_repo(&repo);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Create versioned Orbit config".to_string(),
            description: "Exercise trusted missing-target preparation.".to_string(),
            plan: "Prepare the exact scoped targets and validate the sandbox.".to_string(),
            ..Default::default()
        })
        .expect("add task");
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                context_files: Some(vec![
                    "file:.orbit/config.toml".to_string(),
                    "dir:.orbit/routines".to_string(),
                    "file:.orbit/state/forbidden.json".to_string(),
                    "dir:.orbit/future-store".to_string(),
                    "file:.env".to_string(),
                    "file:../outside".to_string(),
                ]),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("persist explicit future selectors");
    approve_for_execution(&runtime, &task);

    let output = run_worktree_setup(&runtime, std::slice::from_ref(&task.id), "jrun-config");
    let worktree = Path::new(output["workspace_path"].as_str().expect("workspace path"));

    for target in [
        ".orbit/config.toml",
        ".orbit/routines",
        ".orbit/state/forbidden.json",
        ".orbit/future-store",
        ".env",
    ] {
        assert!(
            !worktree.join(target).exists(),
            "setup must not materialize any context-file target: {target}"
        );
    }
    assert!(
        !repo.join(".orbit/config.toml").exists(),
        "setup must never write into the registered primary checkout"
    );
}

/// [ORB-11305] Workflow admission accepts `backlog` (fresh authorized work)
/// and `in-progress` (idempotent retry) and nothing else.
///
/// The statuses it refuses are all somebody's decision that the task should not
/// be running. A bundle containing one refuses as a whole, before the worktree
/// exists, so nothing is half-started.
#[test]
fn worktree_setup_admits_backlog_and_refuses_withdrawn_statuses() {
    let (root, runtime) = test_runtime();
    let repo = root.path().join("repo");
    init_git_repo(&repo);

    let backlog = approve_for_execution(
        &runtime,
        &runtime
            .add_task(TaskAddParams {
                title: "Backlog workflow task".to_string(),
                description: "Starts from backlog without a plan.".to_string(),
                ..Default::default()
            })
            .expect("create backlog candidate"),
    );

    // A plan is still not a prerequisite for a workflow start: backlog alone is
    // the authorization.
    let output = run_worktree_setup(&runtime, std::slice::from_ref(&backlog.id), "jrun-admit");
    assert!(
        !output["workspace_path"]
            .as_str()
            .expect("workspace path output")
            .is_empty()
    );
    let admitted = runtime.get_task(&backlog.id).expect("reload admitted task");
    assert_eq!(admitted.status, TaskStatus::InProgress);
    assert_eq!(admitted.job_run_id.as_deref(), Some("jrun-admit"));

    // Re-running the same setup over an already-admitted task is the retry
    // path and must stay a no-op rather than a second start.
    let admitted_again = runtime
        .admit_task_for_workflow_as_system(&backlog.id, "worktree_setup")
        .expect("idempotent workflow admission");
    assert_eq!(admitted_again.status, TaskStatus::InProgress);

    for (label, task_id) in withdrawn_task_fixtures(&runtime) {
        let error = try_worktree_setup(
            &runtime,
            std::slice::from_ref(&task_id),
            &format!("jrun-{label}"),
        )
        .expect_err(&format!("{label} must not be admitted into a workflow"));
        let message = error.to_string();
        assert!(
            message.contains(&task_id) && message.contains("workflow admission"),
            "{label}: unexpected refusal message: {message}"
        );

        let after = runtime.get_task(&task_id).expect("reload refused task");
        assert_ne!(
            after.status,
            TaskStatus::InProgress,
            "{label} must keep its pre-run status"
        );
        assert_eq!(
            after.job_run_id, None,
            "{label} must not be coupled to the refused run"
        );
    }
}

/// Every status a human parks work in, each built through the same public
/// transition a human would use.
fn withdrawn_task_fixtures(runtime: &OrbitRuntime) -> Vec<(&'static str, String)> {
    let create = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: "Exercises a status workflow admission must refuse.".to_string(),
                ..Default::default()
            })
            .expect("create admission fixture")
    };

    let proposed = create("Proposed workflow task");
    let rejected = create("Rejected workflow task");
    runtime
        .reject_task(
            &rejected.id,
            "exercise workflow admission".to_string(),
            None,
        )
        .expect("reject task");
    let archived = create("Archived workflow task");
    runtime.archive_task(&archived.id).expect("archive task");
    let someday = approve_for_execution(runtime, &create("Someday workflow task"));
    runtime
        .update_task(
            &someday.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Someday),
                ..Default::default()
            },
        )
        .expect("park task in someday");

    vec![
        ("proposed", proposed.id),
        ("rejected", rejected.id),
        ("archived", archived.id),
        ("someday", someday.id),
    ]
}

/// [ORB-10980] `ORBIT_ROOT` for a spawned CLI agent must name the authoritative
/// shared registry root. A managed run's workspace `.orbit` and its
/// worktree-local `.orbit` are both mounted read-only and neither owns the task
/// store, so reporting either one strands the documented `orbit tool run`
/// fallback before the agent can read or update its own task.
#[test]
fn orbit_registry_root_reports_the_registry_not_a_workspace_state_root() {
    let root = tempdir().expect("create tempdir");
    let registry_root = root.path().join("registry");
    let repo_root = root.path().join("repo");
    let workspace_state_root = repo_root.join(".orbit");
    let worktree_state_root = workspace_state_root
        .join("state/worktrees/jrun-fixture")
        .join(".orbit");
    for directory in [&registry_root, &workspace_state_root, &worktree_state_root] {
        fs::create_dir_all(directory).expect("state root");
    }

    let runtime = OrbitRuntime::from_resolved_roots(
        &registry_root,
        &workspace_state_root,
        &worktree_state_root,
    )
    .expect("linked-worktree runtime");
    let reported = RuntimeHost::orbit_registry_root(&runtime).expect("registry root");
    assert_eq!(Path::new(&reported), registry_root);
    assert_ne!(
        Path::new(&reported),
        workspace_state_root,
        "the workspace state root is read-only in a managed run and is not the registry"
    );
    assert_ne!(
        Path::new(&reported),
        worktree_state_root,
        "the worktree-local state root is read-only in a managed run and is not the registry"
    );
}

#[test]
fn orbit_workspace_selector_reports_the_logical_catalog_id() {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    fs::create_dir_all(&global).expect("global");
    fs::create_dir_all(&orbit_dir).expect("orbit dir");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "daniel-e9c542".to_string(),
        },
    )
    .expect("checkout identity");

    let runtime = OrbitRuntime::from_roots_with_binding(
        &global,
        &orbit_dir,
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_orbit".to_string(),
            task_partition_id: "daniel-e9c542".to_string(),
            owner_machine_id: None,
            repo_root: repo,
            ship_mode: ShipMode::Local,
            base_branch: None,
        },
    )
    .expect("bound runtime");
    assert_eq!(
        RuntimeHost::orbit_workspace_selector(&runtime).as_deref(),
        Some("ws_orbit")
    );
    assert_ne!(
        RuntimeHost::orbit_workspace_selector(&runtime).as_deref(),
        Some("daniel-e9c542"),
        "nested tool calls must carry the logical catalog ID, not the checkout identity"
    );
}
