use super::super::dispatch::*;
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::{ShipMode, WorkspaceRuntimeBinding};
use chrono::Utc;
use orbit_engine::DispatchError;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{DeterministicAction, PipelineState};
use serde_json::json;
use tempfile::tempdir;

fn dispatch_declared_action(
    runtime: &OrbitRuntime,
    action: &str,
    input: &serde_json::Value,
) -> Result<serde_json::Value, DispatchError> {
    match DeterministicAction::parse(action) {
        Some(DeterministicAction::Engine(engine_action)) => {
            orbit_engine::execute_deterministic_action(
                runtime,
                engine_action.name(),
                &serde_json::Value::Null,
                input,
                false,
                &std::collections::HashMap::new(),
                None,
            )
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: error.to_string(),
            })
        }
        Some(DeterministicAction::Core(_)) | None => {
            runtime.run_deterministic(action, &json!({}), input, ToolContext::default())
        }
    }
}

fn seed_task(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    dependencies: Vec<String>,
) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            dependencies,
            plan: "Fixture plan.".to_string(),
            workspace_path: Some(".".to_string()),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(status),
            ..Default::default()
        })
        .expect("seed task")
        .id
}

fn seed_pr_handoff_task(runtime: &OrbitRuntime, title: &str, tags: Vec<String>) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            tags,
            plan: "Fixture plan.".to_string(),
            workspace_path: Some(".".to_string()),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed PR handoff task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                execution_summary: Some(
                    "Outcome: success\nChanges:\n- Fixture ready for handoff.".to_string(),
                ),
                implemented_by: Some(Some("codex".to_string())),
                job_run_id: Some(Some("checkpoint-run".to_string())),
                ..Default::default()
            },
        )
        .expect("prepare PR handoff task");
    task.id
}

#[test]
fn checkpointed_pr_handoff_actions_are_dispatched_by_v2_host() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    // These inputs stop at action-specific validation, proving the v2 host
    // forwarded them to the engine instead of rejecting their names.
    for action in ["pr_prepare", "git_rebase"] {
        let err = dispatch_declared_action(&runtime, action, &json!({}))
            .expect_err("incomplete fixture input should fail inside the action");
        match err {
            DispatchError::DeterministicActionFailed {
                action: reported_action,
                ..
            } => assert_eq!(reported_action, action),
            other => panic!("expected registered action failure, got {other}"),
        }
    }

    let no_diff_task = seed_pr_handoff_task(
        &runtime,
        "No-diff promotion",
        vec![NO_DIFF_EXPECTED_TAG.to_string()],
    );
    let no_diff = dispatch_declared_action(
        &runtime,
        "pr_promote",
        &json!({
            "job_run_id": "checkpoint-run",
            "completed_task_ids": [no_diff_task.clone()],
            "workspace_path": ".",
            "no_diff_expected": true,
        }),
    )
    .expect("promote no-diff handoff");
    assert_eq!(no_diff["decision"], json!("performed"));
    assert!(no_diff["pr_number"].is_null());
    let no_diff_task = runtime
        .get_task(&no_diff_task)
        .expect("promoted no-diff task");
    assert_eq!(no_diff_task.status, TaskStatus::Review);
    assert!(no_diff_task.github_pr_number().is_none());

    let diff_task = seed_pr_handoff_task(&runtime, "PR promotion", Vec::new());
    let diff = dispatch_declared_action(
        &runtime,
        "pr_promote",
        &json!({
            "job_run_id": "checkpoint-run",
            "completed_task_ids": [diff_task.clone()],
            "workspace_path": ".",
            "pr_number": "42",
            "pr_url": "https://example.test/pull/42",
        }),
    )
    .expect("promote PR handoff");
    assert_eq!(diff["decision"], json!("performed"));
    assert_eq!(diff["pr_number"], json!("42"));
    let diff_task = runtime.get_task(&diff_task).expect("promoted PR task");
    assert_eq!(diff_task.status, TaskStatus::Review);
    assert_eq!(diff_task.github_pr_number(), Some("42"));
}

