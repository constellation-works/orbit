//! Duplicate pilot preparation is a successful skip, and state routines wait
//! for active holds to end. Fixtures run in isolated children with real stores.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use orbit_core::application::automation::evaluate_routine;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, Task, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_tools::ToolContext;
use orbit_types::workflow::{JobRunState, PipelineState, RoutineDefinition};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Workspace {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Workspace {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            repo.join(".orbit/config.toml"),
            "[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\nsystem_crew = \"fixture\"\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("git");
            orbit_common::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            let output = command.args(args).current_dir(&repo).output().unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Orbit Test"]);
        git(&["config", "user.email", "orbit-test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "seed"]);
        let activities = global.join("resources/activities");
        std::fs::create_dir_all(&activities).unwrap();
        for name in [
            "prepare_task_pilot",
            "task_pilot",
            "apply_task_pilot_results",
            "pipeline_success_guard",
        ] {
            let file = format!("{name}.yaml");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/activities")
                    .join(&file),
                activities.join(file),
            )
            .unwrap();
        }
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some("fixture-machine".into()));
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            runtime,
            repo,
            jobs,
        }
    }

    fn task(&self, title: &str) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Prepare {title}."),
                acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
                plan: "Inspect README.md.".into(),
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap()
    }

    fn action(&self, action: &str, input: Value) -> Value {
        self.runtime
            .run_deterministic(action, &json!({}), &input, ToolContext::default())
            .unwrap_or_else(|error| panic!("{action}: {error}"))
    }

    fn prepare(&self, task_ids: &[&str]) -> Value {
        self.action(
            "prepare_task_pilot",
            json!({"task_ids": task_ids, "workspace_path": self.repo, "base_branch": "main"}),
        )
    }

    /// A live pilot with a real successful prepare checkpoint; another
    /// invocation can observe this hold without starting a provider.
    fn hold(&self, task_ids: &[&str]) -> String {
        let prepared = self.prepare(task_ids);
        let run = self
            .jobs
            .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        state.record_step(0, JobRunState::Success, Some(prepared), None);
        self.runtime.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }
}

#[test]
fn all_held_pilot_run_succeeds_and_retains_every_holder_skip() {
    if !super::dispatch_admission::isolated(
        "task_pilot::all_held_pilot_run_succeeds_and_retains_every_holder_skip",
    ) {
        return;
    }
    let workspace = Workspace::new();
    // Explicit requests must not lose skips to automatic discovery's sample cap.
    let tasks = (0..21)
        .map(|index| workspace.task(&format!("held {index}")))
        .collect::<Vec<_>>();
    let ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let holder = workspace.hold(&ids);
    let job = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_pilot_pipeline.yaml");
    let run = workspace
        .runtime
        .run_job_v2_from_yaml(
            &job,
            json!({"task_ids": ids, "workspace_path": workspace.repo, "base_branch": "main"}),
        )
        .unwrap();
    assert!(run.success, "all-held selections are skips: {run:?}");
    assert_eq!(
        workspace.runtime.show_job_run(&run.run_id).unwrap().state,
        JobRunState::Success
    );
    let prepared = &run.pipeline["prepare"];
    assert_eq!(prepared["task_count"], 0);
    assert_eq!(prepared["partitions"], json!([]));
    assert_eq!(prepared["excluded_sample_truncated"], false);
    assert_eq!(prepared["excluded_by_reason"]["already_preparing"], 21);
    let expected = ids
        .iter()
        .map(|id| json!({"task_id": id, "reason": "already_preparing", "prepared_by_run_ids": [holder]}))
        .collect::<Vec<_>>();
    assert_eq!(prepared["excluded"], json!(expected));
    assert_eq!(run.pipeline["apply"]["status"], "succeeded");
    assert_eq!(
        run.pipeline["apply"]["discovery"]["excluded"],
        json!(expected)
    );
    for task in tasks {
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    }
}

