//! The stopped inline-publication/red-check/expired-budget recovery dead-end.
use super::failed_publication::{escalate, escalated_completion_fixture};
use super::published_owner::{next, save_run};
use super::*;
use orbit_types::workflow::JobRun;

/// Actual publication and failure hook, followed by an expired completion
/// retry and a fresh attempt refused at adoption. All processes are local.
fn incident() -> (Fixture, JobRun) {
    let (mut f, checkout, _) = escalated_completion_fixture();
    let forge = super::escalated_publication::green_forge(&mut f);
    forge.set(&f.head, &["help-entrypoint"], &[], &["Test"]);
    let red = super::handoff_requeue::requeue(&f, false).unwrap_err();
    assert!(red.to_string().contains("required checks failed"), "{red}");
    f.state.completion_deadlines.insert("17".into(), 1);
    f.runtime
        .write_run_state(&f.source.run_id, &f.state)
        .unwrap();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    let retry = f
        .runtime
        .submit_resume_run(&f.source.run_id, Some("operator"), None)
        .unwrap();
    f.wait(&retry.run_id);
    f.start_actions(&retry.run_id);
    let expired = execute_deterministic_action(&f.runtime, "pr_complete", &json!({}),
        &json!({"job_run_id":f.source.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,
            "pr_number":"17","completion":"done","auto_fix":true,"head":f.branch,
            "published_head_sha":f.head,"base":"main","max_wait_seconds":60}),
        false, &HashMap::new(), None).unwrap_err();
    assert!(
        expired.to_string().contains("deadline expired"),
        "{expired}"
    );
    let mut stopped = f.runtime.show_job_run_observed(&retry.run_id).unwrap();
    stopped.state = JobRunState::Held;
    stopped.pid = None;
    stopped.finished_at = Some(Utc::now());
    save_run(&f, &stopped);

    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
    let (mut empty, setup) = next(&f);
    let refused = execute_deterministic_action(&f.runtime, "candidate_resume", &json!({}),
        &json!({"job_run_id":empty.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],
            "base_sha":setup["base_sha"],"prior_job_run_id":f.source.run_id}),
        false, &HashMap::new(), None).unwrap_err();
    assert!(
        refused.to_string().contains("unique requeue binding"),
        "{refused}"
    );
    let mut state = f.runtime.read_run_state(&empty.run_id).unwrap().unwrap();
    state.record_step(
        6,
        JobRunState::Failed,
        None,
        Some(json!(refused.to_string())),
    );
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        "resume_candidate".into(),
        json!({"phase":"failure_handoff","decision":"no_candidate"}),
    );
    f.runtime.write_run_state(&empty.run_id, &state).unwrap();
    empty.state = JobRunState::Failed;
    empty.pid = None;
    empty.finished_at = Some(Utc::now());
    save_run(&f, &empty);
    escalate(&f, &empty.run_id);
    (f, stopped)
}

fn expired_inline_publication_gets_fresh_same_pr_repair() {
    let (mut f, retry) = incident();
    let config = f.repo.join(".orbit/config.toml");
    let policy = std::fs::read_to_string(&config)
        .unwrap()
        .replace("before_landing = false", "before_landing = true");
    std::fs::write(config, policy).unwrap();
    reload(&mut f);
    let original = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let retry_before = f.runtime.read_run_state(&retry.run_id).unwrap().unwrap();
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    assert_eq!(inspection["binding"]["source_run_id"], f.source.run_id);
    assert_eq!(inspection["binding"]["head_sha"], f.head);
    assert!(inspection["policy_digest"].is_string());
    let mut input = f.submit(inspection["binding"].clone());
    input["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
    input["source_repair"]["unmet_acceptance"] = json!(
        "Hosted help-entrypoint check failed on this published head; completion budget expired before repair."
    );
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    let run = f.runtime.show_job_run_observed(id).unwrap();
    let admitted = run.input.as_ref().unwrap();
    assert_eq!(admitted["completion"], "review");
    assert_eq!(admitted["auto_fix"], false);
    assert_eq!(admitted["review"], admitted["__source_repair"]["review"]);
    assert_eq!(admitted["review"]["timing"], "before-landing");
    assert_eq!(
        admitted["review"]["required_validation_commands"],
        json!(["true"])
    );
    let fresh = f.runtime.read_run_state(id).unwrap().unwrap();
    assert!(fresh.step_states.is_empty());
    assert!(fresh.completion_deadlines.is_empty());
    for field in [
        "request_key",
        "diagnosis",
        "unmet_acceptance",
        "policy_digest",
    ] {
        let mut conflict = input.clone();
        conflict["source_repair"][field] = json!("conflicting-request");
        assert!(
            f.runtime
                .run_tool_as_human("orbit.workflow.run.resume", conflict)
                .is_err(),
            "{field}"
        );
    }
    let repaired = super::generations::publish_and_cancel(&f, id, 1);
    assert_ne!(repaired, f.head);
    let resumed =
        f.runtime.read_run_state(id).unwrap().unwrap().pipeline["resume_candidate"].clone();
    assert_eq!(resumed["implement"], true);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");
    let log = f
        .runtime
        .get_task_artifact(&f.task, &format!("validation/{id}/0.json"))
        .unwrap()
        .unwrap();
    let log: Value = serde_json::from_slice(&log.content).unwrap();
    assert_eq!(log["tested_head"], repaired);
    assert_eq!(log["run_id"], id);
    assert_eq!(log["exit_code"], 0);
    let calls = std::fs::read_to_string(f._root.path().join("gh-calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("pr create "))
            .count(),
        1
    );
    assert!(!calls.lines().any(|line| line.starts_with("pr merge ")));
    assert_eq!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", input)
            .unwrap()["run_id"],
        id
    );
    assert_eq!(
        f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap(),
        original
    );
    assert_eq!(
        f.runtime.read_run_state(&retry.run_id).unwrap().unwrap(),
        retry_before
    );
}

fn reload(f: &mut Fixture) {
    f.runtime = orbit_core::OrbitRuntime::from_roots(
        &f._root.path().join("global"),
        &f.repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
        machine_id: "hm_fixture".into(),
        machine_name: None,
    }));
}

