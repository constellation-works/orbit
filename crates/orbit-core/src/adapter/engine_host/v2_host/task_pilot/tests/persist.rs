//! Prepare/apply classification of status changes and material drift.

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

const MATERIAL_DETAIL: &str = "task meaning or dependency evidence changed after preparation";

struct Workspace {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
}

fn workspace(config_toml: Option<&str>) -> Workspace {
    let (root, runtime, _) = runtime_with_workspace_config(config_toml);
    let repo = runtime.paths().repo_root.clone();
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("gitignore");
    std::fs::write(repo.join("README.md"), "fixture\n").expect("readme");
    git(&repo, &["add", ".gitignore", "README.md"]);
    git(&repo, &["commit", "-m", "seed"]);
    Workspace {
        _root: root,
        runtime,
        repo,
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.com")
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn add_task(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("{title} description"),
            acceptance_criteria: vec![format!("{title} is done")],
            plan: format!("{title} plan"),
            status: Some(status),
            ..Default::default()
        })
        .expect("add task")
}

fn prepare(workspace: &Workspace, task_id: &str) -> Value {
    let path = workspace.repo.canonicalize().expect("canonicalize repo");
    workspace
        .runtime
        .run_deterministic(
            "prepare_task_pilot",
            &json!({}),
            &json!({
                "workspace_path": path,
                "task_ids": [task_id],
                "base_branch": "main",
            }),
            ToolContext::default(),
        )
        .expect("prepare task pilot")
}

fn apply(workspace: &Workspace, prepared: &Value) -> Result<Value, String> {
    let task = &prepared["tasks"][0];
    let task_id = task["task_id"].as_str().expect("prepared task id");
    let assessment = json!({
        "task_id": task_id,
        "context_files_before": task["context_files_before"],
        "context_files_after": ["file:README.md"],
        "disposition": "selectors",
        "recommended_crew": "opus",
        "recommended_complexity": "low",
        "assessment_rationale": "README.md is the assessed selector.",
        "validation_approach": "Run the prepare and apply actions.",
        "confidence": "high",
        "evidence_gaps": [],
        "reassessment_triggers": [],
        "blocked_by": [],
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
        "duplicate_of": Value::Null,
        "already_landed": Value::Null,
    });
    workspace
        .runtime
        .run_deterministic(
            "apply_task_pilot_results",
            &json!({}),
            &json!({
                "workspace_path": prepared["workspace_path"],
                "prepared": prepared,
                "results": [{
                    "partition_index": 0,
                    "task_ids": [task_id],
                    "tasks": [assessment],
                }],
            }),
            ToolContext::default(),
        )
        .map_err(|error| error.to_string())
}

fn assert_refused(
    workspace: &Workspace,
    before: &Task,
    output: &Value,
    reason: &str,
    detail: &str,
) {
    assert_eq!(output["applied_count"], 0, "{output}");
    assert_eq!(output["status"], "failed", "{output}");
    let outcome = &output["task_outcomes"][0];
    assert_eq!(outcome["outcome"], "stale", "{outcome}");
    assert_eq!(outcome["reason"], reason, "{outcome}");
    assert_eq!(outcome["detail"], detail, "{outcome}");
    let error = output["error"].as_str().expect("apply error");
    assert!(error.contains(reason) && error.contains(detail), "{error}");
    let after = workspace.runtime.get_task(&before.id).expect("task");
    assert_eq!(after.status, before.status);
    assert_eq!(after.title, before.title);
    assert_eq!(after.description, before.description);
    assert_eq!(after.acceptance_criteria, before.acceptance_criteria);
    assert_eq!(after.plan, before.plan);
    assert_eq!(after.context_files, before.context_files);
    assert_eq!(after.complexity, before.complexity);
    assert_eq!(after.crew, before.crew);
    assert_eq!(after.dependencies(), before.dependencies());
    let history = workspace
        .runtime
        .get_task_history(&before.id)
        .expect("history");
    assert!(
        history
            .iter()
            .all(|entry| entry.event != "task_pilot_applied"),
        "apply wrote the task"
    );
}

