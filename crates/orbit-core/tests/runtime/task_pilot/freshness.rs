//! Completed explicit pilot evidence is reusable by a matching routine, while
//! active work and changes to the consumer's material contract remain guarded.

use orbit_core::application::task::TaskUpdateParams;
use orbit_types::workflow::automation::members::SourceSensitivity;

use super::races::apply_input;
use super::*;

fn assess(workspace: &Workspace, task: &Task, warning: bool) {
    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(prepared["task_ids"], json!([task.id]));
    let mut input = apply_input(workspace, &prepared);
    for field in ["utility_warnings", "surface_warnings"] {
        input["results"][0]["tasks"][0][field] = json!([{
            "text": "Optional adjacent cleanup outside this task.",
            "holds_promotion": false,
        }]);
    }
    if warning {
        input["results"][0]["tasks"][0]["utility_warnings"] =
            json!(["Operator review is needed before implementation."]);
    }
    let applied = workspace.action("apply_task_pilot_results", input);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(applied["status"], "succeeded", "{applied}");
}

fn assert_fresh(workspace: &Workspace, routine: &RoutineDefinition, reason: &str) {
    // Both real evaluations exceed the debounce/max-wait windows. Fresh
    // evidence must never reach provider admission, even on the next tick.
    for minutes in [3, 15] {
        let evaluated = evaluate_routine(
            &workspace.runtime,
            routine,
            false,
            Utc::now() + Duration::minutes(minutes),
        )
        .unwrap();
        assert_eq!(evaluated.reason, reason, "{evaluated:?}");
        assert!(evaluated.batch.is_empty());
        let members = evaluated.state.unwrap().members.unwrap();
        assert!(members.pending.is_empty());
        assert!(members.active.is_none());
        assert!(members.withheld.is_empty());
    }
    assert!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .is_empty()
    );
}

fn explicit_assessment_survives_selector_apply_promotion_and_administrative_edits() {
    let workspace = Workspace::new();
    let task = workspace.task("fresh explicit assessment");
    assert!(task.context_files.is_empty());
    assess(&workspace, &task, false);
    let stored = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(stored.context_files, ["file:README.md"]);
    assert_eq!(stored.status, TaskStatus::Proposed);
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
    let routine = pilot_routine();
    assert_fresh(&workspace, &routine, "fresh");
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "comment": "Administrative observation.",
                "priority": "high", "execution_summary": "Still awaiting ordinary dispatch.",
            }),
        )
        .unwrap();
    workspace.commit_file(
        "unrelated.rs",
        "fn unrelated() {}\n",
        "unrelated source move",
    );
    assert_fresh(&workspace, &routine, "fresh");

    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task.id)
        .unwrap();
    assert_eq!(entry["eligible"], true, "{readiness}");
    let selected = workspace.action("list_backlog_tasks", json!({}));
    assert_eq!(selected["task_ids"], json!([task.id]), "{selected}");
    // An explicit deliberate audit still selects this task despite freshness.
    assert_eq!(workspace.prepare(&[&task.id])["task_ids"], json!([task.id]));

    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "description": "Materially changed requirements.",
            }),
        )
        .unwrap();
    assert_fresh(&workspace, &routine, "fresh");
}