fn rework(f: &Fixture) -> Value {
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    let mut input = f.submit(inspection["binding"].clone());
    input["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
    input["source_repair"]["unmet_acceptance"] = json!(
        "The published help-entrypoint check is red; repair the same candidate after its completion budget expired."
    );
    input
}

fn refused_without_effects(f: &Fixture, input: Value, case: &str) -> String {
    let snapshot = || {
        json!({"task":f.runtime.get_task(&f.task).unwrap(),
        "history":f.runtime.get_task_history(&f.task).unwrap(),
        "source":f.runtime.read_run_state(&f.source.run_id).unwrap(),
        "refs":git(&f.repo, &["ls-remote", "origin"])})
    };
    let before = snapshot();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input)
        .unwrap_err()
        .to_string();
    assert_eq!(
        snapshot(),
        before,
        "{case}: refusal must precede source effects or reservation"
    );
    error
}

fn inline_publication_refuses_changed_proof_scope_and_lineage() {
    let (f, retry) = incident();
    let request = rework(&f);
    let original = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    for field in [
        "phase",
        "decision",
        "task_id",
        "handoff_run_id",
        "task_spec_digest",
        "failed_step_id",
        "pr_number",
        "pr_url",
        "candidate_preserved",
    ] {
        let mut changed = original.clone();
        changed.failure_activity_checkpoint.as_mut().unwrap().output[field] =
            json!("contradictory");
        f.runtime
            .write_run_state(&f.source.run_id, &changed)
            .unwrap();
        refused_without_effects(&f, f.request("inspect"), field);
        refused_without_effects(&f, request.clone(), field);
        f.runtime
            .write_run_state(&f.source.run_id, &original)
            .unwrap();
    }
    for case in [
        "missing receipt",
        "missing PR",
        "failed push",
        "copied push",
        "different PR",
    ] {
        let mut changed = original.clone();
        match case {
            "missing receipt" => changed.failure_activity_checkpoint = None,
            "missing PR" => {
                changed.pipeline.as_object_mut().unwrap().remove("pr_open");
            }
            "failed push" => {
                changed.step_states.insert(4, JobRunState::Failed);
            }
            "copied push" => {
                changed.step_outputs.insert(4, json!({}));
            }
            "different PR" => {
                changed.pipeline["pr_open"]["pr_number"] = json!("18");
                changed
                    .step_outputs
                    .insert(5, changed.pipeline["pr_open"].clone());
            }
            _ => unreachable!(),
        }
        f.runtime
            .write_run_state(&f.source.run_id, &changed)
            .unwrap();
        refused_without_effects(&f, f.request("inspect"), case);
        f.runtime
            .write_run_state(&f.source.run_id, &original)
            .unwrap();
    }
    let answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    for (field, value) in [
        ("headRefOid", json!(f.base)),
        ("headRefName", json!("other")),
        ("baseRefName", json!("other")),
        ("number", json!(18)),
        (
            "url",
            json!("https://github.com/foreign/repository/pull/17"),
        ),
        ("autoMergeRequest", json!({"enabledAt":"now"})),
    ] {
        let mut changed = answer.clone();
        changed[field] = value;
        std::fs::write(&f.answer, changed.to_string()).unwrap();
        refused_without_effects(&f, f.request("inspect"), field);
        std::fs::write(&f.answer, answer.to_string()).unwrap();
    }
    let task = f.runtime.get_task(&f.task).unwrap();
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some("A different scope".into()),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    refused_without_effects(&f, f.request("inspect"), "changed specification");
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some(task.description),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    f.inspect();

    let current = f
        .runtime
        .show_job_run_observed(task.job_run_id.as_deref().unwrap())
        .unwrap();
    for member in [&f.source, &retry, &current] {
        for case in ["active", "claimed", "unfinished"] {
            let mut changed = member.clone();
            match case {
                "active" => changed.pid = Some(std::process::id()),
                "claimed" => changed.input.as_mut().unwrap()["claimed"] = json!(true),
                "unfinished" => changed.finished_at = None,
                _ => unreachable!(),
            }
            save_run(&f, &changed);
            refused_without_effects(&f, f.request("inspect"), case);
            save_run(&f, member);
        }
        let origin = member.executed_on.as_ref().unwrap();
        let set_origin = |machine: &str| {
            f.runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute(
                            "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                            [
                                json!({"machine_id":machine,"machine_name":null}).to_string(),
                                member.run_id.clone(),
                            ],
                        )
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        };
        set_origin("hm_foreign");
        refused_without_effects(&f, f.request("inspect"), "foreign lineage member");
        set_origin(&origin.machine_id);
    }
    let retry_state = f.runtime.read_run_state(&retry.run_id).unwrap().unwrap();
    for case in [
        "changed deadline",
        "different prefix",
        "missing prefix",
        "rebase recovery",
    ] {
        let mut changed = retry_state.clone();
        match case {
            "changed deadline" => {
                changed.completion_deadlines.insert("17".into(), 2);
            }
            "different prefix" => {
                changed.pipeline["push"]["local_sha"] = json!(f.base);
            }
            "missing prefix" => {
                changed.pipeline.as_object_mut().unwrap().remove("push");
            }
            "rebase recovery" => {
                changed
                    .rebase_recovery_checkpoints
                    .insert("complete_pr".into(), json!({"head":f.base}));
            }
            _ => unreachable!(),
        }
        f.runtime.write_run_state(&retry.run_id, &changed).unwrap();
        refused_without_effects(&f, f.request("inspect"), case);
        f.runtime
            .write_run_state(&retry.run_id, &retry_state)
            .unwrap();
    }
    let mut changed = f.runtime.read_run_state(&current.run_id).unwrap().unwrap();
    let empty = changed.clone();
    changed.record_pipeline_output("pr_open", json!({"pr_number":"18"}));
    f.runtime
        .write_run_state(&current.run_id, &changed)
        .unwrap();
    refused_without_effects(&f, f.request("inspect"), "ambiguous newer publication");
    f.runtime.write_run_state(&current.run_id, &empty).unwrap();
    let retained = PathBuf::from(
        original.pipeline["worktree"]["workspace_path"]
            .as_str()
            .unwrap(),
    )
    .join("retained.txt");
    std::fs::write(&retained, "unfinished repair\n").unwrap();
    refused_without_effects(&f, f.request("inspect"), "uncommitted original candidate");
    std::fs::remove_file(retained).unwrap();
    f.inspect();
}

