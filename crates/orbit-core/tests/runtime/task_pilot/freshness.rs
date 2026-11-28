//! Completed explicit pilot evidence is reusable by a matching routine, while
//! active work and changes to the consumer's material contract remain guarded.

use orbit_core::application::task::TaskUpdateParams;
use orbit_core::test_support::fixture_engine::TaskAutomationUpdate;
use orbit_engine::RunLifecycle;
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, PluginApply,
};
use orbit_types::workflow::automation::members::SourceSensitivity;

use super::races::apply_input;
use super::*;

pub(super) fn apply_explicit_pilot_assessment(workspace: &Workspace, task: &Task, warning: bool) {
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

fn assert_pilot_routine_stays_fresh(
    workspace: &Workspace,
    routine: &RoutineDefinition,
    reason: &str,
) {
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
    apply_explicit_pilot_assessment(&workspace, &task, false);
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
    assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");
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
    assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");

    let readiness = orbit_core::unit::workspace_auto::workspace_auto_readiness(
        &workspace.runtime,
        &[],
        None,
        50,
        &[],
    )
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
                "id": task.id, "model": "codex", "title": "Changed administrative title.",
            }),
        )
        .unwrap();
    assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");
}

/// Metadata edits, source moves, a changed freshness policy and another
/// consumer never re-pilot a task whose explicit evidence is on record: every
/// branch must stay fresh.
fn routine_reuse_ignores_metadata_source_policy_and_consumer_changes() {
    for change in ["material", "source", "policy", "consumer"] {
        let workspace = Workspace::new();
        let task = workspace.task(change);
        apply_explicit_pilot_assessment(&workspace, &task, false);
        let mut routine = pilot_routine();
        assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");
        workspace.commit_file(
            "unrelated.rs",
            "fn unrelated() {}\n",
            "unrelated source move",
        );
        assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");
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
        assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");
    }
}

fn newer_unready_explicit_evidence_is_fresh_without_releasing_active_pilot_holds() {
    let workspace = Workspace::new();
    let task = workspace.task("fresh unready verdict");
    let routine = pilot_routine();
    apply_explicit_pilot_assessment(&workspace, &task, false);
    assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh");
    apply_explicit_pilot_assessment(&workspace, &task, true);
    assert_pilot_routine_stays_fresh(&workspace, &routine, "fresh_unready");
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
    apply_explicit_pilot_assessment(&workspace, &task, false);
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

fn bind_pr_ship_mode(workspace: &mut Workspace) {
    let workspace_id = workspace.runtime.workspace_id().unwrap();
    workspace.runtime = OrbitRuntime::from_roots_with_binding(
        &workspace.runtime.global_root(),
        &workspace.repo.join(".orbit"),
        orbit_core::runtime::WorkspaceRuntimeBinding {
            logical_workspace_id: workspace_id.clone(),
            task_partition_id: workspace_id,
            owner_machine_id: None,
            checkout_role: None,
            repo_root: workspace.repo.clone(),
            ship_mode: orbit_core::app::run::ShipMode::Pr,
            base_branch: Some("main".into()),
        },
    )
    .unwrap()
    .with_host_resource_probe(super::super::dispatch_admission::PressureProbe::calm())
    .with_automation_machine_identity(Some("fixture-machine".into()));
}

fn discover(workspace: &Workspace) -> Value {
    workspace.action(
        "prepare_task_pilot",
        json!({"workspace_path": workspace.repo, "base_branch": "main"}),
    )
}

fn edit_description_and_tags(workspace: &Workspace, task_id: &str, tags: &[&str]) {
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task_id, "model": "codex",
                "description": "Changed requirements.", "tags": tags,
            }),
        )
        .unwrap();
}