fn routine_reuse_obeys_configured_material_source_and_consumer_policy() {
    for change in ["material", "source", "policy", "consumer"] {
        let mut workspace = Workspace::new();
        let config = workspace.repo.join(".orbit/config.toml");
        let mut contents = std::fs::read_to_string(&config).unwrap();
        contents.push_str("\n[workflow.task_pilot_freshness]\nmaterial_fields = [\"title\", \"selectors\", \"tags\"]\nsource_sensitivity = \"context_files\"\n");
        std::fs::write(config, contents).unwrap();
        workspace.runtime = runtime_at(
            &workspace.runtime.global_root(),
            &workspace.repo.join(".orbit"),
        );
        let task = workspace.task(change);
        assess(&workspace, &task, false);
        let mut routine = pilot_routine();
        assert_fresh(&workspace, &routine, "fresh");
        workspace.commit_file(
            "unrelated.rs",
            "fn unrelated() {}\n",
            "unrelated source move",
        );
        assert_fresh(&workspace, &routine, "fresh");
        match change {
            "material" => {
                workspace
                    .runtime
                    .run_tool(
                        "orbit.task.update",
                        json!({
                            "id": task.id, "model": "codex", "tags": ["changed"],
                        }),
                    )
                    .unwrap();
            }
            "source" => workspace.commit_file("README.md", "changed\n", "selector source move"),
            "policy" | "consumer" => {
                // Both a changed existing consumer and another consumer must
                // evaluate evidence under their own resolved contract.
                if change == "consumer" {
                    routine.name = "other-pilot".into();
                }
                routine
                    .trigger
                    .state
                    .as_mut()
                    .unwrap()
                    .freshness
                    .source_sensitivity = Some(SourceSensitivity::Any);
            }
            _ => unreachable!(),
        }
        assert_fresh(&workspace, &routine, "fresh");
    }
}

fn newer_unready_explicit_evidence_is_fresh_without_releasing_active_pilot_holds() {
    let workspace = Workspace::new();
    let task = workspace.task("fresh unready verdict");
    let routine = pilot_routine();
    assess(&workspace, &task, false);
    assert_fresh(&workspace, &routine, "fresh");
    assess(&workspace, &task, true);
    assert_fresh(&workspace, &routine, "fresh_unready");
    let holder = workspace.hold(&[&task.id]);
    let held = evaluate_routine(&workspace.runtime, &routine, false, Utc::now()).unwrap();
    assert_eq!(held.reason, "work_withheld");
    assert_eq!(
        held.state.unwrap().members.unwrap().withheld[&task.id],
        format!("already_preparing: {holder}")
    );
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let settled = evaluate_routine(&workspace.runtime, &routine, false, Utc::now()).unwrap();
    assert_eq!(settled.reason, "fresh_unready");
    assert!(settled.batch.is_empty());
}

fn admission_reuses_an_explicit_assessment_off_the_scan_page() {
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
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "newest pending member".into(),
            description: "Inspect README.md.".into(),
            plan: "Inspect README.md.".into(),
            acceptance_criteria: vec!["The README repair is observable.".into()],
            context_files: vec!["file:README.md".into()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .unwrap();
    let routine = pilot_routine();
    let now = Utc::now();
    let observed = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let members = observed.state.unwrap().members.unwrap();
    assert!(members.pending.contains_key(&task.id));
    assert!(members.scan_after.is_some());
    assess(&workspace, &task, false);
    let skipped = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert!(skipped.batch.is_empty(), "{skipped:?}");
    let members = skipped.state.unwrap().members.unwrap();
    assert!(!members.pending.contains_key(&task.id));
    assert!(members.active.is_none());
    assert!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .is_empty()
    );
}

fn automatic_discovery_skips_previously_piloted_tasks_after_edits() {
    let workspace = Workspace::new();
    let task = workspace.task("automatic discovery");
    let discover = || {
        workspace.action(
            "prepare_task_pilot",
            json!({"workspace_path": workspace.repo, "base_branch": "main"}),
        )
    };
    assert_eq!(discover()["task_ids"], json!([task.id]));
    assess(&workspace, &task, true);
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "description": "Changed requirements."
            }),
        )
        .unwrap();
    assert_eq!(discover()["task_ids"], json!([]));
    assert_eq!(workspace.prepare(&[&task.id])["task_ids"], json!([task.id]));
}

