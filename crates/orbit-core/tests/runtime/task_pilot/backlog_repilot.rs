//! The bundled routine reassesses edited backlog requirements without an
//! operator request, while preserving execution and failure holds.

use super::freshness;
use super::source_moves::{apply_claim, prepare_claim};
use super::*;
use orbit_core::application::task::TaskUpdateParams;

fn bundled_routine() -> RoutineDefinition {
    let template = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/routines/task_pilot.yaml"),
    )
    .unwrap()
    .replace("__ORBIT_ROUTINE_NAME__", "fixture-pilot")
    .replace("__ORBIT_OWNER_MACHINE__", "fixture-machine")
    .replace("__ORBIT_BASE_BRANCH__", "main");
    let mut routine: RoutineDefinition = serde_yaml::from_str(&template).unwrap();
    routine.enabled = true;
    routine
}

fn assessed_backlog(workspace: &Workspace) -> Task {
    let task = workspace.task("backlog requirements");
    freshness::apply_explicit_pilot_assessment(workspace, &task, false);
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Backlog
    );
    task
}

fn edit_requirements(workspace: &Workspace, task: &Task, field: &str) {
    let mut input = json!({"id": task.id, "model": "codex"});
    input[field] = if field == "description" {
        json!("Changed requirements.")
    } else {
        json!(["Changed acceptance criterion."])
    };
    workspace
        .runtime
        .run_tool("orbit.task.update", input)
        .unwrap();
    let selected = workspace.action("list_backlog_tasks", json!({}));
    assert_eq!(selected["task_ids"], json!([]), "{selected}");
    assert!(
        selected.to_string().contains("pilot_assessment_stale"),
        "{selected}"
    );
}

fn edited_backlog_is_repiloted_once_by_the_bundled_routine() {
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exec sleep 30"]);
    for field in ["description", "acceptance_criteria"] {
        let workspace = Workspace::new();
        workspace.install_pilot_job();
        let task = assessed_backlog(&workspace);
        let routine = bundled_routine();
        let now = Utc::now();
        let unchanged = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
        assert!(unchanged.batch.is_empty(), "{unchanged:?}");
        edit_requirements(&workspace, &task, field);
        let observed = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
        assert!(
            observed
                .state
                .unwrap()
                .members
                .unwrap()
                .pending
                .contains_key(&task.id)
        );
        let due = evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            now + Duration::minutes(3),
        )
        .unwrap();
        let attempt = due.state.unwrap().members.unwrap().active.unwrap();
        let run_id = attempt.action_id.as_ref().unwrap();
        assert_eq!(
            workspace
                .jobs
                .list_job_runs("task_pilot_pipeline")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            workspace
                .jobs
                .get_job_run(run_id)
                .unwrap()
                .unwrap()
                .input
                .unwrap()["task_ids"],
            json!([task.id])
        );
        let preparing = evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            now + Duration::minutes(4),
        )
        .unwrap();
        assert_eq!(preparing.reason, "batch_pending", "{preparing:?}");
        assert_eq!(
            workspace
                .jobs
                .list_job_runs("task_pilot_pipeline")
                .unwrap()
                .len(),
            1
        );
        let prepared = prepare_claim(&workspace, &attempt);
        assert_eq!(prepared["task_ids"], json!([task.id]), "{prepared}");
        let applied = apply_claim(&workspace, &attempt, &prepared);
        assert_eq!(applied["applied_count"], 1, "{applied}");
        workspace.record_steps(run_id, &[("prepare", &prepared), ("apply", &applied)]);
        workspace.runtime.cancel_job_run(run_id).unwrap();
        assert_eq!(
            workspace.action("list_backlog_tasks", json!({}))["task_ids"],
            json!([task.id])
        );
        for minutes in [15, 60, 1440] {
            let evaluated = evaluate_routine(
                &workspace.runtime,
                &routine,
                false,
                now + Duration::minutes(minutes),
            )
            .unwrap();
            assert!(evaluated.batch.is_empty(), "{evaluated:?}");
            assert!(evaluated.state.unwrap().members.unwrap().active.is_none());
            assert_eq!(
                workspace
                    .jobs
                    .list_job_runs("task_pilot_pipeline")
                    .unwrap()
                    .len(),
                1
            );
        }
    }
}

