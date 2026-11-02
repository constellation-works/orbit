//! Failed standalone repilots hold unchanged requirements after the run ends.

use orbit_core::application::task::TaskUpdateParams;

use super::freshness::apply_explicit_pilot_assessment;
use super::races::apply_input;
use super::*;

fn discover(workspace: &Workspace) -> Value {
    workspace.prepare(&[])
}

fn stale_task(workspace: &Workspace, status: TaskStatus) -> Task {
    let task = workspace.task("standalone failed repilot");
    apply_explicit_pilot_assessment(workspace, &task, false);
    if status == TaskStatus::Backlog {
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
    }
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "description": "Changed requirements."
            }),
        )
        .unwrap();
    task
}

fn finish_failed_run(workspace: &Workspace, run_id: &str) {
    workspace
        .jobs
        .finalize_job_run_with_outcome(run_id, JobRunState::Failed, Utc::now(), None)
        .unwrap();
}

fn standalone_task_dependent_failures_hold_proposed_and_backlog_repilots() {
    for status in [TaskStatus::Proposed, TaskStatus::Backlog] {
        for malformed_partition in [false, true] {
            let workspace = Workspace::new();
            let task = stale_task(&workspace, status);
            let run_id = workspace.live_run();
            let prepared = workspace.prepare_in_run(&run_id, &[]);
            assert_eq!(prepared["task_ids"], json!([task.id]));
            let mut input = apply_input(&workspace, &prepared);
            input["run_id"] = json!(run_id);
            if malformed_partition {
                input["results"][0]["tasks"] = json!([]);
            } else {
                // Deterministic refusal tied to this task's assessment.
                input["results"][0]["tasks"][0]["context_files_after"] = json!(["file:missing.rs"]);
            }
            let failed = workspace.action("apply_task_pilot_results", input.clone());
            assert_eq!(failed["status"], "failed", "{failed}");
            assert_eq!(failed["applied_count"], 0, "{failed}");
            workspace.action("apply_task_pilot_results", input);
            finish_failed_run(&workspace, &run_id);
            assert_eq!(
                discover(&workspace)["task_ids"],
                json!([]),
                "an unchanged {status:?} task must stay held after standalone failure"
            );
            let history = workspace.runtime.get_task_history(&task.id).unwrap();
            let holds = history
                .iter()
                .filter(|entry| entry.event == "task_pilot_unresolved")
                .collect::<Vec<_>>();
            assert_eq!(holds.len(), 1, "apply replay must not duplicate the hold");
            assert_eq!(
                serde_json::from_str::<Value>(holds[0].note.as_deref().unwrap()).unwrap()["pilot_run_id"],
                run_id
            );
            assert_eq!(
                workspace.prepare(&[&task.id])["task_ids"],
                json!([task.id]),
                "an explicit operator request can retry unchanged requirements"
            );
            workspace
                .runtime
                .run_tool(
                    "orbit.task.update",
                    json!({
                        "id": task.id, "model": "codex", "plan": "Administrative update."
                    }),
                )
                .unwrap();
            assert_eq!(discover(&workspace)["task_ids"], json!([]));
            let mut edit = json!({"id": task.id, "model": "codex"});
            if malformed_partition {
                edit["acceptance_criteria"] = json!(["Revised criterion."]);
            } else {
                edit["description"] = json!("Requirements changed again.");
            }
            workspace
                .runtime
                .run_tool("orbit.task.update", edit)
                .unwrap();
            let retried = discover(&workspace);
            assert_eq!(retried["task_ids"], json!([task.id]));
            let applied = workspace.action(
                "apply_task_pilot_results",
                apply_input(&workspace, &retried),
            );
            assert_eq!(applied["applied_count"], 1, "{applied}");
            assert_eq!(discover(&workspace)["task_ids"], json!([]));
        }
    }
}

fn standalone_partition_failure_holds_only_task_dependent_repilots() {
    for (code, message, held) in [
        (
            "provider_unavailable",
            "Provider unavailable during inspection.",
            false,
        ),
        (
            "executor_failed",
            "inspection_crew_unavailable: no inspection crew",
            false,
        ),
        (
            "invalid_response",
            "Task-dependent malformed pilot output.",
            true,
        ),
    ] {
        let workspace = Workspace::new();
        let task = stale_task(&workspace, TaskStatus::Backlog);
        let run_id = workspace.live_run();
        let prepared = workspace.prepare_in_run(&run_id, &[]);
        assert_eq!(prepared["task_ids"], json!([task.id]));
        workspace
            .runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_step_for_workspace(
                &workspace.runtime.workspace_id().unwrap(),
                &run_id,
                &orbit_types::workflow::JobRunStep {
                    step_index: 1,
                    target_type: orbit_types::workflow::JobTargetType::Activity,
                    target_id: "task_pilot".into(),
                    state: JobRunState::Failed,
                    error_code: Some(code.into()),
                    error_message: Some(message.into()),
                    lock_wait_ms: None,
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    exit_code: Some(1),
                    agent_response_json: None,
                },
            )
            .unwrap();
        let failed = workspace.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": workspace.repo, "prepared": prepared,
                "run_id": run_id, "results": [null],
            }),
        );
        assert_eq!(failed["status"], "failed", "{failed}");
        finish_failed_run(&workspace, &run_id);
        assert_eq!(
            discover(&workspace)["task_ids"],
            if held { json!([]) } else { json!([task.id]) },
            "{code}"
        );
        assert_eq!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "task_pilot_unresolved"),
            held,
            "{code}"
        );
    }
}

crate::scenario_table!(
    standalone_failed_repilot_scenarios,
    standalone_task_dependent_failures_hold_proposed_and_backlog_repilots,
    standalone_partition_failure_holds_only_task_dependent_repilots
);