fn stale_scoped_no_diff_task_is_excluded_while_eligible_stale_task_is_selected() {
    let workspace = Workspace::new();
    let excluded = workspace.task("stale no-diff");
    let eligible = workspace.task("stale eligible");
    apply_explicit_pilot_assessment(&workspace, &excluded, false);
    apply_explicit_pilot_assessment(&workspace, &eligible, false);
    edit_description_and_tags(&workspace, &excluded.id, &["no-diff-expected"]);
    edit_description_and_tags(&workspace, &eligible.id, &[]);
    let discovered = discover(&workspace);
    assert_eq!(discovered["task_ids"], json!([eligible.id]), "{discovered}");
    assert_eq!(
        discovered["excluded_by_reason"]["no_diff_task"], 1,
        "{discovered}"
    );
    assert!(
        discovered["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["task_id"] == excluded.id && entry["reason"] == "no_diff_task"),
        "{discovered}"
    );
}

fn stale_scoped_pr_route_task_without_forge_is_excluded_while_local_route_is_selected() {
    let mut workspace = Workspace::new();
    let excluded = workspace.task("stale pr route");
    let eligible = workspace.task("stale local route");
    apply_explicit_pilot_assessment(&workspace, &excluded, false);
    apply_explicit_pilot_assessment(&workspace, &eligible, false);
    bind_pr_ship_mode(&mut workspace);
    edit_description_and_tags(&workspace, &excluded.id, &[]);
    edit_description_and_tags(&workspace, &eligible.id, &["delivery:task_local_pipeline"]);
    let discovered = discover(&workspace);
    assert_eq!(discovered["task_ids"], json!([eligible.id]), "{discovered}");
    assert_eq!(
        discovered["excluded_by_reason"]["pr_forge_remote_missing"], 1,
        "{discovered}"
    );
    assert!(
        discovered["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["task_id"] == excluded.id
                && entry["reason"] == "pr_forge_remote_missing"),
        "{discovered}"
    );
}

fn automatic_discovery_repilots_previously_piloted_tasks_after_a_meaning_edit() {
    let workspace = Workspace::new();
    let task = workspace.task("automatic discovery");
    let discover = || {
        workspace.action(
            "prepare_task_pilot",
            json!({"workspace_path": workspace.repo, "base_branch": "main"}),
        )
    };
    assert_eq!(discover()["task_ids"], json!([task.id]));
    apply_explicit_pilot_assessment(&workspace, &task, true);
    // Fresh: the applied assessment still certifies the task, so automatic
    // discovery leaves it alone.
    assert_eq!(discover()["task_ids"], json!([]));
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "description": "Changed requirements."
            }),
        )
        .unwrap();
    // The description edit makes the assessment stale: discovery queues a
    // fresh pilot without an operator repilot.
    assert_eq!(discover()["task_ids"], json!([task.id]));
    assert_eq!(workspace.prepare(&[&task.id])["task_ids"], json!([task.id]));
}