/// Every public name comes from the shared typed declaration. This exercises
/// the entire declaration through the host, so a declared-but-unimplemented
/// action cannot silently become an advertised runtime capability.
#[test]
fn every_declared_deterministic_action_reaches_an_implementation() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    for action in DeterministicAction::NAMES {
        let result = dispatch_declared_action(&runtime, action, &json!({}));
        assert!(
            !matches!(
                result,
                Err(DispatchError::DeterministicActionNotRegistered(_))
            ),
            "declared action `{action}` must reach its core or engine implementation"
        );
    }
}

#[test]
fn unknown_deterministic_action_retains_not_registered_error() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let result = runtime.run_deterministic(
        "not_a_deterministic_action",
        &json!({}),
        &json!({}),
        ToolContext::default(),
    );

    assert!(matches!(
        result,
        Err(DispatchError::DeterministicActionNotRegistered(action))
            if action == "not_a_deterministic_action"
    ));
}

/// [ORB-10410] `worktree_gc` ships as a deterministic activity reached
/// through the seeded (deliberately disabled) `worktree_gc` routine, so
/// nothing exercised it until an operator opted in — and by then the v2
/// allowlist no longer carried its name. Invoking the action directly must
/// reach the engine's reaper and return its structured GC envelope.
#[test]
fn worktree_gc_is_dispatchable_directly_through_the_v2_host() {
    let (_root, runtime, repo_root) = super::super::test_support::runtime_with_workspace_layout();
    let git_init = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo_root)
        .output()
        .expect("run git init");
    assert!(
        git_init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&git_init.stderr)
    );

    let output =
        dispatch_declared_action(&runtime, "worktree_gc", &json!({ "older_than_hours": 24 }))
            .expect("worktree_gc must dispatch through the v2 host");

    // This workspace has recorded no job runs, so the reaper considers no
    // worktrees. The assertion is the envelope itself: only the real
    // action produces it, and an unregistered name never gets this far.
    assert_eq!(output["reports"], json!([]));
    assert_eq!(output["bytes_reclaimed"], json!(0));
    assert!(
        output["dry_run"].is_boolean(),
        "GC envelope reports its deletion mode: {output}"
    );
}

#[test]
fn workspace_ship_input_prefers_the_registry_neutral_runtime_binding() {
    let root = tempdir().expect("tempdir");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("global orbit");
    std::fs::create_dir_all(&workspace).expect("workspace orbit");
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\nbase_branch = \"agent-main\"\n",
    )
    .expect("workspace config");

    let runtime = OrbitRuntime::from_roots_with_binding(
        &global,
        &workspace,
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_bound".to_string(),
            workspace_id: "ws_bound".to_string(),
            owner_machine_id: None,
            repo_root: repo,
            ship_mode: ShipMode::Pr,
            base_branch: None,
        },
    )
    .expect("bound runtime");
    let input = runtime
        .run_deterministic(
            "resolve_workspace_ship_input",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .expect("resolve bound ship input without a workspace registry");

    assert_eq!(input, json!({"mode": "pr", "base_branch": "agent-main"}));
}

#[test]
fn promote_agent_main_stub_is_loudly_fenced() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let err = runtime
        .run_deterministic(
            "promote_agent_main",
            &json!({}),
            &json!({
                "source_branch": "agent-main",
                "target_branch": "main",
            }),
            ToolContext::default(),
        )
        .expect_err("retired promotion stub should fail loudly");

    match err {
        DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, "promote_agent_main");
            assert!(message.contains("retired stub"), "{message}");
            assert!(message.contains("git_merge"), "{message}");
            assert!(message.contains("git_push"), "{message}");
        }
        other => panic!("expected registered action failure, got {other}"),
    }
}

#[test]
fn revert_on_red_stub_is_loudly_fenced() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let err = runtime
        .run_deterministic(
            "revert_on_red",
            &json!({}),
            &json!({
                "commit_sha": "abc123",
                "branch": "agent-main",
            }),
            ToolContext::default(),
        )
        .expect_err("retired revert stub should fail loudly");

    match err {
        DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, "revert_on_red");
            assert!(message.contains("retired stub"), "{message}");
            assert!(message.contains("manual incident task"), "{message}");
        }
        other => panic!("expected registered action failure, got {other}"),
    }
}