fn inline_repair_rechecks_policy_and_stopped_lineage_before_source_effects() {
    let (mut f, mut retry) = incident();
    let request = rework(&f);
    for field in ["unmet_acceptance", "policy_digest"] {
        let mut missing = request.clone();
        missing["source_repair"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        refused_without_effects(&f, missing, field);
    }
    let config = f.repo.join(".orbit/config.toml");
    let original_policy = std::fs::read_to_string(&config).unwrap();
    let changed_policy = original_policy.replace(
        "required_validation_commands = [\"true\"]",
        "required_validation_commands = [\"false\"]",
    );
    std::fs::write(&config, &changed_policy).unwrap();
    reload(&mut f);
    let error = refused_without_effects(&f, request.clone(), "stale policy before admission");
    assert!(error.contains("policy_digest"), "{error}");
    std::fs::write(&config, &original_policy).unwrap();
    reload(&mut f);
    let request = rework(&f);
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request)
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let input = json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]});
    let refuse_adoption = |f: &Fixture| {
        execute_deterministic_action(
            &f.runtime,
            "candidate_resume",
            &json!({}),
            &input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err()
        .to_string()
    };
    let original_retry = retry.clone();
    retry.duration_ms = Some(42);
    save_run(&f, &retry);
    let error = refuse_adoption(&f);
    assert!(error.contains("newer execution changed"), "{error}");
    save_run(&f, &original_retry);
    std::fs::write(&config, changed_policy).unwrap();
    reload(&mut f);
    let error = refuse_adoption(&f);
    assert!(error.contains("current policy"), "{error}");
    assert_eq!(
        git(
            PathBuf::from(setup["workspace_path"].as_str().unwrap()).as_path(),
            &["branch", "--show-current"]
        ),
        setup["head_ref"].as_str().unwrap()
    );
}

crate::scenario_table!(
    inline_publication_scenarios,
    expired_inline_publication_gets_fresh_same_pr_repair,
    inline_publication_refuses_changed_proof_scope_and_lineage,
    inline_repair_rechecks_policy_and_stopped_lineage_before_source_effects
);