fn assert_applied(workspace: &Workspace, task_id: &str, output: &Value) {
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    let task = workspace.runtime.get_task(task_id).expect("task");
    assert_eq!(task.context_files, vec!["file:README.md".to_string()]);
    assert_eq!(task.complexity, Some(TaskComplexity::Low));
    let history = workspace
        .runtime
        .get_task_history(task_id)
        .expect("history");
    assert!(
        history
            .iter()
            .any(|entry| entry.event == "task_pilot_applied"),
        "apply did not record the write"
    );
}

#[test]
fn admission_to_in_progress_is_status_changed_and_writes_nothing() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "Admit", TaskStatus::Backlog);
    let prepared = prepare(&workspace, &task.id);
    RuntimeHost::admit_task_for_workflow(&workspace.runtime, &task.id, "worktree_setup")
        .expect("admit");
    let started = workspace.runtime.get_task(&task.id).expect("started task");
    assert_eq!(started.status, TaskStatus::InProgress);
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &started,
        &output,
        "status_changed",
        "task status changed after preparation; task-pilot does not rewrite active work",
    );
}

#[test]
fn meaning_edits_name_the_drifted_component_and_write_nothing() {
    let workspace = workspace(None);
    let cases = [
        ("title", "title"),
        ("description", "description"),
        ("criteria", "criteria"),
        ("plan", "plan"),
        ("selectors", "selectors"),
    ];
    for (label, component) in cases {
        let task = add_task(&workspace.runtime, label, TaskStatus::Backlog);
        let prepared = prepare(&workspace, &task.id);
        let edited = workspace
            .runtime
            .update_task(&task.id, edit_params(label))
            .expect("edit");
        let output = apply(&workspace, &prepared).expect("apply");
        assert_refused(
            &workspace,
            &edited,
            &output,
            "material_changed",
            &format!("{MATERIAL_DETAIL}: {component}"),
        );
    }

    let task = add_task(&workspace.runtime, "both", TaskStatus::Backlog);
    let prepared = prepare(&workspace, &task.id);
    let edited = workspace
        .runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                title: Some("both retitled".to_string()),
                plan: Some("both replanned".to_string()),
                ..Default::default()
            },
        )
        .expect("edit title and plan");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "material_changed",
        &format!("{MATERIAL_DETAIL}: plan, title"),
    );
}

fn edit_params(label: &str) -> TaskUpdateParams {
    match label {
        "title" => TaskUpdateParams {
            title: Some("retitled".to_string()),
            ..Default::default()
        },
        "description" => TaskUpdateParams {
            description: Some("rewritten description".to_string()),
            ..Default::default()
        },
        "criteria" => TaskUpdateParams {
            acceptance_criteria: Some(vec!["a different criterion".to_string()]),
            ..Default::default()
        },
        "plan" => TaskUpdateParams {
            plan: Some("a different plan".to_string()),
            ..Default::default()
        },
        "selectors" => TaskUpdateParams {
            context_files: Some(vec!["file:README.md".to_string()]),
            ..Default::default()
        },
        _ => panic!("unknown edit {label}"),
    }
}

#[test]
fn context_files_outside_freshness_stay_context_files_changed() {
    let workspace = workspace(Some(
        "[workflow.task_pilot_freshness]\nmaterial_fields = [\"title\", \"description\", \"criteria\", \"plan\"]\n",
    ));
    let task = add_task(&workspace.runtime, "selectors", TaskStatus::Backlog);
    let prepared = prepare(&workspace, &task.id);
    let edited = workspace
        .runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                context_files: Some(vec!["file:README.md".to_string()]),
                ..Default::default()
            },
        )
        .expect("edit selectors");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "context_files_changed",
        "task context_files changed after preparation",
    );
}

#[test]
fn task_prepared_in_progress_is_not_rewritten() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "active", TaskStatus::Backlog);
    workspace
        .runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("start");
    let started = workspace.runtime.get_task(&task.id).expect("started");
    let prepared = prepare(&workspace, &task.id);
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &started,
        &output,
        "status_not_mutable",
        "task-pilot does not rewrite in-progress, review, or terminal work",
    );
}