fn edited_assessment_blocks_dispatch_until_explicit_repilot() {
    let workspace = Workspace::new();
    let task = workspace.task("stale dispatch guard");
    assess(&workspace, &task, false);
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
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "description": "Changed requirements."
            }),
        )
        .unwrap();
    let selected = workspace.action("list_backlog_tasks", json!({}));
    assert_eq!(selected["task_ids"], json!([]), "{selected}");
    assert!(
        selected.to_string().contains("orbit task repilot"),
        "{selected}"
    );
    assert_fresh(&workspace, &pilot_routine(), "fresh");
    assess(&workspace, &task, false);
    let selected = workspace.action("list_backlog_tasks", json!({}));
    assert_eq!(selected["task_ids"], json!([task.id]), "{selected}");
    workspace.runtime.run_tool("orbit.task.update", json!({
        "id": task.id, "model": "codex", "acceptance_criteria": ["Changed acceptance criterion."]
    })).unwrap();
    let selected = workspace.action("list_backlog_tasks", json!({}));
    assert_eq!(selected["task_ids"], json!([]), "{selected}");
    assert_fresh(&workspace, &pilot_routine(), "fresh");
    assess(&workspace, &task, false);
    assert_eq!(
        workspace.action("list_backlog_tasks", json!({}))["task_ids"],
        json!([task.id])
    );
}

#[cfg(unix)]
fn explicit_repilot_tool_and_update_submit_once_with_live_dedup() {
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exec sleep 30"]);
    for update in [false, true] {
        let workspace = Workspace::new();
        workspace.install_pilot_job();
        let task = workspace.task("explicit repilot request");
        workspace
            .runtime
            .run_tool(
                "orbit.task.update",
                json!({"id": task.id, "tags": ["no-pilot"], "model": "codex"}),
            )
            .unwrap();
        assess(&workspace, &task, true);
        let tool = if update {
            "orbit.task.update"
        } else {
            "orbit.task.repilot"
        };
        let mut input = json!({"id": task.id, "workspace": workspace.repo});
        if update {
            input["repilot"] = json!(true);
        }
        assert!(
            workspace.runtime.run_tool(tool, input.clone()).is_err(),
            "an unprivileged caller cannot request a new pilot"
        );
        let human = runtime_at(
            &workspace.runtime.global_root(),
            &workspace.repo.join(".orbit"),
        )
        .with_actor(orbit_core::ActorIdentity::human("human:operator"));
        let _operator = orbit_common::test_env::scoped([("ORBIT_OPERATOR", Some("1"))]);
        let response = human.run_tool(tool, input.clone()).unwrap();
        let runs = workspace.jobs.list_job_runs("task_pilot_pipeline").unwrap();
        assert_eq!(runs.len(), 1, "{response}");
        assert_eq!(
            runs[0].input.as_ref().unwrap()["task_ids"],
            json!([task.id])
        );
        assert!(human.run_tool(tool, input.clone()).is_err());
        assert_eq!(
            workspace
                .jobs
                .list_job_runs("task_pilot_pipeline")
                .unwrap()
                .len(),
            1
        );
        workspace.runtime.cancel_job_run(&runs[0].run_id).unwrap();
    }
}

#[cfg(unix)]
fn a_never_piloted_task_is_automatically_admitted_once() {
    use super::source_moves::{apply_claim, prepare_claim};
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exec sleep 30"]);
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("first automatic assessment");
    let routine = pilot_routine();
    let now = Utc::now();
    let first = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    assert!(
        first
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&task.id)
    );
    let admitted = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    let attempt = admitted.state.unwrap().members.unwrap().active.unwrap();
    let run_id = attempt.action_id.clone().unwrap();
    assert_eq!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .len(),
        1
    );
    let prepared = prepare_claim(&workspace, &attempt);
    let applied = apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    workspace.record_steps(&run_id, &[("prepare", &prepared), ("apply", &applied)]);
    workspace.runtime.cancel_job_run(&run_id).unwrap();
    for minutes in [15, 60, 1440] {
        let evaluated = evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            now + Duration::minutes(minutes),
        )
        .unwrap();
        assert!(evaluated.batch.is_empty(), "{evaluated:?}");
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

