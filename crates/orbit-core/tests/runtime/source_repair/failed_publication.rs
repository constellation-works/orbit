//! Stopped failed publications receive fresh authority through the public tool.
use super::*;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use std::path::Path;

fn escalate(f: &Fixture, id: &str) {
    let task = f.runtime.get_task(&f.task).unwrap();
    let request = FinalRecoveryRequest {
        task_id: f.task.clone(),
        run_id: id.into(),
        observed: f.runtime.final_recovery_revision(&task).unwrap(),
        repo_root: f.repo.clone(),
        base_ref: "origin/main".into(),
        completion: FinalRecoveryCompletion::Review,
        requeue_bound: Default::default(),
    };
    let outcome = f.runtime.apply_final_recovery(&request, Some(&json!({"decision":"escalate",
        "diagnosis":"publication needs explicit source repair", "human_action":"inspect retained host publication"}))).unwrap();
    assert!(matches!(outcome, FinalRecoveryOutcome::Escalated { .. }));
}

/// The engine's transient-infrastructure escalation, through the host
/// contract it calls: admission, then the classified application.
fn infra_escalate(
    f: &Fixture,
    run_id: &str,
    failed_step: &str,
    checkout: &Path,
    class: orbit_types::workflow::TransientInfrastructure,
) {
    use orbit_engine::{
        FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
        FinalRecoveryApplied,
    };
    let admission = RuntimeHost::admit_final_recovery(
        &f.runtime,
        run_id,
        &FinalRecoveryAdmissionRequest {
            task_id: f.task.clone(),
            failed_step_id: failed_step.into(),
            base_ref: Some("main".into()),
        },
    )
    .unwrap();
    assert_eq!(admission, FinalRecoveryAdmission::Admitted);
    let applied = RuntimeHost::apply_final_recovery(
        &f.runtime,
        run_id,
        &FinalRecoveryApplication {
            task_id: f.task.clone(),
            failed_step_id: failed_step.into(),
            decision: orbit_types::workflow::FinalRecoveryDecision::Escalate {
                diagnosis: format!("step `{failed_step}` failed on transient infrastructure"),
                human_action: "wait for the condition to clear".into(),
            },
            resume_step_index: None,
            repair_commit: None,
            resume_context: None,
            workspace_path: checkout.to_path_buf(),
            completion_done: false,
            infrastructure: Some(class),
        },
    )
    .unwrap();
    assert!(
        matches!(applied, FinalRecoveryApplied::Escalated { .. }),
        "{applied:?}"
    );
}

fn failed_after_push_gets_fresh_authority_without_replaying_the_attempt() {
    let f = Fixture::with_policy("before-landing");
    let first_input = f.submit(f.inspect());
    let first = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", first_input.clone())
        .unwrap();
    let id = first["run_id"].as_str().unwrap();
    let head = generations::publish_and_stop(&f, id, 1, true);
    escalate(&f, id);
    let before = serde_json::to_value(f.runtime.read_run_state(id).unwrap()).unwrap();
    let mut inspect = f.request("inspect");
    inspect["id"] = json!(id);
    let observed = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", inspect)
        .unwrap();
    assert_eq!(observed["binding"]["head_sha"], head);
    assert_eq!(observed["binding"]["branch"], f.branch);
    assert_eq!(observed["binding"]["pull_request"], "17");
    let mut submit = f.submit(observed["binding"].clone());
    submit["id"] = json!(id);
    submit["source_repair"]["request_key"] = json!("failed-publication-repair");
    let next = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit.clone())
        .unwrap();
    let next_id = next["run_id"].as_str().unwrap();
    f.wait(next_id);
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit)
        .unwrap();
    assert_eq!(replay["run_id"], next_id);
    let old_replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", first_input)
        .unwrap();
    assert_eq!(old_replay["run_id"], id);
    let error = f
        .runtime
        .submit_resume_run(id, Some("operator"), None)
        .unwrap_err();
    assert!(error.to_string().contains("one attempt"), "{error}");
    let next_head = generations::publish_and_cancel(&f, next_id, 2);
    assert_ne!(next_head, head);
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(id).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == "source_repair_reserved")
            .count(),
        2
    );
}

crate::scenario_table!(
    failed_publication_scenarios,
    failed_after_push_gets_fresh_authority_without_replaying_the_attempt,
    failed_push_refusals_preserve_task_and_receipts
);