#[test]
fn reserve_locks_records_unmet_dependencies_in_run_state() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let dependency = seed_task(&runtime, "Dependency", TaskStatus::Backlog, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![dependency.clone()],
    );
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), Some(json!({})), None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({})),
        )
        .expect("write state");

    let output = runtime
        .run_deterministic(
            "reserve_locks",
            &json!({}),
            &json!({
                "run_id": run.run_id,
                "task_ids": [blocked],
            }),
            ToolContext::default(),
        )
        .expect("reserve locks");

    assert_eq!(output["reserved"], json!(false));
    assert_eq!(output["waiting_on_deps"], json!([dependency]));
    let state = runtime
        .read_run_state(&run.run_id)
        .expect("read run state")
        .expect("state exists");
    assert_eq!(state.waiting_on_deps, Some(vec![dependency]));
    assert_eq!(state.waiting_on_locks, None);
}

/// Seeds a run and calls `reserve_locks` for `task_ids`, returning both the
/// action result and the run id so callers can assert on persisted state.
fn reserve_locks_for(
    runtime: &OrbitRuntime,
    task_ids: Vec<String>,
) -> (String, Result<serde_json::Value, DispatchError>) {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_gate_pipeline", 1, Utc::now(), Some(json!({})), None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({})),
        )
        .expect("write state");

    let result = runtime.run_deterministic(
        "reserve_locks",
        &json!({}),
        &json!({
            "run_id": run.run_id,
            "task_ids": task_ids,
        }),
        ToolContext::default(),
    );
    (run.run_id, result)
}

fn expect_reserve_locks_failure(result: Result<serde_json::Value, DispatchError>) -> String {
    match result {
        Err(DispatchError::DeterministicActionFailed { action, message }) => {
            assert_eq!(action, "reserve_locks");
            message
        }
        other => panic!("expected reserve_locks to fail fast, got {other:?}"),
    }
}

#[test]
fn reserve_locks_fails_fast_on_archived_dependency() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let dependency = seed_task(&runtime, "Dependency", TaskStatus::Backlog, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![dependency.clone()],
    );
    runtime
        .archive_task(&dependency)
        .expect("archive dependency");

    let (run_id, result) = reserve_locks_for(&runtime, vec![blocked.clone()]);
    let message = expect_reserve_locks_failure(result);

    assert!(
        message.contains("task.dependencies.unsatisfiable"),
        "message must be distinguishable from gate.starvation: {message}"
    );
    assert!(
        message.contains(&dependency),
        "must name the blocker: {message}"
    );
    assert!(
        message.contains(&blocked),
        "must name the blocked task: {message}"
    );
    assert!(message.contains("archived"), "must explain why: {message}");
    // The dependency IDs land in run state too, so `orbit run show` reports
    // them without re-deriving the task graph.
    let state = runtime
        .read_run_state(&run_id)
        .expect("read run state")
        .expect("state exists");
    assert_eq!(state.waiting_on_deps, Some(vec![dependency]));
}

#[test]
fn reserve_locks_fails_fast_on_rejected_dependency() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let dependency = seed_task(&runtime, "Dependency", TaskStatus::Backlog, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![dependency.clone()],
    );
    runtime
        .reject_task(&dependency, "Decided against.".to_string(), None)
        .expect("reject dependency");

    let (_, result) = reserve_locks_for(&runtime, vec![blocked]);
    let message = expect_reserve_locks_failure(result);

    assert!(
        message.contains("task.dependencies.unsatisfiable"),
        "{message}"
    );
    assert!(message.contains(&dependency), "{message}");
    assert!(message.contains("rejected"), "{message}");
}

#[test]
fn reserve_locks_fails_fast_on_dangling_dependency() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let dependency = seed_task(&runtime, "Dependency", TaskStatus::Backlog, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![dependency.clone()],
    );
    runtime.delete_task(&dependency).expect("delete dependency");

    let (_, result) = reserve_locks_for(&runtime, vec![blocked]);
    let message = expect_reserve_locks_failure(result);

    assert!(
        message.contains("task.dependencies.unsatisfiable"),
        "{message}"
    );
    assert!(message.contains(&dependency), "{message}");
    assert!(message.contains("no such task"), "{message}");
}

