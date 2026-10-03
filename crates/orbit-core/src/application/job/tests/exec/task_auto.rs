//! Execute the canonical task-auto graph; only backlog observation and child
//! execution are scripted. Bundle validation and terminal guards are real.
use std::sync::{Arc, Mutex};

use orbit_engine::{DispatchError, JobOutcome, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use serde_json::{Value, json};

use super::{seed_default_catalogs, test_runtime, try_execute_named_job};
use crate::OrbitRuntime;

struct AutoHost<'a> {
    runtime: &'a OrbitRuntime,
    backlog: Value,
    failed_task: Option<&'a str>,
    children: Mutex<Vec<Value>>,
}

impl RuntimeHost for AutoHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        context: ToolContext,
    ) -> Result<Value, DispatchError> {
        match action {
            "list_backlog_tasks" => Ok(self.backlog.clone()),
            "invoke_and_wait" => {
                self.children.lock().expect("children").push(input.clone());
                let task = input["run_input"]["task_ids"][0].as_str().expect("task id");
                let failed = self.failed_task == Some(task);
                Ok(json!({"run_id": format!("fixture-gate-{task}"),
                    "status": if failed { "failed" } else { "succeeded" },
                    "error": failed.then_some("fixture gate refused delivery")}))
            }
            "validate_bundles" | "pipeline_success_guard" => {
                <OrbitRuntime as RuntimeHost>::run_deterministic(
                    self.runtime,
                    action,
                    config,
                    input,
                    context,
                )
            }
            _ => Err(DispatchError::DeterministicActionNotRegistered(
                action.to_string(),
            )),
        }
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

fn execute(
    backlog: Value,
    failed_task: Option<&str>,
) -> (Result<JobOutcome, DispatchError>, Vec<Value>) {
    let (root, runtime, repo, global) = test_runtime();
    seed_default_catalogs(&global);
    let host = AutoHost {
        runtime: &runtime,
        backlog,
        failed_task,
        children: Mutex::new(Vec::new()),
    };
    let run_id = root
        .path()
        .file_name()
        .expect("fixture id")
        .to_string_lossy();
    let outcome = try_execute_named_job(
        &runtime,
        &repo,
        &host,
        "task_auto_pipeline",
        json!({
            "mode": "local", "base_branch": "fixture-base", "base_sync": "local",
            "landing_branch": "fixture-landing", "completion": "done", "allowed_crews": ["sol"],
        }),
        &run_id,
    );
    let children = host.children.into_inner().expect("children");
    (outcome, children)
}

fn backlog() -> Value {
    json!({"tasks": [], "task_ids": ["ORB-1", "ORB-2"], "task_count": 2,
        "bundles": [["ORB-1"], ["ORB-2"]]})
}

#[test]
fn shipped_task_auto_dispatches_each_bundle_with_the_operator_contract() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_task_auto_dispatches_each_bundle_with_the_operator_contract,
    )) {
        return;
    }
    let (outcome, mut children) = execute(backlog(), None);
    assert!(outcome.expect("auto graph").success);
    children.sort_by_key(|input| {
        input["run_input"]["task_ids"][0]
            .as_str()
            .unwrap()
            .to_owned()
    });
    assert_eq!(children.len(), 2);
    for (input, task) in children.iter().zip(["ORB-1", "ORB-2"]) {
        assert_eq!(input["job_name"], "task_gate_pipeline");
        assert_eq!(input["run_input"]["task_ids"], json!([task]));
        assert_eq!(input["run_input"]["mode"], "local");
        assert_eq!(input["run_input"]["completion"], "done");
        assert_eq!(input["run_input"]["allowed_crews"], json!(["sol"]));
        assert_eq!(input["run_input"]["base_branch"], "fixture-base");
        assert_eq!(input["run_input"]["landing_branch"], "fixture-landing");
    }
}

#[test]
fn shipped_task_auto_fails_after_collecting_a_failed_gate_with_other_results() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_task_auto_fails_after_collecting_a_failed_gate_with_other_results,
    )) {
        return;
    }
    let (outcome, children) = execute(backlog(), Some("ORB-2"));
    assert_eq!(
        children.len(),
        2,
        "all admitted children complete before the success guard"
    );
    let error = outcome
        .expect_err("failed gate cannot make a successful parent")
        .to_string();
    assert!(error.contains("fixture gate refused delivery"), "{error}");
}

#[test]
fn shipped_task_auto_empty_backlog_succeeds_without_a_child() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_task_auto_empty_backlog_succeeds_without_a_child,
    )) {
        return;
    }
    let (outcome, children) = execute(
        json!({"tasks": [], "task_ids": [], "task_count": 0, "bundles": []}),
        None,
    );
    assert!(outcome.expect("empty auto graph").success);
    assert!(children.is_empty());
}

#[test]
fn shipped_task_auto_refuses_unknown_bundle_before_dispatching_a_child() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_task_auto_refuses_unknown_bundle_before_dispatching_a_child,
    )) {
        return;
    }
    let (outcome, children) = execute(
        json!({"tasks": [], "task_ids": ["ORB-1"], "task_count": 1, "bundles": [["ORB-unknown"]]}),
        None,
    );
    let error = outcome
        .expect_err("unknown bundle must be refused")
        .to_string();
    assert!(error.contains("unknown task_id ORB-unknown"), "{error}");
    assert!(children.is_empty());
}
