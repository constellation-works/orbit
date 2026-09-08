//! Sibling tests for the engine `RuntimeHost` adapter.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use orbit_engine::{
    RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate, V2AuditWriter, V2DispatchInput,
};
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_types::task::{ExternalRef, Task, TaskStatus, push_external_ref_if_missing};
use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::application::task::{
    SYSTEM_ACTOR_LABEL, TaskAddParams, TaskRecordUpdateParams, TaskUpdateParams,
};
use crate::application::workflow::ShipMode;
use crate::runtime::WorkspaceRuntimeBinding;
use crate::{ActorIdentity, OrbitRuntime};

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

fn test_runtime_with_config(config: &str) -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(workspace_root.join("config.toml"), config).expect("write test config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

fn attribution_test_config() -> &'static str {
    r#"
[workflow]
default_crew = "claude-team"

[crews.claude-team]
model = "claude-sonnet-4-6"
provider = "claude"
backend = "cli"

[crews.codex-team]
model = "gpt-5.5"
provider = "codex"
backend = "cli"
"#
}

fn insert_run_with_resolved_crew(runtime: &OrbitRuntime, job_id: &str, input: Value) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, Utc::now(), Some(input.clone()), None)
        .expect("insert job run");
    runtime
        .record_run_crew_from_input(&run.run_id, &input)
        .expect("record run crew");
    run.run_id
}

fn run_update_task_v2_activity(runtime: &OrbitRuntime, run_id: &str, input: Value) -> Value {
    let audit_dir = tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        run_id,
        "test:update_task".to_string(),
        None,
    )
    .expect("audit writer");
    let spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: "update_task".to_string(),
        config: json!({}),
    });

    orbit_engine::dispatch_v2_activity(V2DispatchInput {
        activity_name: "update_task",
        spec: &spec,
        fs_profile: None,
        input,
        audit,
        run_id,
        host: Some(runtime),
    })
    .expect("dispatch update_task activity")
    .output
}

fn approve_for_execution(runtime: &OrbitRuntime, task: &Task) -> Task {
    runtime
        .approve_task(
            &task.id,
            Some("approve for test execution".to_string()),
            None,
        )
        .expect("approve task")
}

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
        include_str!("../../../../assets/activities/agent_implement.yaml"),
    )
    .expect("seed agent implementation activity");
    init_git_repo(&repo);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Create versioned Orbit config".to_string(),
            description: "Exercise trusted missing-target preparation.".to_string(),
            plan: "Prepare the exact scoped targets and validate the sandbox.".to_string(),
            workspace_path: Some(".".to_string()),
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

#[test]
fn apply_task_automation_update_does_not_touch_context_files_when_unset() {
    let (root, runtime) = test_runtime();
    // Pre-create files in the workspace so add_task does not prune the
    // selectors as missing.
    let repo = root.path().join("repo");
    fs::write(repo.join("README.md"), "test\n").expect("write README.md");
    fs::write(repo.join("CLAUDE.md"), "test\n").expect("write CLAUDE.md");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Preserve context_files".to_string(),
            description: "Exercise context_files preservation across automation updates."
                .to_string(),
            workspace_path: Some(".".to_string()),
            context_files: vec!["README.md".to_string(), "CLAUDE.md".to_string()],
            ..Default::default()
        })
        .expect("add task");

    let initial_context_files = runtime
        .get_task(&task.id)
        .expect("reload task")
        .context_files;
    assert!(
        !initial_context_files.is_empty(),
        "test precondition: add_task should keep existing context_files"
    );

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                plan: Some("## Plan\nSome plan body.\n".to_string()),
                context_files: None,
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation update without context_files set");

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(
        updated.context_files, initial_context_files,
        "context_files: None should leave the field untouched"
    );
}

#[test]
fn apply_task_automation_update_replaces_context_files_when_set() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Replace context_files".to_string(),
            description: "Exercise context_files replacement via automation update.".to_string(),
            workspace_path: Some(".".to_string()),
            context_files: vec!["Cargo.toml".to_string()],
            ..Default::default()
        })
        .expect("add task");

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                context_files: Some(vec![
                    "file:src/new.rs".to_string(),
                    "dir:src/new_dir".to_string(),
                ]),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation update with new context_files");

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(
        updated.context_files,
        vec!["file:src/new.rs".to_string(), "dir:src/new_dir".to_string()]
    );
}