fn automatic_discovery_retries_failed_repilot_only_after_new_requirements() {
    for field in ["description", "acceptance_criteria"] {
        let workspace = Workspace::new();
        let task = workspace.task("failed repilot");
        apply_explicit_pilot_assessment(&workspace, &task, false);
        edit_description_and_tags(&workspace, &task.id, &[]);
        assert_eq!(discover(&workspace)["task_ids"], json!([task.id]));

        // Model the durable failure a drain records after its stale repilot
        // exits without applying an assessment. Exercise discovery through
        // the public prepare action, with real task updates and receipts.
        let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
            &orbit_store::maintenance::task_registry::task_registry_path(
                &workspace.runtime.global_root(),
            ),
        )
        .unwrap();
        let backends = orbit_store::compose::workspace_coordinated_backends(
            registry,
            workspace.runtime.workspace_id().unwrap(),
            workspace.runtime.sqlite_store().unwrap(),
        )
        .unwrap();
        backends
            .task
            .history
            .update_task_history(
                &task.id,
                orbit_store::contract::TaskHistoryUpdateParams {
                    actor: "system".into(),
                    append_history: vec![orbit_types::task::TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: "task_pilot_unresolved".into(),
                        note: Some(json!({"pilot_run_id": "failed-repilot"}).to_string()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                        surface: None,
                        caller: None,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(discover(&workspace)["task_ids"], json!([]));
        workspace
            .runtime
            .run_tool(
                "orbit.task.update",
                json!({
                    "id": task.id, "model": "codex",
                    "title": "Administrative rename", "tags": ["metadata"],
                    "plan": "Administrative plan update.",
                    "execution_summary": "Still held.", "comment": "Observation.",
                    "priority": "high", "description": "Changed requirements.",
                }),
            )
            .unwrap();
        assert_eq!(
            discover(&workspace)["task_ids"],
            json!([]),
            "metadata and an unchanged description must preserve the failure hold"
        );

        let mut edit = json!({"id": task.id, "model": "codex"});
        edit[field] = if field == "description" {
            json!("Requirements changed again after the failed repilot.")
        } else {
            json!(["A new acceptance criterion after the failed repilot."])
        };
        workspace
            .runtime
            .run_tool("orbit.task.update", edit)
            .unwrap();
        let retried = discover(&workspace);
        assert_eq!(
            retried["task_ids"],
            json!([task.id]),
            "a later {field} edit must release the failed repilot: {retried}"
        );
        let holder = workspace.hold(&[&task.id]);
        assert_eq!(
            discover(&workspace)["task_ids"],
            json!([]),
            "the fresh pilot's live preparation must prevent duplicate selection"
        );
        workspace.runtime.cancel_job_run(&holder).unwrap();
        let applied = workspace.action(
            "apply_task_pilot_results",
            apply_input(&workspace, &retried),
        );
        assert_eq!(applied["applied_count"], 1, "{applied}");
        for _ in 0..2 {
            assert_eq!(
                discover(&workspace)["task_ids"],
                json!([]),
                "the fresh assessment must not queue another automatic pilot"
            );
        }
        assert_eq!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .filter(|entry| entry.event == "task_pilot_applied")
                .count(),
            2
        );
    }
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
        apply_explicit_pilot_assessment(&workspace, &task, true);
        let tool = if update {
            "orbit.task.update"
        } else {
            "orbit.task.repilot"
        };
        let mut input = json!({"id": task.id, "workspace": workspace.repo});
        if update {
            input["repilot"] = json!(true);
        }
        let refused = workspace.runtime.run_tool(tool, input.clone()).unwrap_err();
        if update {
            let message = refused.to_string();
            assert!(message.contains("`operator` capability"), "{message}");
            assert!(message.contains("orbit task repilot"), "{message}");
        }
        // The runtime resolves the caller from the Env it is built with, so
        // the operator grant must be in that Env.
        let _operator = orbit_common::test_env::scoped([("ORBIT_OPERATOR", Some("1"))]);
        let human = orbit_core::test_support::with_runtime_env(
            runtime_at(
                &workspace.runtime.global_root(),
                &workspace.repo.join(".orbit"),
            ),
            orbit_common::Env::capture(),
        )
        .with_actor(orbit_core::runtime::context::ActorIdentity::human(
            "human:operator",
        ));
        if update {
            let mut unsupported = input.clone();
            unsupported["status"] = json!("someday");
            let message = human.run_tool(tool, unsupported).unwrap_err().to_string();
            assert!(message.contains("someday"), "{message}");
            assert!(message.contains("orbit task repilot"), "{message}");
            assert_eq!(
                human.get_task(&task.id).unwrap().status,
                TaskStatus::Proposed
            );
        }
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
fn decision_rewrite_repilots_before_dispatch() {
    use orbit_core::application::task::{FinalRecoveryCompletion, FinalRecoveryRequest};
    use orbit_types::task::DecisionRequestState;

    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exec sleep 30"]);
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("decision recovery");
    apply_explicit_pilot_assessment(&workspace, &task, false);
    let runtime = &workspace.runtime;
    runtime.approve_task(&task.id, None, None).unwrap();
    RunLifecycle::admit_task_for_workflow(
        &orbit_core::test_support::EngineHost::new(runtime),
        &task.id,
        "worktree_setup",
    )
    .unwrap();
    let running = runtime.get_task(&task.id).unwrap();
    let observed = runtime.final_recovery_revision(&running).unwrap();
    let criteria = ["Verify the retry also covers the failure path."];
    runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: task.id.clone(),
                run_id: "jrun-decision-rewrite".into(),
                observed,
                repo_root: workspace.repo.clone(),
                base_ref: "main".into(),
                completion: FinalRecoveryCompletion::Review,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "decide",
                "summary": "Choose which requirements the next attempt should cover.",
                "options": [
                    {
                        "summary": "Cover the failure path",
                        "consequence": "The next attempt verifies failure handling.",
                        "rewrite": {"acceptance_criteria": criteria},
                        "action": {"kind": "requeue"}
                    },
                    {
                        "summary": "Retire the task",
                        "consequence": "There is no next attempt.",
                        "action": {"kind": "archive"}
                    }
                ],
                "recommended": 1
            })),
        )
        .unwrap();
    let blocked = runtime.get_task(&task.id).unwrap();
    assert_eq!(blocked.status, TaskStatus::Blocked);
    let request = runtime
        .list_decision_requests(None, Some(&task.id), false)
        .unwrap()
        .pop()
        .unwrap();
    let answered = runtime
        .answer_decision_request_as_human(&request.id, "1", None, "human:operator".into(), None)
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    let ready_for_pilot = runtime.get_task(&task.id).unwrap();
    assert_eq!(ready_for_pilot.status, TaskStatus::Backlog);
    assert_eq!(ready_for_pilot.acceptance_criteria, criteria);
    assert_eq!(ready_for_pilot.description, blocked.description);
    assert_eq!(ready_for_pilot.tags, blocked.tags);

    let held = RunLifecycle::admit_task_for_workflow(
        &orbit_core::test_support::EngineHost::new(runtime),
        &task.id,
        "worktree_setup",
    )
    .unwrap_err()
    .to_string();
    assert!(held.contains("changed after its latest pilot"), "{held}");
    let runs = workspace.jobs.list_job_runs("task_pilot_pipeline").unwrap();
    assert_eq!(runs.len(), 1, "the answer must request a new pilot");
    let run = &runs[0];
    assert_eq!(run.input.as_ref().unwrap()["task_ids"], json!([task.id]));
    let prepared = workspace.prepare_in_run(&run.run_id, &[&task.id]);
    assert_eq!(prepared["task_ids"], json!([task.id]), "{prepared}");
    let applied = workspace.action(
        "apply_task_pilot_results",
        apply_input(&workspace, &prepared),
    );
    assert_eq!(applied["applied_count"], 1, "{applied}");
    workspace.record_steps(&run.run_id, &[("prepare", &prepared), ("apply", &applied)]);
    runtime.cancel_job_run(&run.run_id).unwrap();
    assert_eq!(
        RunLifecycle::admit_task_for_workflow(
            &orbit_core::test_support::EngineHost::new(runtime),
            &task.id,
            "worktree_setup"
        )
        .unwrap()
        .status,
        TaskStatus::InProgress
    );
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
    apply_explicit_pilot_assessment(&workspace, &task, true);
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
    .with_actor(orbit_core::runtime::context::ActorIdentity::human(
        "human:operator",
    ));
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