#[test]
fn mixed_pilot_selection_applies_free_tasks_and_skips_held_tasks() {
    if !super::dispatch_admission::isolated(
        "task_pilot::mixed_pilot_selection_applies_free_tasks_and_skips_held_tasks",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let held = workspace.task("held");
    let free = workspace.task("free");
    let holder = workspace.hold(&[&held.id]);
    let prepared = workspace.prepare(&[&held.id, &free.id]);
    assert_eq!(prepared["task_ids"], json!([free.id]));
    assert_eq!(
        prepared["partitions"],
        json!([{"partition_index": 0, "task_ids": [free.id]}])
    );
    let skips = json!([{"task_id": held.id, "reason": "already_preparing", "prepared_by_run_ids": [holder]}]);
    assert_eq!(prepared["excluded"], skips);
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": 0, "task_ids": [free.id],
                "tasks": [{
                    "task_id": free.id,
                    "context_files_before": [], "context_files_after": ["file:README.md"],
                    "disposition": "selectors", "recommended_crew": "fixture",
                    "recommended_complexity": "low", "confidence": "high",
                    "assessment_rationale": "README.md contains the affected material.",
                    "validation_approach": "Inspect the persisted scope.",
                    "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                    "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                    "duplicate_of": null, "already_landed": null,
                }],
            }],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1);
    assert_eq!(applied["unresolved_count"], 0);
    assert_eq!(applied["discovery"]["excluded"], skips);
    assert_eq!(workspace.runtime.get_task(&held.id).unwrap(), held);
    assert_eq!(
        workspace.runtime.get_task(&free.id).unwrap().context_files,
        ["file:README.md"]
    );
}

fn pilot_routine() -> RoutineDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 1, "name": "fixture-pilot", "enabled": true,
        "target": "job:task_pilot_pipeline",
        "trigger": {"state": {
            "kind": "preparation_eligible", "owner_machine": "fixture-machine", "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
            "retries": 1, "deadline_minutes": 90,
        }},
    }))
    .unwrap()
}

#[test]
fn preparation_routine_withholds_active_pilot_tasks_until_the_hold_ends() {
    if !super::dispatch_admission::isolated(
        "task_pilot::preparation_routine_withholds_active_pilot_tasks_until_the_hold_ends",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("routine candidate");
    let routine = pilot_routine();
    let now = Utc::now();
    let pending = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    assert_eq!(pending.reason, "debouncing");
    assert!(
        pending
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&task.id)
    );
    let holder = workspace.hold(&[&task.id]);
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "plan": "Inspect the changed README.md material.",
                "comment": "An edit during a pilot must not start duplicate work.",
            }),
        )
        .unwrap();
    let suppressed = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(suppressed.reason, "work_withheld");
    assert!(suppressed.batch.is_empty());
    let members = suppressed.state.unwrap().members.unwrap();
    assert_eq!(
        members.withheld[&task.id],
        format!("already_preparing: {holder}")
    );
    assert!(!members.pending.contains_key(&task.id));
    assert!(members.active.is_none());
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let released = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    assert_eq!(released.reason, "debouncing");
    let members = released.state.unwrap().members.unwrap();
    assert!(members.withheld.is_empty());
    assert!(members.pending.contains_key(&task.id));
    let due = evaluate_routine(
        &workspace.runtime,
        &routine,
        true,
        now + Duration::minutes(7),
    )
    .unwrap();
    assert_eq!(due.reason, "would_fire");
    assert_eq!(due.batch.len(), 1);
    assert_eq!(due.batch[0].task_ids, std::slice::from_ref(&task.id));
    assert_eq!(workspace.prepare(&[&task.id])["task_ids"], json!([task.id]));
}

/// A retained pending member can be off the next page when a targeted pilot
/// acquires its hold. Admission must suppress it even without re-observation.
#[test]
fn preparation_routine_admission_withholds_a_held_member_off_the_scan_page() {
    if !super::dispatch_admission::isolated(
        "task_pilot::preparation_routine_admission_withholds_a_held_member_off_the_scan_page",
    ) {
        return;
    }
    let workspace = Workspace::new();
    for index in 0..50 {
        workspace
            .runtime
            .add_task(TaskAddParams {
                title: format!("opted out {index}"),
                tags: vec!["no-diff-expected".into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap();
    }
    let task = workspace.task("newest pending member");
    let routine = pilot_routine();
    let now = Utc::now();
    let observed = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let members = observed.state.unwrap().members.unwrap();
    assert!(members.pending.contains_key(&task.id));
    assert!(
        members.scan_after.is_some(),
        "the next observation must continue off this page"
    );
    let holder = workspace.hold(&[&task.id]);
    let suppressed = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(suppressed.reason, "work_withheld");
    assert!(suppressed.batch.is_empty());
    let members = suppressed.state.unwrap().members.unwrap();
    assert_eq!(
        members.withheld[&task.id],
        format!("already_preparing: {holder}")
    );
    assert!(
        members.pending.contains_key(&task.id),
        "admission preserves the deferred member"
    );
    assert!(members.active.is_none());
}