#[test]
fn automation_can_restamp_in_progress_task_without_plan() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Restamp task metadata".to_string(),
            description: "Exercise idempotent in-progress automation updates.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);

    assert!(task.plan.is_empty());
    let started = runtime
        .start_task(&task.id, Some("start approved task".to_string()), None)
        .expect("start backlog task without plan");
    assert_eq!(started.status, TaskStatus::InProgress);

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                job_run_id: Some("jrun-test".to_string()),
                status: Some(TaskStatus::InProgress),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("restamp in-progress task metadata");

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(updated.status, TaskStatus::InProgress);
    assert_eq!(updated.job_run_id.as_deref(), Some("jrun-test"));
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
                workspace_path: Some(".".to_string()),
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
                workspace_path: Some(".".to_string()),
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

#[test]
fn direct_update_to_in_progress_is_classification_without_a_plan() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Direct update remains gated".to_string(),
            description: "A direct update is not workflow admission.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create proposed task");

    let updated = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("direct status classification needs no plan");
    assert_eq!(updated.status, TaskStatus::InProgress);
    assert!(updated.plan.is_empty());
}

#[test]
fn direct_start_from_proposed_still_requires_plan() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Direct start remains gated".to_string(),
            description: "A human start is not workflow admission.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create proposed task");

    let err = runtime
        .start_task(&task.id, Some("attempt direct start".to_string()), None)
        .expect_err("direct start should still require a plan");
    assert!(
        err.to_string()
            .contains("requires a non-empty execution plan"),
        "{err}"
    );
}

#[test]
fn generic_automation_update_does_not_unarchive_empty_plan_tasks() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Archived generic automation".to_string(),
            description: "Generic metadata stamping is not workflow admission.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create task");
    runtime.archive_task(&task.id).expect("archive task");

    let err = runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect_err("generic automation update should not admit archived task");
    assert!(
        err.to_string()
            .contains("requires a non-empty execution plan"),
        "{err}"
    );
}

#[test]
fn update_task_automation_records_status_history_as_system() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review automated update".to_string(),
            description: "Exercise update_task automation attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("human starts work".to_string()), None)
        .expect("start task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                execution_summary: Some("Implemented and validated.".to_string()),
                ..Default::default()
            },
        )
        .expect("set execution summary");

    orbit_engine::execute_deterministic_action(
        &runtime,
        "update_task",
        &Value::Null,
        &json!({
            "task_id": task.id.clone(),
            "status": "review"
        }),
        false,
        &HashMap::new(),
        None,
    )
    .expect("run update_task automation");

    let history = runtime.get_task_history(&task.id).expect("reload history");
    let status_entry = history
        .iter()
        .rev()
        .find(|entry| {
            entry.event == "status_changed" && entry.to_status == Some(TaskStatus::Review)
        })
        .expect("review transition history");
    assert_eq!(status_entry.by, SYSTEM_ACTOR_LABEL);
    assert_eq!(
        status_entry.note.as_deref(),
        Some("automation: update_task \u{2192} review")
    );
}

#[test]
fn v2_update_task_activity_uses_resolved_crew_identity() {
    let (_root, runtime) = test_runtime_with_config(attribution_test_config());
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review attributed update".to_string(),
            description: "Exercise update_task activity crew attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            crew: Some("claude-team".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                execution_summary: Some(
                    "Agent persisted intermediate work without changing status.".to_string(),
                ),
                ..Default::default()
            },
        )
        .expect("intermediate task update");
    let run_id = insert_run_with_resolved_crew(
        &runtime,
        "task_pipeline",
        json!({ "task_id": task.id.clone() }),
    );
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                job_run_id: Some(run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("stamp job run");

    run_update_task_v2_activity(
        &runtime,
        &run_id,
        json!({
            "task_id": task.id.clone(),
            "status": "review"
        }),
    );

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(updated.status, TaskStatus::Review);
    assert_eq!(updated.implemented_by.as_deref(), Some("claude"));
}