#[cfg(unix)]
fn held_warning_triage_does_not_raise_approval_alongside_plugin_decision() {
    use orbit_core::application::task::{PilotWarningDecision, PilotWarningTriageRequest};
    use orbit_types::task::DecisionRequestState;

    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("warning with plugin decision");
    apply_explicit_pilot_assessment(&workspace, &task, true);
    let plugin_request = DecisionRequest::new(
        task.id.clone(),
        DecisionGate::PluginRequest,
        vec![
            DecisionAnswer::plain("accept", "Accept", "Record acceptance."),
            DecisionAnswer::plain("reject", "Reject", "Record rejection."),
        ],
        DecisionBinding::PluginObject {
            plugin: "fixture".into(),
            subject: "result-1".into(),
            revision: "rev-1".into(),
            manifest_digest: "digest-1".into(),
        },
        "plugin:fixture".into(),
        Utc::now(),
    )
    .with_summary("Accept the fixture result?".into(), None)
    .with_plugin_apply(PluginApply {
        plugin: "fixture".into(),
        subject: "result-1".into(),
        tool: "fixture.apply".into(),
        manifest_digest: "digest-1".into(),
        input: json!({"result": "result-1"}),
        revision: "rev-1".into(),
        requested_by: Some("fixture-operator".into()),
    });
    orbit_core::test_support::EngineHost::new(&workspace.runtime)
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                append_history: vec![plugin_request.history_entry("plugin:fixture")],
                ..Default::default()
            },
        )
        .unwrap();

    let triage = workspace
        .runtime
        .record_pilot_warning_triage(&PilotWarningTriageRequest {
            task_id: task.id.clone(),
            requested_by: "codex".into(),
            decision: PilotWarningDecision::Propose {
                summary: "Choose whether to reassess the warning.".into(),
                options: serde_json::from_value(json!([
                    {
                        "summary": "Assess again",
                        "consequence": "Submit one explicit pilot.",
                        "action": {"kind": "re_pilot"}
                    },
                    {
                        "summary": "Archive",
                        "consequence": "Retire the work.",
                        "action": {"kind": "archive"}
                    }
                ]))
                .unwrap(),
                recommended: 1,
            },
        })
        .unwrap();
    assert_eq!(triage["outcome"], "decision_pending", "{triage}");
    assert!(
        triage["comment"]
            .as_str()
            .unwrap()
            .contains("No proposed_approval was raised")
    );
    assert_eq!(
        workspace
            .runtime
            .list_decision_requests(Some(DecisionGate::ProposedApproval), Some(&task.id), false)
            .unwrap()
            .len(),
        0,
        "pilot-warning triage must not add a competing generic approval"
    );
    let requests = workspace
        .runtime
        .list_decision_requests(Some(DecisionGate::PluginRequest), Some(&task.id), false)
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].state, DecisionRequestState::Open);
}