#[test]
fn reserve_locks_still_waits_on_a_reachable_dependency() {
    // The legitimate-wait path must be untouched: `reserved: false` with the
    // blocker recorded, and no error — the gate loop keeps polling.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let dependency = seed_task(&runtime, "Dependency", TaskStatus::InProgress, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![dependency.clone()],
    );

    let (run_id, result) = reserve_locks_for(&runtime, vec![blocked]);
    let output = result.expect("reachable dependency must not fail dispatch");

    assert_eq!(output["reserved"], json!(false));
    assert_eq!(output["waiting_on_deps"], json!([dependency.clone()]));
    let state = runtime
        .read_run_state(&run_id)
        .expect("read run state")
        .expect("state exists");
    assert_eq!(state.waiting_on_deps, Some(vec![dependency]));
}

#[test]
fn reserve_locks_reports_only_the_dead_end_when_mixed_with_a_live_wait() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let waiting = seed_task(&runtime, "Waiting", TaskStatus::Backlog, Vec::new());
    let dead_end = seed_task(&runtime, "Dead end", TaskStatus::Backlog, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![waiting.clone(), dead_end.clone()],
    );
    runtime.archive_task(&dead_end).expect("archive dependency");

    let (_, result) = reserve_locks_for(&runtime, vec![blocked]);
    let message = expect_reserve_locks_failure(result);

    assert!(message.contains(&dead_end), "{message}");
    assert!(
        !message.contains(&waiting),
        "a still-reachable dependency must not be reported as unsatisfiable: {message}"
    );
}

#[test]
fn reserve_locks_publishes_empty_waiting_on_deps_when_dependencies_are_met() {
    // The gate pipeline references `steps.reserve.output.waiting_on_deps`
    // unconditionally, so the key must exist on the lock path too.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let ready = seed_task(&runtime, "Ready", TaskStatus::Backlog, Vec::new());

    let (_, result) = reserve_locks_for(&runtime, vec![ready]);
    let output = result.expect("reserve locks");

    assert_eq!(output["waiting_on_deps"], json!([]));
    assert_eq!(output["reserved"], json!(true));
}

/// ORB-11349: admission loads every task, so one unrelated task's lifecycle
/// write used to decide whether this gate ran. A transition is a multi-file
/// publication held under that task's bundle lock; a gate that read through it
/// paired the new event log with the not-yet-republished envelope and failed
/// the whole run as bundle corruption. Hold exactly that critical section on
/// task A and require B's gate to admit correctly anyway.
#[test]
fn reserve_locks_admits_through_an_unrelated_task_mid_transition() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let dependency = seed_task(&runtime, "Dependency", TaskStatus::Done, Vec::new());
    let blocked = seed_task(
        &runtime,
        "Blocked",
        TaskStatus::Backlog,
        vec![dependency.clone()],
    );
    let unrelated = seed_task(&runtime, "Unrelated", TaskStatus::Backlog, Vec::new());

    let transition_open = std::sync::Barrier::new(2);
    let transition_closed = std::sync::atomic::AtomicBool::new(false);

    let output = std::thread::scope(|scope| {
        scope.spawn(|| {
            runtime
                .stores()
                .tasks()
                .with_task_write_lock(&unrelated, &mut || {
                    transition_open.wait();
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    transition_closed.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .expect("hold the unrelated task's write lock");
        });

        transition_open.wait();
        let (_, result) = reserve_locks_for(&runtime, vec![blocked.clone()]);
        result.expect("an unrelated task's transition must not fail this gate")
    });

    assert!(
        transition_closed.load(std::sync::atomic::Ordering::SeqCst),
        "the gate must read the unrelated task as settled, not mid-transition"
    );
    assert_eq!(output["reserved"], json!(true));
    assert_eq!(output["waiting_on_deps"], json!([]));
}

#[test]
fn waiting_locks_from_reserve_output_extracts_unique_conflict_files() {
    let locks = waiting_locks_from_reserve_output(&json!({
        "reserved": false,
        "conflicts": [
            { "file": "file:src/lib.rs", "held_by_id": "ORB-1" },
            { "file": "file:src/lib.rs", "held_by_id": "reservation-1" },
            { "file": "dir:crates/orbit-core/src", "held_by_id": "ORB-2" }
        ],
    }));

    assert_eq!(
        locks,
        vec![
            "dir:crates/orbit-core/src".to_string(),
            "file:src/lib.rs".to_string()
        ]
    );
}