crate::scenario_table!(
    failed_handoff_publication_scenarios,
    failure_handoff_reconstructs_the_exact_repair_and_requires_fresh_gates,
    handoff_refusals_require_the_exact_durable_publication
);

fn failure_handoff_fixture() -> (Fixture, String) {
    handoff_fixture(None)
}

// Some(false) retains the reviewer head; Some(true) preserves one dirty repair
// on top. None keeps the pre-review validation-failure scenario.
fn handoff_fixture(reviewer: Option<bool>) -> (Fixture, String) {
    escalated_handoff_fixture(reviewer, None)
}

/// [`handoff_fixture`], escalated through the engine's host contract with
/// `infra` as the engine's classification when it is set.
pub(super) fn escalated_handoff_fixture(
    reviewer: Option<bool>,
    infra: Option<orbit_types::workflow::TransientInfrastructure>,
) -> (Fixture, String) {
    escalated_handoff_fixture_with(reviewer, infra, &|_| {})
}

/// [`escalated_handoff_fixture`], calling `mid_handoff` once final recovery
/// has blocked the task and before the still-live run's failure handoff
/// publishes its candidate.
pub(super) fn escalated_handoff_fixture_with(
    reviewer: Option<bool>,
    infra: Option<orbit_types::workflow::TransientInfrastructure>,
    mid_handoff: &dyn Fn(&Fixture),
) -> (Fixture, String) {
    let mut f = if reviewer.is_some() {
        Fixture::with_policy("before-pr")
    } else {
        Fixture::handoff()
    };
    if infra.is_some() {
        f.enable_final_recovery();
    }
    std::fs::write(f._root.path().join("bin/gh"), format!(
        "#!/bin/sh\ncase \"$1 $2\" in\n 'pr list') printf '['; cat '{}'; printf ']';;\n 'pr create') echo https://github.com/owner/repository/pull/17;;\n 'pr view') cat '{}';;\n *) exit 99;;\nesac\n",
        f.answer.display(), f.answer.display())).unwrap();

    let definition_path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let mut job: Value =
        serde_yaml::from_str(&std::fs::read_to_string(&definition_path).unwrap()).unwrap();
    job["spec"]["failure_activity"] = json!("pr_failure_handoff");
    job["spec"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"validate","target":"activity:candidate_validate"}));
    if reviewer.is_some() {
        let steps = job["spec"]["steps"].as_array_mut().unwrap();
        steps.insert(
            4,
            json!({"id":"review_gate_settle","target":"activity:review_gate_settle"}),
        );
        steps.insert(5, json!({"id":"review_validate","target":"activity:candidate_validate",
            "default_input":{"ownership_base_sha":"{{ steps.review_gate_settle.output.implementation_head_sha }}"}}));
    }
    std::fs::write(&definition_path, serde_yaml::to_string(&job).unwrap()).unwrap();
    let mut run = f.runtime.insert_job_run("task_pr_pipeline", 1, Utc::now(),
        Some(json!({"task_ids":[f.task],"base_branch":"main","completion":"review","auto_fix":false})), None).unwrap();
    run.state = JobRunState::Running;
    run.pid = Some(std::process::id());
    published_owner::save_run(&f, &run);
    f.runtime
        .write_run_state(
            &run.run_id,
            &orbit_types::workflow::PipelineState::new(
                run.run_id.clone(),
                run.job_id.clone(),
                run.input.clone().unwrap(),
            ),
        )
        .unwrap();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":run.run_id,"run_id":run.run_id,
        "task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    published_owner::checkpoint(&f, &run.run_id, 0, "worktree", setup.clone());
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    f.branch = setup["head_ref"].as_str().unwrap().into();
    std::fs::write(
        checkout.join("qa-inventory.txt"),
        "inventory awaiting repair\n",
    )
    .unwrap();
    f.runtime.apply_task_automation_update(&f.task, TaskAutomationUpdate {
        execution_summary: Some("Outcome: success\nChanges: prepare candidate inventory.\nAssessment: host checks required.".into()),
        ..Default::default()
    }).unwrap();
    let commit = f.action(
        "git_commit",
        json!({"job_run_id":run.run_id,"scope":"all",
        "workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    published_owner::checkpoint(&f, &run.run_id, 1, "commit", commit);
    let old_head = git(&checkout, &["rev-parse", "HEAD"]);
    let validation = json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]});
    let (failed_index, failed_step, activity, error, parent) = if reviewer.is_some() {
        let parent = settle_and_publish_reviewer(&f, &run.run_id, &checkout, &setup, &validation);
        let open = json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"head":f.branch,"base":"main",
            "base_ref":setup["base_ref"],"base_sha":setup["base_sha"],
            "reviewed_head_sha":parent,"reviewed_base_sha":setup["base_sha"]});
        // The provider refuses this opening attempt; the terminal hook runs
        // after connectivity recovers and produces the real blocked receipt.
        let gh = f._root.path().join("bin/gh");
        let healthy = std::fs::read_to_string(&gh).unwrap();
        std::fs::write(
            &gh,
            healthy.replace(
                "echo https://github.com/owner/repository/pull/17",
                "echo 'provider unavailable' >&2; exit 1",
            ),
        )
        .unwrap();
        let error = execute_deterministic_action(
            &f.runtime,
            "pr_open",
            &json!({}),
            &open,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        std::fs::write(&gh, healthy).unwrap();
        (7, "pr_open", "pr_open", error, parent)
    } else {
        let error = execute_deterministic_action(
            &f.runtime,
            "candidate_validate",
            &json!({}),
            &validation,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("required validation"), "{error}");
        (8, "validate", "candidate_validate", error, old_head.clone())
    };
    let mut state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    state.record_step(
        failed_index,
        JobRunState::Failed,
        None,
        Some(json!(error.to_string())),
    );
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    f.source = run.clone();
    // Escalated permits the real terminal hook; direct Requeued suppresses it.
    match infra {
        None => escalate(&f, &run.run_id),
        Some(class) => {
            infra_escalate(&f, &run.run_id, failed_step, &checkout, class);
            state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
        }
    }
    mid_handoff(&f);
    if reviewer != Some(false) {
        std::fs::write(
            checkout.join("qa-fixed.txt"),
            "retained QA inventory repair\n",
        )
        .unwrap();
    }
    let handoff = f.action(
        "pr_failure_handoff",
        json!({"run_id":run.run_id,"failed_step_id":failed_step,
        "activity_name":activity,"error_code":"execution","error_message":error.to_string(),
        "job_input":run.input,"pipeline":state.pipeline}),
    );
    assert_eq!(handoff["decision"], "blocked_failure_pr");
    assert_eq!(
        handoff["preservation_commit_created"],
        reviewer != Some(false)
    );
    if reviewer == Some(false) {
        assert_eq!(handoff["head_sha"], parent);
    } else {
        assert_eq!(git(&checkout, &["rev-parse", "HEAD^"]), parent);
    }
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        failed_step.into(),
        handoff.clone(),
    );
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    f.head = handoff["head_sha"].as_str().unwrap().into();
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefName"] = json!(f.branch);
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    run = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    run.state = JobRunState::Failed;
    run.pid = None;
    run.finished_at = Some(Utc::now());
    published_owner::save_run(&f, &run);
    f.source = run;
    f.state = state;
    // Reconstruct from durable objects and remote identity, with no retained
    // worktree or branch. Git removes only this disposable fixture's worktree.
    git(&f.repo, &["worktree", "remove", checkout.to_str().unwrap()]);
    git(&f.repo, &["branch", "-D", &f.branch]);
    (f, parent)
}

