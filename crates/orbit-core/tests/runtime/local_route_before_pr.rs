//! `review.before_pr` on a local-only route is held before dispatch [ORB-14168].
//!
//! Readiness, the drain's wave and `orbit doctor`'s review switches share the
//! ship mode automatic delivery uses. Submitting `task_local_pipeline` still
//! fails closed with the same refusal, on a local workspace and on a PR one.

use orbit_core::application::review::review_switches;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    OrbitError, OrbitRuntime, ShipMode, TaskComplexity, TaskStatus, WorkspaceRuntimeBinding,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const REFUSAL_MARK: &str = "no meaning on the local-only delivery route";

fn open(ship_mode: ShipMode, global: &str, workspace: &str) -> (TempDir, OrbitRuntime) {
    let root = TempDir::new().unwrap();
    let global_root = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace_root = repo.join(".orbit");
    std::fs::create_dir_all(&global_root).unwrap();
    std::fs::create_dir_all(&workspace_root).unwrap();
    if !global.is_empty() {
        std::fs::write(global_root.join("config.toml"), global).unwrap();
    }
    if !workspace.is_empty() {
        std::fs::write(workspace_root.join("config.toml"), workspace).unwrap();
    }
    let binding = WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_local_route".to_string(),
        task_partition_id: "ws_local_route".to_string(),
        owner_machine_id: None,
        repo_root: repo,
        ship_mode,
        base_branch: Some("main".to_string()),
    };
    let runtime =
        OrbitRuntime::from_roots_with_binding(&global_root, &workspace_root, binding).unwrap();
    (root, runtime)
}

fn seed(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            complexity: TaskComplexity::Medium,
            task_type: Some(orbit_core::TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed task")
        .id
        .to_string()
}

fn backlog(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("list backlog tasks")
}

fn admitted(output: &Value) -> Vec<String> {
    output["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect()
}

fn excluded<'a>(output: &'a Value, task: &str) -> &'a Value {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
        .unwrap_or_else(|| panic!("{task} was not excluded: {output}"))
}

fn readiness_entry(runtime: &OrbitRuntime, task: &str) -> Value {
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .cloned()
        .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
}

fn running_drain(runtime: &OrbitRuntime) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({})),
            None,
        )
        .expect("drain run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .expect("run state");
    jobs.mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .expect("running");
    run.run_id
}

/// The detail readiness and the drain's last pass show for this layer.
fn conflict(source: &str) -> String {
    format!(
        "review.before_pr ({source}) holds PR creation for a reviewer and has no meaning on the \
         local-only delivery route; ship through the PR route or turn review.before_pr off for \
         local delivery (after-landing review is the delivery-code-review auto-task)"
    )
}

fn assert_held(runtime: &OrbitRuntime, task: &str, source: &str) {
    let detail = conflict(source);
    let discovered = backlog(runtime, json!({}));
    assert!(
        admitted(&discovered).is_empty(),
        "a local route must not admit the task: {discovered}"
    );
    let skipped = excluded(&discovered, task);
    assert_eq!(skipped["reason"], "local_route_before_pr", "{discovered}");
    assert_eq!(skipped["detail"], detail, "{discovered}");

    // An explicit mode still wins: the same task is eligible for a PR delivery.
    let as_pr = backlog(runtime, json!({"mode": "pr"}));
    assert_eq!(admitted(&as_pr), vec![task.to_string()], "{as_pr}");

    let drain = running_drain(runtime);
    let wave = runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");
    let pass = runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .expect("last pass");
    let waiting = pass
        .excluded
        .iter()
        .find(|entry| entry.task_id == task)
        .expect("the pass records the held task");
    assert_eq!(waiting.reason.as_deref(), Some("local_route_before_pr"));
    assert_eq!(waiting.detail.as_deref(), Some(detail.as_str()));

    let entry = readiness_entry(runtime, task);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "local_route_before_pr", "{entry}");
    assert_eq!(entry["detail"], detail, "{entry}");

    let switches = review_switches(runtime, chrono::Utc::now()).expect("review switches");
    assert!(switches.before_pr.local_route_incompatible);
    assert!(!switches.healthy());
    assert!(
        switches
            .before_pr
            .problems
            .iter()
            .any(|problem| problem == &detail),
        "{:?}",
        switches.before_pr.problems
    );
}