fn hold_execution_claim(workspace: &Workspace, task: &Task) -> rusqlite::Connection {
    use orbit_store::contracts::{
        AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase, ExecutionLocation,
    };
    let claim = ExecutionClaim {
        claim_id: "backlog-repilot-claim".into(),
        task_id: task.id.clone(),
        request_id: "backlog-repilot-request".into(),
        executed_on: ExecutionLocation {
            machine_id: "fixture-machine".into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: "fixture-claim-run".into(),
            job_name: "task_pr_pipeline".into(),
            machine_name: None,
        },
        footprint: Vec::new(),
        reservation_id: "backlog-repilot-reservation".into(),
        reservation_expires_at: (Utc::now() + Duration::minutes(30)).to_rfc3339(),
        phase: ExecutionClaimPhase::Running,
        repair: None,
    };
    let connection =
        rusqlite::Connection::open(workspace.runtime.global_root().join("orbit.db")).unwrap();
    connection.execute(
        "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
         VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
        rusqlite::params![workspace.runtime.workspace_id().unwrap(), claim.claim_id, serde_json::to_string(&claim).unwrap(), Utc::now().to_rfc3339()],
    ).unwrap();
    connection
}

fn live_execution_claim_keeps_edited_backlog_settled() {
    let workspace = Workspace::new();
    let task = assessed_backlog(&workspace);
    edit_requirements(&workspace, &task, "description");
    let connection = hold_execution_claim(&workspace, &task);
    let routine = bundled_routine();
    for minutes in [3, 15] {
        let held = evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            Utc::now() + Duration::minutes(minutes),
        )
        .unwrap();
        assert!(held.batch.is_empty(), "{held:?}");
        let members = held.state.unwrap().members.unwrap();
        assert!(members.active.is_none());
        assert_eq!(members.withheld[&task.id], "execution_claim_active");
        assert!(
            workspace
                .jobs
                .list_job_runs("task_pilot_pipeline")
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        workspace.action("list_backlog_tasks", json!({}))["task_ids"],
        json!([])
    );
    // Releasing execution makes the stale task observable again without an
    // operator repilot; the old accepted receipt must not permanently retire it.
    connection
        .execute(
            "DELETE FROM task_coordination_rows WHERE row_id = ?1",
            ["backlog-repilot-claim"],
        )
        .unwrap();
    let observed = evaluate_routine(&workspace.runtime, &routine, true, Utc::now()).unwrap();
    assert!(
        observed
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&task.id)
    );
}

fn failed_backlog_repilot_stops_at_the_routine_retry_bound() {
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exec sleep 30"]);
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = assessed_backlog(&workspace);
    edit_requirements(&workspace, &task, "description");
    let routine = bundled_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let due = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    let mut attempt = due.state.unwrap().members.unwrap().active.unwrap();
    let max_attempts = attempt.max_attempts;
    let mut tick = 3;
    // Follow the configured retry budget rather than pinning its policy.
    for index in 1..=max_attempts {
        assert_eq!(attempt.attempt, index);
        workspace
            .runtime
            .cancel_job_run(attempt.action_id.as_ref().unwrap())
            .unwrap();
        tick += 1;
        let failed = evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            now + Duration::minutes(tick),
        )
        .unwrap();
        if index < max_attempts {
            assert_eq!(failed.reason, "retry_backoff", "{failed:?}");
            tick += 6;
            let retry = evaluate_routine(
                &workspace.runtime,
                &routine,
                false,
                now + Duration::minutes(tick),
            )
            .unwrap();
            attempt = retry.state.unwrap().members.unwrap().active.unwrap();
        }
    }
    for minutes in [tick + 1, tick + 60, tick + 1440] {
        let held = evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            now + Duration::minutes(minutes),
        )
        .unwrap();
        assert!(held.batch.is_empty(), "{held:?}");
        let members = held.state.unwrap().members.unwrap();
        assert!(members.active.is_none());
        assert!(members.failed[&task.id].exhausted);
        assert_eq!(
            workspace
                .jobs
                .list_job_runs("task_pilot_pipeline")
                .unwrap()
                .len(),
            max_attempts as usize
        );
        assert_eq!(
            workspace.action("list_backlog_tasks", json!({}))["task_ids"],
            json!([])
        );
    }
    let connection = hold_execution_claim(&workspace, &task);
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(tick + 1441),
    )
    .unwrap();
    connection
        .execute(
            "DELETE FROM task_coordination_rows WHERE row_id = ?1",
            ["backlog-repilot-claim"],
        )
        .unwrap();
    let held = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(tick + 1442),
    )
    .unwrap();
    let members = held.state.unwrap().members.unwrap();
    assert!(
        members.failed[&task.id].exhausted,
        "execution holds must preserve the failed input"
    );
    assert!(members.active.is_none());
    assert_eq!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .len(),
        max_attempts as usize
    );
}

crate::scenario_table!(
    backlog_repilot_scenarios,
    edited_backlog_is_repiloted_once_by_the_bundled_routine,
    live_execution_claim_keeps_edited_backlog_settled,
    failed_backlog_repilot_stops_at_the_routine_retry_bound
);