#[test]
fn v2_update_task_activity_preserves_existing_implemented_by() {
    let (_root, runtime) = test_runtime_with_config(attribution_test_config());
    let task = runtime
        .add_task(TaskAddParams {
            title: "Preserve existing implementer".to_string(),
            description: "Exercise review to done automation attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            crew: Some("codex-team".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some("Agent moved the task to review.".to_string()),
                ..Default::default()
            },
            Some("claude".to_string()),
            Some("claude".to_string()),
        )
        .expect("agent review update");
    let reviewed = runtime.get_task(&task.id).expect("reload reviewed task");
    assert_eq!(reviewed.implemented_by.as_deref(), Some("claude"));

    let run_id = insert_run_with_resolved_crew(
        &runtime,
        "task_pipeline",
        json!({ "task_id": task.id.clone() }),
    );
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                job_run_id: Some(run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("stamp job run");

    run_update_task_v2_activity(
        &runtime,
        &run_id,
        json!({
            "task_id": task.id.clone(),
            "status": "done"
        }),
    );

    let done = runtime.get_task(&task.id).expect("reload done task");
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.implemented_by.as_deref(), Some("claude"));
}

#[test]
fn direct_review_status_is_classification_without_execution_evidence() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review guard".to_string(),
            description: "Exercise review summary requirement.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");

    let updated = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("direct review classification needs no execution summary");
    assert_eq!(updated.status, TaskStatus::Review);
    assert!(updated.execution_summary.is_empty());
    assert_eq!(updated.implemented_by, None);
}

#[test]
fn activity_update_comment_records_comment_as_system() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Activity comment".to_string(),
            description: "Exercise activity comment attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);

    runtime
        .update_task_from_activity(
            &task.id,
            TaskActivityUpdate {
                status: TaskStatus::InProgress,
                expected_status: TaskStatus::Backlog,
                execution_summary: None,
                comment: Some("Automation left a note.".to_string()),
                note: Some("automation start".to_string()),
                agent: None,
                model: None,
            },
        )
        .expect("activity update");

    let comments = runtime
        .get_task_comments(&task.id)
        .expect("reload comments");
    let comment = comments.last().expect("activity comment");
    assert_eq!(comment.by, SYSTEM_ACTOR_LABEL);
    assert_eq!(comment.message, "Automation left a note.");
    // ORB-10311: the comment persists but no bare `commented` history stub is created.
    let history = runtime.get_task_history(&task.id).expect("reload history");
    assert!(
        !history.iter().any(|entry| entry.event == "commented"),
        "a persisted comment must not emit a `commented` history entry"
    );
}