#[cfg(unix)]
fn held_warning_keeps_one_decision_until_explicit_decision_repilot() {
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exec sleep 30"]);
    use orbit_core::application::task::{PilotWarningDecision, PilotWarningTriageRequest};
    use orbit_types::task::DecisionRequestState;
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("held warning stays quiet");
    assess(&workspace, &task, true);
    workspace.runtime.record_pilot_warning_triage(&PilotWarningTriageRequest {
        task_id: task.id.clone(), requested_by: "codex".into(),
        decision: PilotWarningDecision::Propose {
            summary: "Choose whether to reassess the warning.".into(),
            options: serde_json::from_value(json!([
                {"summary": "Assess again", "consequence": "One explicit pilot is submitted.", "action": {"kind": "re_pilot"}},
                {"summary": "Archive", "consequence": "Work is retired.", "action": {"kind": "archive"}}
            ])).unwrap(), recommended: 1,
        },
    }).unwrap();
    let request = workspace
        .runtime
        .list_decision_requests(None, Some(&task.id), false)
        .unwrap()
        .pop()
        .unwrap();
    let mut warning_routine = pilot_routine();
    warning_routine.name = "fixture-warning".into();
    warning_routine.target =
        serde_json::from_value(json!("job:escalation_triage_pipeline")).unwrap();
    warning_routine.trigger.state.as_mut().unwrap().kind = StateTriggerKind::PilotWarningsHeld;
    for minutes in [3, 15, 60, 1440] {
        for routine in [pilot_routine(), warning_routine.clone()] {
            let evaluated = evaluate_routine(
                &workspace.runtime,
                &routine,
                false,
                Utc::now() + Duration::minutes(minutes),
            )
            .unwrap();
            assert!(evaluated.batch.is_empty(), "{evaluated:?}");
        }
        let requests = workspace
            .runtime
            .list_decision_requests(None, Some(&task.id), false)
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, request.id);
        assert_eq!(requests[0].state, DecisionRequestState::Open);
    }
    for job in ["task_pilot_pipeline", "escalation_triage_pipeline"] {
        assert!(workspace.jobs.list_job_runs(job).unwrap().is_empty());
    }
    let human = runtime_at(
        &workspace.runtime.global_root(),
        &workspace.repo.join(".orbit"),
    )
    .with_actor(orbit_core::ActorIdentity::human("human:operator"));
    let answered = human
        .answer_decision_request_as_human(
            &request.id,
            "1",
            None,
            "human:operator".into(),
            Some("cli".into()),
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Proposed
    );
    assert_eq!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .len(),
        1
    );
    assert!(
        human
            .answer_decision_request_as_human(
                &request.id,
                "1",
                None,
                "human:operator".into(),
                Some("cli".into())
            )
            .is_err()
    );
    assert_eq!(
        workspace
            .jobs
            .list_job_runs("task_pilot_pipeline")
            .unwrap()
            .len(),
        1
    );
    let runs = workspace.jobs.list_job_runs("task_pilot_pipeline").unwrap();
    workspace.runtime.cancel_job_run(&runs[0].run_id).unwrap();
}

crate::scenario_table!(
    explicit_assessment_freshness_scenarios,
    explicit_assessment_survives_selector_apply_promotion_and_administrative_edits,
    routine_reuse_obeys_configured_material_source_and_consumer_policy,
    newer_unready_explicit_evidence_is_fresh_without_releasing_active_pilot_holds,
    admission_reuses_an_explicit_assessment_off_the_scan_page,
    automatic_discovery_skips_previously_piloted_tasks_after_edits,
    edited_assessment_blocks_dispatch_until_explicit_repilot
);

// The inert substitute worker uses the same Unix shell fixture as other
// submission tests; existing freshness/admission cases stay cross-platform.
#[cfg(unix)]
crate::scenario_table!(
    explicit_repilot_request_scenarios,
    explicit_repilot_tool_and_update_submit_once_with_live_dedup,
    held_warning_keeps_one_decision_until_explicit_decision_repilot,
    a_never_piloted_task_is_automatically_admitted_once
);