crate::scenario_table!(
    explicit_assessment_freshness_scenarios,
    explicit_assessment_survives_selector_apply_promotion_and_administrative_edits,
    routine_reuse_ignores_metadata_source_policy_and_consumer_changes,
    newer_unready_explicit_evidence_is_fresh_without_releasing_active_pilot_holds,
    admission_reuses_an_explicit_assessment_off_the_scan_page,
    automatic_discovery_repilots_previously_piloted_tasks_after_a_meaning_edit,
    automatic_discovery_retries_failed_repilot_only_after_new_requirements,
    stale_scoped_no_diff_task_is_excluded_while_eligible_stale_task_is_selected,
    stale_scoped_pr_route_task_without_forge_is_excluded_while_local_route_is_selected
);

// The inert substitute worker uses the same Unix shell fixture as other
// submission tests; existing freshness/admission cases stay cross-platform.
#[cfg(unix)]
crate::scenario_table!(
    explicit_repilot_request_scenarios,
    decision_rewrite_repilots_before_dispatch,
    explicit_repilot_tool_and_update_submit_once_with_live_dedup,
    held_warning_keeps_one_decision_until_explicit_decision_repilot,
    held_warning_triage_does_not_raise_approval_alongside_plugin_decision,
    a_never_piloted_task_is_automatically_admitted_once
);