fn failure_handoff_reconstructs_the_exact_repair_and_requires_fresh_gates() {
    let (f, old_head) = failure_handoff_fixture();
    git(&f.repo, &["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("new-base.txt"), "new current base\n").unwrap();
    git(&f.repo, &["add", "new-base.txt"]);
    git(
        &f.repo,
        &["commit", "-qm", "advance after stopped publication"],
    );
    git(&f.repo, &["push", "-q", "origin", "main"]);
    let current_base = git(&f.repo, &["rev-parse", "HEAD"]);
    let binding = f.inspect();
    assert_eq!(binding["head_sha"], f.head);
    assert_ne!(binding["head_sha"], old_head);
    let before = serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let input = f.submit(binding);
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input)
        .unwrap();
    assert_eq!(replay["run_id"], id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"run_id":id,
        "task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    assert_eq!(setup["base_sha"], current_base);
    assert!(checkout.join("new-base.txt").is_file());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":id,"task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"]}),
    );
    assert_eq!(resumed["source_sha"], f.head);
    assert_eq!(resumed["implement"], true);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(
        std::fs::read_to_string(checkout.join("qa-fixed.txt")).unwrap(),
        "retained QA inventory repair\n"
    );
    f.action(
        "git_commit",
        json!({"job_run_id":id,"scope":"all","workspace_path":checkout,
        "base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    let validated = f.action(
        "candidate_validate",
        json!({"job_run_id":id,"completed_task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    assert_eq!(validated["decision"], "passed");
    assert_eq!(validated["commands"].as_array().unwrap().len(), 1);
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        before
    );
}

pub(super) fn refused_without_writes(f: &Fixture, source: &str, binding: &Value, case: &str) {
    let task = serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap();
    let state = serde_json::to_value(f.runtime.read_run_state(source).unwrap()).unwrap();
    let history = serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap();
    let mut inspect = f.request("inspect");
    inspect["id"] = json!(source);
    let mut submit = f.submit(binding.clone());
    submit["id"] = json!(source);
    submit["source_repair"]["request_key"] = json!("refused-publication");
    for input in [inspect, submit] {
        let error = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", input)
            .err()
            .unwrap_or_else(|| panic!("{case}: unexpectedly admitted"));
        assert!(
            error.to_string().contains("source repair refused"),
            "{case}: {error}"
        );
    }
    assert_eq!(
        serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
        task,
        "{case}: task changed"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap(),
        history,
        "{case}: authority reserved"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(source).unwrap()).unwrap(),
        state,
        "{case}: receipt rewritten"
    );
}

fn failed_push_refusals_preserve_task_and_receipts() {
    let f = Fixture::with_policy("before-landing");
    let first = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(f.inspect()))
        .unwrap();
    let id = first["run_id"].as_str().unwrap();
    generations::publish_and_stop(&f, id, 1, true);
    escalate(&f, id);
    let mut inspect = f.request("inspect");
    inspect["id"] = json!(id);
    let binding = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", inspect)
        .unwrap()["binding"]
        .clone();
    let original = f.runtime.read_run_state(id).unwrap().unwrap();
    for (name, index) in [
        ("commit", 1),
        ("prepare_branch", 2),
        ("sync_base", 3),
        ("push", 4),
        ("resume_candidate", 6),
    ] {
        let mut state = original.clone();
        state.step_states.insert(index, JobRunState::Failed);
        f.runtime.write_run_state(id, &state).unwrap();
        refused_without_writes(&f, id, &binding, name);
    }
    for (name, field, value, index) in [
        ("push", "local_sha", json!(f.base), 4),
        ("sync_base", "base_sha", json!(f.head), 3),
        ("prepare_branch", "remote_sha", json!(f.base), 2),
        ("resume_candidate", "prior_pull_request", json!("18"), 6),
        ("resume_candidate", "source_sha", json!(f.base), 6),
    ] {
        let mut state = original.clone();
        state.pipeline[name][field] = value;
        state
            .step_outputs
            .insert(index, state.pipeline[name].clone());
        f.runtime.write_run_state(id, &state).unwrap();
        refused_without_writes(&f, id, &binding, field);
    }
    f.runtime.write_run_state(id, &original).unwrap();
    let run = f.runtime.show_job_run_observed(id).unwrap();
    for case in ["live", "foreign", "copied_admission"] {
        let mut changed = run.clone();
        match case {
            "live" => {
                changed.state = JobRunState::Running;
                changed.pid = Some(std::process::id());
                changed.finished_at = None;
            }
            "foreign" => changed.executed_on.as_mut().unwrap().machine_id = "hm_other".into(),
            "copied_admission" => {
                changed.input.as_mut().unwrap()["__source_repair"]["authorized_by"] =
                    json!("copied")
            }
            _ => unreachable!(),
        }
        published_owner::save_run(&f, &changed);
        // Ordinary upsert deliberately preserves the trusted execution origin;
        // model hostile persisted evidence explicitly in this isolated fixture.
        f.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                        [
                            serde_json::to_string(&changed.executed_on).unwrap(),
                            id.to_string(),
                        ],
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
        refused_without_writes(&f, id, &binding, case);
    }
    published_owner::save_run(&f, &run);
    f.runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                    [
                        serde_json::to_string(&run.executed_on).unwrap(),
                        id.to_string(),
                    ],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    provider_refusals(&f, id, &binding);
    let task = f.runtime.get_task(&f.task).unwrap();
    for scope in [false, true] {
        let params = if scope {
            orbit_core::application::task::TaskUpdateParams {
                context_files: Some(vec!["file:other-scope".into()]),
                ..Default::default()
            }
        } else {
            orbit_core::application::task::TaskUpdateParams {
                description: Some("changed mandate".into()),
                ..Default::default()
            }
        };
        f.runtime
            .update_task_with_identity(&f.task, params, None, Some("codex".into()))
            .unwrap();
        refused_without_writes(&f, id, &binding, "task meaning/scope");
        f.runtime
            .update_task_with_identity(
                &f.task,
                orbit_core::application::task::TaskUpdateParams {
                    description: Some(task.description.clone()),
                    context_files: Some(task.context_files.clone()),
                    ..Default::default()
                },
                None,
                Some("codex".into()),
            )
            .unwrap();
    }
}

pub(super) fn provider_refusals(f: &Fixture, id: &str, binding: &Value) {
    let original: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    for (field, value) in [
        ("headRefOid", json!(f.base)),
        ("headRefName", json!("other-branch")),
        ("baseRefName", json!("other-base")),
        ("number", json!(18)),
        ("url", json!("https://github.com/other/repo/pull/17")),
        ("state", json!("CLOSED")),
        ("autoMergeRequest", json!({"enabledAt":"now"})),
        ("mergedAt", json!("2026-01-01T00:00:00Z")),
    ] {
        let mut answer = original.clone();
        answer[field] = value;
        std::fs::write(&f.answer, answer.to_string()).unwrap();
        refused_without_writes(f, id, binding, field);
    }
    std::fs::write(&f.answer, original.to_string()).unwrap();
}

fn handoff_refusals_require_the_exact_durable_publication() {
    let (f, old_head) = failure_handoff_fixture();
    let binding = f.inspect();
    for field in [
        "task_id",
        "handoff_run_id",
        "checkpoint_owner",
        "task_spec_digest",
        "branch",
        "pr_number",
        "original_base_sha",
        "target_base_sha",
        "failed_step_id",
    ] {
        let mut state = f.state.clone();
        state.failure_activity_checkpoint.as_mut().unwrap().output[field] = json!("contradictory");
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
        refused_without_writes(&f, &f.source.run_id, &binding, field);
    }
    for case in [
        "missing",
        "wrong_activity",
        "wrong_step",
        "old_validation_head",
        "push",
        "parent",
    ] {
        let mut state = f.state.clone();
        let receipt = state.failure_activity_checkpoint.as_mut().unwrap();
        match case {
            "missing" => state.failure_activity_checkpoint = None,
            "wrong_activity" => receipt.activity_name = "agent_implement".into(),
            "wrong_step" => receipt.failed_step_id = "push".into(),
            "old_validation_head" => receipt.output["head_sha"] = json!(old_head),
            "push" => receipt.output["push"]["local_sha"] = json!(old_head),
            "parent" => receipt.output["preservation_commit_created"] = json!(false),
            _ => unreachable!(),
        }
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
        refused_without_writes(&f, &f.source.run_id, &binding, case);
    }
    f.runtime
        .write_run_state(&f.source.run_id, &f.state)
        .unwrap();
    provider_refusals(&f, &f.source.run_id, &binding);
    git(&f.repo, &["branch", &f.branch, &f.base]);
    refused_without_writes(
        &f,
        &f.source.run_id,
        &binding,
        "moved retained local branch",
    );
    git(&f.repo, &["branch", "-D", &f.branch]);
    git(
        &f.repo,
        &[
            "push",
            "-q",
            "--force",
            "origin",
            &format!("{}:refs/heads/{}", f.base, f.branch),
        ],
    );
    refused_without_writes(&f, &f.source.run_id, &binding, "remote lease drift");
}

fn settle_and_publish_reviewer(
    f: &Fixture,
    id: &str,
    checkout: &Path,
    setup: &Value,
    validation: &Value,
) -> String {
    use orbit_types::workflow::{
        REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewTiming,
    };
    let prepared = f.action(
        "pr_prepare",
        json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,
        "head":f.branch,"base":"main","base_ref":setup["base_ref"]}),
    );
    published_owner::checkpoint(f, id, 2, "prepare_branch", prepared.clone());
    let mut sync_input = prepared.clone();
    sync_input["job_run_id"] = json!(id);
    sync_input["workspace_path"] = json!(checkout);
    sync_input["completed_task_ids"] = json!([f.task]);
    sync_input["preparation"] = prepared;
    let sync = f.action("git_rebase", sync_input);
    published_owner::checkpoint(f, id, 3, "sync_base", sync.clone());
    let implementation = sync["head_sha"].clone();
    let mut run = f.runtime.show_job_run_observed(id).unwrap();
    let policy = f.runtime.operation_policy();
    run.input.as_mut().unwrap()["review"] = json!(ReviewAdmission {
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: policy.version,
        timing: ReviewTiming::BeforePr,
        timing_source: policy.review_before_pr.source.label().into(),
        crew: policy.review_crew.value.first().cloned(),
        crew_pool: Vec::new(),
        crew_source: policy.review_crew.source.label().into(),
        budget: policy.review_budget(),
        required_validation_commands: Some(
            f.runtime.workflow_required_validation_commands().to_vec()
        ),
        baseline_commands: f.runtime.review_baseline_commands().to_vec(),
        host_evidence: policy.review_host_evidence.value.clone(),
        captured_at: Utc::now(),
    });
    published_owner::save_run(f, &run);
    let mut gate = validation.clone();
    gate["base"] = json!("main");
    gate["base_sync"] = json!("remote");
    gate["mode"] = json!("pr");
    let admission = f.action("review_gate_admit", gate.clone());
    assert_eq!(admission["applies"], true);
    std::fs::write(checkout.join("review.txt"), "reviewer correction\n").unwrap();
    let report = json!({"schema_version":REVIEW_CONTRACT_VERSION,"attempt_id":admission["attempt_id"],
        "verdict":"accept_with_fixes","summary":"Correct the inventory note",
        "findings":[{"id":"F1","severity":"medium","summary":"Missing correction",
        "paths":["review.txt"],"disposition":"repaired","change":"Added correction"}],
        "validation":[{"id":"V1","command":"true","outcome":"passed","role":"required"}],"escalation":null});
    let path = f.repo.join(".orbit/tmp/reviewer-report.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, report.to_string()).unwrap();
    f.runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({"id":f.task,"model":"codex",
        "path":REVIEW_REPORT_ARTIFACT,"source_path":path}),
        )
        .unwrap();
    gate["admission"] = admission;
    let settled = f.action("review_gate_settle", gate);
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["reviewer_fixed"], true);
    assert_eq!(settled["implementation_head_sha"], implementation);
    published_owner::checkpoint(f, id, 4, "review_gate_settle", settled.clone());
    let mut input = validation.clone();
    input["ownership_base_sha"] = implementation;
    let validated = f.action("candidate_validate", input);
    assert_eq!(validated["tested_head"], settled["reviewed_head_sha"]);
    published_owner::checkpoint(f, id, 5, "review_validate", validated);
    let pushed = f.action(
        "git_push",
        json!({"job_run_id":id,"workspace_path":checkout,
        "branch":f.branch,"base":"main","base_ref":setup["base_ref"]}),
    );
    assert_eq!(pushed["local_sha"], settled["reviewed_head_sha"]);
    published_owner::checkpoint(f, id, 6, "push", pushed);
    settled["reviewed_head_sha"].as_str().unwrap().into()
}