fn assert_ready(runtime: &OrbitRuntime, task: &str) {
    let discovered = backlog(runtime, json!({}));
    assert_eq!(
        admitted(&discovered),
        vec![task.to_string()],
        "{discovered}"
    );
    assert!(excluded_absent(&discovered, task), "{discovered}");
    let entry = readiness_entry(runtime, task);
    assert_eq!(entry["eligible"], true, "{entry}");
    assert_eq!(entry["reason"], "ready", "{entry}");
    let switches = review_switches(runtime, chrono::Utc::now()).expect("review switches");
    assert!(!switches.before_pr.local_route_incompatible);
}

fn excluded_absent(output: &Value, task: &str) -> bool {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["id"] != task)
}

fn local_pipeline_error(runtime: &OrbitRuntime, task: &str) -> OrbitError {
    runtime
        .submit_pipeline_run(
            "task_local_pipeline",
            json!({"task_ids": [task]}),
            None,
            Some("test"),
        )
        .expect_err("this fixture does not complete a local delivery")
}

/// A local workspace holds backlog work while effective `review.before_pr` is
/// on, whether that value is inherited from global config or set on the
/// workspace. A workspace `false` overrides a global `true`. A PR workspace
/// stays eligible. `task_local_pipeline` still refuses the combination on
/// both routes.
#[test]
fn local_route_holds_before_pr_and_the_pr_route_stays_eligible() {
    if !isolated(
        "local_route_before_pr::local_route_holds_before_pr_and_the_pr_route_stays_eligible",
    ) {
        return;
    }

    let (_root, local_global) = open(ShipMode::Local, "[review]\nbefore_pr = true\n", "");
    let inherited = seed(&local_global, "inherited");
    assert_eq!(
        local_global
            .operation_policy()
            .review_before_pr
            .source
            .label(),
        "global"
    );
    assert_held(&local_global, &inherited, "global");
    let refused = local_pipeline_error(&local_global, &inherited);
    let OrbitError::InvalidInput(message) = &refused else {
        panic!("local-route admission must refuse before the job loads, got {refused}");
    };
    assert!(message.contains(REFUSAL_MARK), "{message}");
    assert!(message.contains("turn review.before_pr off"), "{message}");

    let (_root, overridden) = open(
        ShipMode::Local,
        "[review]\nbefore_pr = true\n",
        "[review]\nbefore_pr = false\n",
    );
    let cleared = seed(&overridden, "workspace off");
    assert!(
        !overridden.operation_policy().review_before_pr.value,
        "workspace false overrides global true"
    );
    assert_ready(&overridden, &cleared);
    let cleared_submit = local_pipeline_error(&overridden, &cleared);
    assert!(
        !cleared_submit.to_string().contains(REFUSAL_MARK),
        "turning the switch off must leave the local-route guard idle, got {cleared_submit}"
    );

    let (_root, local_workspace) = open(ShipMode::Local, "", "[review]\nbefore_pr = true\n");
    let workspace_set = seed(&local_workspace, "workspace on");
    assert_eq!(
        local_workspace
            .operation_policy()
            .review_before_pr
            .source
            .label(),
        "workspace"
    );
    assert_held(&local_workspace, &workspace_set, "workspace");
    let workspace_refused = local_pipeline_error(&local_workspace, &workspace_set);
    assert!(
        matches!(&workspace_refused, OrbitError::InvalidInput(message) if message.contains(REFUSAL_MARK)),
        "{workspace_refused}"
    );

    let (_root, pr) = open(ShipMode::Pr, "", "[review]\nbefore_pr = true\n");
    let pr_task = seed(&pr, "pr route");
    assert_ready(&pr, &pr_task);
    // The job-name guard does not follow the workspace ship mode: a local
    // delivery submitted from a PR workspace is still refused.
    let pr_refused = local_pipeline_error(&pr, &pr_task);
    assert!(
        matches!(&pr_refused, OrbitError::InvalidInput(message) if message.contains(REFUSAL_MARK)),
        "{pr_refused}"
    );
    let as_local = backlog(&pr, json!({"mode": "local"}));
    assert_eq!(
        excluded(&as_local, &pr_task)["reason"],
        "local_route_before_pr",
        "{as_local}"
    );
}