#[test]
fn proposed_to_backlog_still_applies() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "promote", TaskStatus::Proposed);
    let prepared = prepare(&workspace, &task.id);
    workspace
        .runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
        )
        .expect("move to backlog");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_applied(&workspace, &task.id, &output);
    assert_eq!(
        workspace.runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn malformed_component_map_fails_apply_without_writing() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "malformed", TaskStatus::Backlog);
    let before = workspace.runtime.get_task(&task.id).expect("task");
    let mut prepared = prepare(&workspace, &task.id);
    prepared["tasks"][0]["material_components"] = json!("not-an-object");
    let error = apply(&workspace, &prepared).expect_err("malformed components");
    assert!(error.contains("material_components"), "{error}");
    assert_eq!(
        workspace
            .runtime
            .get_task(&task.id)
            .expect("task")
            .complexity,
        before.complexity
    );
    let history = workspace
        .runtime
        .get_task_history(&task.id)
        .expect("history");
    assert!(
        history
            .iter()
            .all(|entry| entry.event != "task_pilot_applied")
    );
}

const CREW_AND_DEPENDENCIES: &str = "\
[workflow.task_pilot_freshness]
material_fields = [\"title\", \"description\", \"criteria\", \"plan\", \"selectors\", \"crew\", \"dependencies\"]
";

#[test]
fn configured_crew_and_dependency_meaning_drift_are_named() {
    let workspace = workspace(Some(CREW_AND_DEPENDENCIES));
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "crew".to_string(),
            description: "crew description".to_string(),
            acceptance_criteria: vec!["crew is resolved".to_string()],
            plan: "crew plan".to_string(),
            status: Some(TaskStatus::Backlog),
            crew: Some("opus".to_string()),
            ..Default::default()
        })
        .expect("add crewed task");
    let prepared = prepare(&workspace, &task.id);
    let edited = workspace
        .runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                crew: Some(Some("luna".to_string())),
                ..Default::default()
            },
        )
        .expect("change crew");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "material_changed",
        &format!("{MATERIAL_DETAIL}: crew"),
    );

    let dependency = add_task(&workspace.runtime, "dependency", TaskStatus::Backlog);
    let main = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "dependent".to_string(),
            description: "dependent description".to_string(),
            acceptance_criteria: vec!["dependency meaning is stable".to_string()],
            plan: "dependent plan".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .expect("add dependent task");
    let prepared = prepare(&workspace, &main.id);
    workspace
        .runtime
        .update_task(
            &dependency.id,
            TaskUpdateParams {
                description: Some("dependency meaning changed".to_string()),
                ..Default::default()
            },
        )
        .expect("edit dependency");
    let before = workspace.runtime.get_task(&main.id).expect("dependent");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &before,
        &output,
        "material_changed",
        &format!("{MATERIAL_DETAIL}: dependencies"),
    );
}

#[test]
fn dependency_status_alone_still_applies_and_is_not_named_with_a_title_edit() {
    let workspace = workspace(Some(CREW_AND_DEPENDENCIES));
    let dependency = add_task(&workspace.runtime, "status dependency", TaskStatus::Backlog);
    let main = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "status main".to_string(),
            description: "status main description".to_string(),
            acceptance_criteria: vec!["dependency status can move".to_string()],
            plan: "status main plan".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .expect("add dependent task");
    let prepared = prepare(&workspace, &main.id);
    workspace
        .runtime
        .update_task(
            &dependency.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("start dependency");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_applied(&workspace, &main.id, &output);

    let dependency = add_task(&workspace.runtime, "mixed dependency", TaskStatus::Backlog);
    let main = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "mixed main".to_string(),
            description: "mixed main description".to_string(),
            acceptance_criteria: vec!["title drift is not a dependency edit".to_string()],
            plan: "mixed main plan".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .expect("add mixed task");
    let prepared = prepare(&workspace, &main.id);
    workspace
        .runtime
        .update_task(
            &dependency.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("finish dependency");
    let edited = workspace
        .runtime
        .update_task(
            &main.id,
            TaskUpdateParams {
                title: Some("mixed main retitled".to_string()),
                ..Default::default()
            },
        )
        .expect("retitle");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "material_changed",
        &format!("{MATERIAL_DETAIL}: title"),
    );
}