fn validated_reviewer_handoffs_keep_identity_and_require_fresh_gates() {
    for preservation in [false, true] {
        let (f, reviewed) = handoff_fixture(Some(preservation));
        let before =
            serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
        let binding = f.inspect();
        assert_eq!(binding["head_sha"], f.head);
        assert_eq!(binding["branch"], f.branch);
        assert_eq!(binding["pull_request"], "17");
        if !preservation {
            assert_eq!(f.head, reviewed);
        }
        let result = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
            .unwrap();
        let id = result["run_id"].as_str().unwrap();
        f.wait(id);
        let run = f.runtime.show_job_run_observed(id).unwrap();
        assert_eq!(run.input.as_ref().unwrap()["completion"], "review");
        assert_eq!(run.input.as_ref().unwrap()["auto_fix"], false);
        f.start_actions(id);
        let setup = f.action(
            "worktree_setup",
            json!({"job_run_id":id,"run_id":id,
            "task_ids":[f.task],"base":"main","base_sync":"remote"}),
        );
        let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
        let resumed = f.action(
            "candidate_resume",
            json!({"job_run_id":id,"task_ids":[f.task],
            "workspace_path":checkout,"base_sha":setup["base_sha"]}),
        );
        assert_eq!(resumed["source_sha"], f.head);
        assert_eq!(resumed["reused_head_sha"], f.head);
        assert_eq!(resumed["implement"], true);
        assert_eq!(resumed["reused_branch"], f.branch);
        assert_eq!(resumed["prior_pull_request"], "17");
        assert_eq!(
            std::fs::read_to_string(checkout.join("review.txt")).unwrap(),
            "reviewer correction\n"
        );
        assert_eq!(checkout.join("qa-fixed.txt").exists(), preservation);
        let state = f.runtime.read_run_state(id).unwrap().unwrap();
        for step in ["review_gate_settle", "review_validate", "push", "pr_open"] {
            assert!(
                state.pipeline.get(step).is_none_or(Value::is_null),
                "fresh attempt reused {step}"
            );
        }
        f.action(
            "git_commit",
            json!({"job_run_id":id,"scope":"all","workspace_path":checkout,
            "base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
        );
        let validated = f.action(
            "candidate_validate",
            json!({"job_run_id":id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
        );
        assert_eq!(validated["decision"], "passed");
        let admitted = f.action("review_gate_admit", json!({"job_run_id":id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"base":"main","base_sync":"remote","mode":"pr","base_sha":setup["base_sha"]}));
        assert_eq!(admitted["applies"], true, "fresh review is required");
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().github_pr_number(),
            Some("17")
        );
        assert_eq!(
            serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
            before
        );
    }
}

crate::scenario_table!(
    validated_reviewer_handoff_scenarios,
    validated_reviewer_handoffs_keep_identity_and_require_fresh_gates
);

fn reviewer_handoffs_refuse_unproven_transitions_without_writes() {
    let (f, reviewed) = handoff_fixture(Some(true));
    let binding = f.inspect();
    let original = f.state.clone();
    for case in [
        "missing_settlement",
        "failed_settlement",
        "copied_settlement",
        "settlement_head",
        "settlement_base",
        "settlement_implementation",
        "settlement_fixed",
        "settlement_verdict",
        "missing_validation",
        "failed_validation",
        "copied_validation",
        "validation_head",
        "validation_branch",
        "validation_decision",
    ] {
        let mut state = original.clone();
        let (id, index) = if case.contains("settlement") {
            ("review_gate_settle", 4)
        } else {
            ("review_validate", 5)
        };
        match case {
            "missing_settlement" | "missing_validation" => {
                state.pipeline.as_object_mut().unwrap().remove(id);
            }
            "failed_settlement" | "failed_validation" => {
                state.step_states.insert(index, JobRunState::Failed);
            }
            "copied_settlement" | "copied_validation" => {
                state.step_outputs.insert(index, json!({}));
            }
            _ => {
                let (field, value) = match case {
                    "settlement_head" => ("reviewed_head_sha", json!(f.base)),
                    "settlement_base" => ("reviewed_base_sha", json!(reviewed)),
                    "settlement_implementation" => ("implementation_head_sha", json!(reviewed)),
                    "settlement_fixed" => ("reviewer_fixed", json!(false)),
                    "settlement_verdict" => ("gate", json!("incomplete")),
                    "validation_head" => (
                        "tested_head",
                        original.pipeline["sync_base"]["head_sha"].clone(),
                    ),
                    "validation_branch" => ("branch", json!("other")),
                    "validation_decision" => ("decision", json!("failed")),
                    _ => unreachable!(),
                };
                state.pipeline[id][field] = value;
                state.step_outputs.insert(index, state.pipeline[id].clone());
            }
        }
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
        refused_without_writes(&f, &f.source.run_id, &binding, case);
    }
    f.runtime
        .write_run_state(&f.source.run_id, &original)
        .unwrap();
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let clean_job = std::fs::read_to_string(&path).unwrap();
    for case in [
        "ownership_binding",
        "host_action",
        "review_order",
        "implementation_identity",
    ] {
        let mut job: Value = serde_yaml::from_str(&clean_job).unwrap();
        let steps = job["spec"]["steps"].as_array_mut().unwrap();
        let mut state = original.clone();
        match case {
            "ownership_binding" => steps[5]["default_input"]["ownership_base_sha"] = json!("HEAD"),
            "host_action" => steps[4]["target"] = json!("activity:git_commit"),
            "implementation_identity" => {
                let symbolic = format!(
                    "{}^{{commit}}",
                    original.pipeline["sync_base"]["head_sha"].as_str().unwrap()
                );
                state.pipeline["sync_base"]["head_sha"] = json!(symbolic);
                state.pipeline["sync_base"]["rewritten"] = json!(true);
                state.pipeline["review_gate_settle"]["implementation_head_sha"] = json!(symbolic);
                state
                    .step_outputs
                    .insert(3, state.pipeline["sync_base"].clone());
                state
                    .step_outputs
                    .insert(4, state.pipeline["review_gate_settle"].clone());
            }
            "review_order" => {
                steps.swap(4, 5);
                state
                    .step_outputs
                    .insert(4, original.pipeline["review_validate"].clone());
                state
                    .step_outputs
                    .insert(5, original.pipeline["review_gate_settle"].clone());
            }
            _ => unreachable!(),
        }
        std::fs::write(&path, serde_yaml::to_string(&job).unwrap()).unwrap();
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
        refused_without_writes(&f, &f.source.run_id, &binding, case);
    }
    std::fs::write(&path, clean_job).unwrap();
    f.runtime
        .write_run_state(&f.source.run_id, &original)
        .unwrap();
    provider_refusals(&f, &f.source.run_id, &binding);

    // Two extra commits cannot masquerade as the hook's single preservation
    // child, even with a matching remote head and internally consistent receipt.
    git(&f.repo, &["checkout", "-qb", &f.branch, &f.head]);
    std::fs::write(f.repo.join("unproven.txt"), "unproven descendant\n").unwrap();
    git(&f.repo, &["add", "unproven.txt"]);
    git(&f.repo, &["commit", "-qm", "unproven descendant"]);
    let descendant = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["push", "-q", "origin", &f.branch]);
    let mut state = original.clone();
    let output = &mut state.failure_activity_checkpoint.as_mut().unwrap().output;
    output["head_sha"] = json!(descendant);
    output["push"]["local_sha"] = json!(descendant);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(descendant);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    refused_without_writes(&f, &f.source.run_id, &binding, "arbitrary descendant");
}

fn escalated_completion_receipt_is_not_a_blocked_publication_receipt() {
    let mut f = Fixture::new();
    let binding = f.inspect();
    let checkout = published_owner::publication(&mut f);
    escalate(&f, &f.source.run_id);
    let output = f.action("pr_failure_handoff", json!({"run_id":f.source.run_id,
        "failed_step_id":"complete_pr","activity_name":"pr_complete","error_code":"execution",
        "error_message":"completion observation failed","job_input":f.source.input,"pipeline":f.state.pipeline}));
    assert_eq!(output["decision"], "review_completion_failure");
    assert_eq!(output["candidate_preserved"], true);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.record_step(
        7,
        JobRunState::Failed,
        None,
        Some(json!("completion observation failed")),
    );
    state.record_failure_activity("pr_failure_handoff".into(), "complete_pr".into(), output);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.source.state = JobRunState::Failed;
    f.source.pid = None;
    f.source.finished_at = Some(Utc::now());
    published_owner::save_run(&f, &f.source);
    // A real completion-preservation receipt has a distinct contract. It is
    // retained, never rewritten into a blocked-publication receipt for admission.
    refused_without_writes(&f, &f.source.run_id, &binding, "completion receipt variant");
}

crate::scenario_table!(
    reviewer_handoff_refusal_scenarios,
    reviewer_handoffs_refuse_unproven_transitions_without_writes,
    escalated_completion_receipt_is_not_a_blocked_publication_receipt
);