#[test]
fn automation_update_under_agent_runtime_uses_model_identity_for_attribution() {
    let (_root, runtime) = test_runtime();
    let runtime = runtime.with_actor(crate::ActorIdentity::agent("gpt-agent-test"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Agent-driven automation".to_string(),
            description: "Exercise agent runtime attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                plan: Some("Implement the task through workflow automation.".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation plan update");
    let planned = runtime.get_task(&task.id).expect("reload planned task");
    assert_eq!(planned.planned_by.as_deref(), Some("gpt-agent-test"));

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                execution_summary: Some("Implemented through automation.".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation review update");

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(updated.implemented_by.as_deref(), Some("gpt-agent-test"));
}

#[test]
fn automation_update_without_agent_runtime_falls_back_to_system_attribution() {
    let _env =
        orbit_common::test_env::unset(orbit_common::test_env::AGENT_IDENTITY_ENV.iter().copied());
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "System automation".to_string(),
            description: "Exercise non-agent workflow attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                execution_summary: Some("Implemented through system automation.".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation review update");

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(updated.implemented_by.as_deref(), Some(SYSTEM_ACTOR_LABEL));
}

#[test]
fn automation_review_done_transitions_preserve_existing_implemented_by_without_model() {
    let (_root, runtime) = test_runtime();
    let runtime = runtime.with_actor(crate::ActorIdentity::agent("gpt-agent-test"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Preserve implementer".to_string(),
            description: "Exercise existing implementer preservation.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                execution_summary: Some("Existing implementation summary.".to_string()),
                implemented_by: Some(Some(
                    orbit_common::test_fixtures::TEST_CLAUDE_MODEL.to_string(),
                )),
                ..Default::default()
            },
        )
        .expect("seed existing implementation attribution");

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation review update");
    let reviewed = runtime.get_task(&task.id).expect("reload reviewed task");
    assert_eq!(
        reviewed.implemented_by.as_deref(),
        Some(orbit_common::test_fixtures::TEST_CLAUDE_MODEL)
    );

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation done update");
    let done = runtime.get_task(&task.id).expect("reload done task");
    assert_eq!(
        done.implemented_by.as_deref(),
        Some(orbit_common::test_fixtures::TEST_CLAUDE_MODEL)
    );
}

#[test]
fn generic_automation_status_update_uses_system_history_and_preserves_implementer() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Generic automation".to_string(),
            description: "Exercise TaskAutomationUpdate attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("human starts work".to_string()), None)
        .expect("start task");

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                execution_summary: Some("Automated handoff complete.".to_string()),
                agent: Some("codex".to_string()),
                model: Some("gpt-test".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation update");

    let updated = runtime.get_task(&task.id).expect("reload task");
    let history = runtime.get_task_history(&task.id).expect("reload history");
    let status_entry = history
        .iter()
        .rev()
        .find(|entry| {
            entry.event == "status_changed" && entry.to_status == Some(TaskStatus::Review)
        })
        .expect("review transition history");
    assert_eq!(status_entry.by, SYSTEM_ACTOR_LABEL);
    assert_eq!(updated.implemented_by.as_deref(), Some("gpt-test"));
}

#[test]
fn apply_task_automation_update_keeps_external_refs_written_after_locked_read() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Concurrent refs".to_string(),
            description: "Exercise automation external_ref merge under the task lock.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let concurrent_ref = ExternalRef::github_pr("200").expect("concurrent github-pr ref");
    let automation_ref = ExternalRef::github_pr("100").expect("automation github-pr ref");
    runtime.set_after_locked_state_read_hook(Arc::new({
        let runtime = runtime.clone();
        let task_id = task.id.clone();
        let concurrent_ref = concurrent_ref.clone();
        move |snapshot| {
            if snapshot.id != task_id {
                return;
            }
            let mut refs = snapshot.external_refs.clone();
            push_external_ref_if_missing(&mut refs, concurrent_ref.clone());
            runtime
                .stores()
                .task_records()
                .update(
                    &task_id,
                    TaskRecordUpdateParams {
                        actor: "concurrent-writer".to_string(),
                        external_refs: Some(refs),
                        ..Default::default()
                    },
                )
                .expect("write concurrent ref under the re-entrant lock");
        }
    }));

    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                external_refs: vec![automation_ref.clone()],
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation ref write");

    let updated = runtime.get_task(&task.id).expect("reload task");
    assert!(
        updated
            .external_refs
            .iter()
            .any(|reference| reference.has_key(&automation_ref.system, &automation_ref.id)),
        "automation ref must survive: {:?}",
        updated.external_refs
    );
    assert!(
        updated
            .external_refs
            .iter()
            .any(|reference| reference.has_key(&concurrent_ref.system, &concurrent_ref.id)),
        "concurrent ref must survive: {:?}",
        updated.external_refs
    );
}

#[test]
fn apply_task_automation_update_refuses_done_when_status_changes_after_locked_read() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Operator withdrawal".to_string(),
            description: "Exercise automation expected_status compare-and-set.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");
    let task = approve_for_execution(&runtime, &task);
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                execution_summary: Some("Ready for merge.".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("move to review");

    runtime.set_after_locked_state_read_hook(Arc::new({
        let runtime = runtime.clone();
        let task_id = task.id.clone();
        move |snapshot| {
            if snapshot.id != task_id || snapshot.status != TaskStatus::Review {
                return;
            }
            runtime
                .update_task(
                    &task_id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                )
                .expect("operator reclassifies to backlog");
        }
    }));

    let error = runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect_err("stale done write must lose to the operator");
    let message = error.to_string();
    assert!(
        message.contains("status changed"),
        "names the race: {message}"
    );
    assert!(
        message.contains("backlog"),
        "names the new status: {message}"
    );

    let current = runtime.get_task(&task.id).expect("reload task");
    assert_eq!(current.status, TaskStatus::Backlog);
}

#[test]
fn direct_update_task_uses_unknown_attribution_without_an_actor_envelope() {
    let (_root, runtime) = test_runtime();
    let runtime = runtime.with_actor(ActorIdentity::unknown());
    let task = runtime
        .add_task(TaskAddParams {
            title: "Unenveloped comment".to_string(),
            description: "Exercise direct update attribution.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                comment: Some("Visible note.".to_string()),
                ..Default::default()
            },
        )
        .expect("update task");

    let comments = runtime
        .get_task_comments(&task.id)
        .expect("reload comments");
    let comment = comments.last().expect("direct comment");
    assert_eq!(comment.by, "unknown");
    assert_eq!(comment.message, "Visible note.");
    // ORB-10311: the full comment is persisted, but no redundant `commented`
    // history entry is emitted alongside it.
    let history = runtime.get_task_history(&task.id).expect("reload history");
    assert!(
        !history.iter().any(|entry| entry.event == "commented"),
        "a persisted comment must not emit a `commented` history entry"
    );
}

#[test]
fn source_task_id_change_history_records_previous_and_replacement() {
    let (_root, runtime) = test_runtime();
    let source = runtime
        .add_task(TaskAddParams {
            title: "Source of regression".to_string(),
            description: "Origin task referenced by a regression.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add source task");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Regressed task".to_string(),
            description: "Exercise source_task_id change history enrichment.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                source_task_id: Some(Some(source.id.clone())),
                ..Default::default()
            },
        )
        .expect("set source_task_id");
    let set_note = latest_source_task_id_note(&runtime, &task.id);
    assert!(set_note.contains("(none)"), "{set_note}");
    assert!(set_note.contains(&source.id), "{set_note}");

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                source_task_id: Some(None),
                ..Default::default()
            },
        )
        .expect("clear source_task_id");
    let cleared_note = latest_source_task_id_note(&runtime, &task.id);
    assert!(cleared_note.contains(&source.id), "{cleared_note}");
    assert!(cleared_note.contains("(none)"), "{cleared_note}");
}

fn latest_source_task_id_note(runtime: &OrbitRuntime, id: &str) -> String {
    runtime
        .get_task_history(id)
        .expect("reload history")
        .into_iter()
        .rev()
        .find(|entry| {
            entry.event == "updated"
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("source_task_id changed"))
        })
        .and_then(|entry| entry.note)
        .expect("source_task_id change history entry")
}

#[test]
fn direct_update_identity_prefers_model_for_authored_roles() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Human update identity precedence".to_string(),
            description: "Exercise direct update authored-role precedence.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task");

    let planned = runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                plan: Some("Implement and validate the task.".to_string()),
                ..Default::default()
            },
            Some("codex".to_string()),
            Some("gpt-explicit".to_string()),
        )
        .expect("author plan with explicit identity");
    assert_eq!(planned.planned_by.as_deref(), Some("gpt-explicit"));

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
        )
        .expect("approve task");
    runtime
        .start_task(&task.id, Some("start task".to_string()), None)
        .expect("start task");
    let reviewed = runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some("Implemented and validated.".to_string()),
                ..Default::default()
            },
            Some("codex".to_string()),
            Some("gpt-explicit".to_string()),
        )
        .expect("review task with explicit identity");
    assert_eq!(reviewed.implemented_by.as_deref(), Some("gpt-explicit"));
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
            workspace_id: "daniel-e9c542".to_string(),
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

#[test]
fn recovered_rebase_checkpoint_survives_restart_without_completing_the_step() {
    use orbit_types::workflow::{JobRunState, PipelineState};

    let (root, runtime) = test_runtime();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        json!({"task_ids": ["T-recovery"]}),
    );
    state.record_step(
        3,
        JobRunState::Success,
        Some(json!({"head_sha": "before"})),
        None,
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .unwrap();
    let output = json!({
        "run_id": run.run_id,
        "head_sha_before": "before",
        "head_sha": "after",
        "base_sha": "target",
        "remote_sha_before": "remote",
    });
    runtime
        .checkpoint_rebase_recovery(&run.run_id, "sync_base", &output)
        .unwrap();
    drop(runtime);

    let reopened = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    let durable = RuntimeHost::read_run_state(&reopened, &run.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(durable.rebase_recovery_checkpoints["sync_base"], output);
    assert_eq!(durable.step_outputs, state.step_outputs);
    assert_eq!(durable.step_states, state.step_states);
    assert_eq!(durable.next_step_index, 4);
    assert!(durable.failure_activity_checkpoint.is_none());
    assert!(
        reopened
            .checkpoint_rebase_recovery("missing-run", "sync_base", &output)
            .is_err()
    );
}
