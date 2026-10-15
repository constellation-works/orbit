//! A fully published repair becomes the next cancelled recovery source.
use super::*;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::workflow::JobTargetType;

// The fixture's compact definition keeps its original publication indices.
fn checkpoint(f: &Fixture, id: &str, step: &str, output: Value) {
    let index = match step {
        "worktree" => 0,
        "commit" => 1,
        "prepare_branch" => 2,
        "sync_base" => 3,
        "push" => 4,
        "pr_open" => 5,
        "resume_candidate" => 6,
        _ => panic!("unknown fixture checkpoint {step}"),
    };
    let mut state = f.runtime.read_run_state(id).unwrap().unwrap();
    state.record_step(index, JobRunState::Success, Some(output.clone()), None);
    state.record_pipeline_output(step, output);
    f.runtime.write_run_state(id, &state).unwrap();
    f.runtime
        .complete_job_run_step(
            id,
            &JobRunStepParams {
                step_index: index as usize,
                target_type: JobTargetType::Activity,
                target_id: step.into(),
                state: JobRunState::Success,
                started_at: Utc::now(),
                finished_at: Utc::now(),
                duration_ms: Some(1),
                exit_code: Some(0),
                lock_wait_ms: None,
                agent_response_json: None,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
}

fn publish_and_cancel(f: &Fixture, id: &str, generation: usize) -> String {
    f.wait(id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"run_id":id,"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    checkpoint(f, id, "worktree", setup.clone());
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action("candidate_resume", json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}));
    assert_ne!(setup["head_ref"], resumed["reused_branch"]);
    assert_eq!(resumed["reused_branch"], f.branch);
    checkpoint(f, id, "resume_candidate", resumed);
    std::fs::write(
        checkout.join("source.txt"),
        format!("repair generation {generation}\n"),
    )
    .unwrap();
    let commit = f.action("git_commit", json!({"job_run_id":id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    checkpoint(f, id, "commit", commit);
    let input = json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"head":f.branch,"branch":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]});
    let prepared = f.action("pr_prepare", input.clone());
    checkpoint(f, id, "prepare_branch", prepared.clone());
    let mut rebase_input = input.clone();
    for (key, value) in prepared.as_object().unwrap() {
        rebase_input[key] = value.clone();
    }
    let synced = f.action("git_rebase", rebase_input);
    checkpoint(f, id, "sync_base", synced);
    let validated = f.action("candidate_validate", input.clone());
    assert_eq!(validated["decision"], "passed");
    let pushed = f.action("git_push", input.clone());
    checkpoint(f, id, "push", pushed);
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", input.clone());
    assert_eq!(opened["pr_number"], "17");
    assert_eq!(opened["pr_reused"], true);
    checkpoint(f, id, "pr_open", opened);
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    let mut gate = input;
    gate["before_landing"] = json!(true);
    gate["mode"] = json!("pr");
    let admitted = f.action("review_gate_admit", gate);
    assert_eq!(admitted["applies"], true);
    // Deliberately stop before any reviewer commit. Preserve the actual host
    // checkpoints and task/run binding, unlike the earlier replay-only case.
    let mut run = f.runtime.show_job_run_observed(id).unwrap();
    run.state = JobRunState::Cancelled;
    run.finished_at = Some(Utc::now());
    run.pid = None;
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    let mut state = f.runtime.read_run_state(id).unwrap().unwrap();
    state.compact_for_terminal(JobRunState::Cancelled);
    f.runtime.write_run_state(id, &state).unwrap();
    head
}

fn inspect_source(f: &Fixture, id: &str) -> Value {
    let mut input = f.request("inspect");
    input["id"] = json!(id);
    input
}

fn assert_refused(f: &Fixture, id: &str, binding: &Value, case: &str) {
    let mut submit = f.submit(binding.clone());
    submit["id"] = json!(id);
    submit["source_repair"]["request_key"] = json!("changed-evidence");
    for input in [inspect_source(f, id), submit] {
        let error = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", input)
            .unwrap_err();
        assert!(
            error.to_string().contains("source repair refused"),
            "{case}: {error}"
        );
    }
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == "source_repair_reserved")
            .count(),
        1,
        "{case} must not reserve authority"
    );
}

fn changed_adoption_and_admission_refuse(f: &Fixture, id: &str, binding: &Value) {
    let state = f.runtime.read_run_state(id).unwrap().unwrap();
    for (field, value) in [
        ("reused_branch", json!("orbit/foreign")),
        ("reused_head_sha", json!(f.base)),
        ("source_run_id", json!(id)),
        ("source_sha", json!(f.base)),
        ("prior_pull_request", json!("18")),
        ("prior_pull_request_head", json!("foreign")),
        ("base_sha", json!(f.head)),
        ("implement", json!(false)),
        ("phase", json!("push")),
    ] {
        let mut changed = state.clone();
        changed.pipeline["resume_candidate"][field] = value;
        changed
            .step_outputs
            .insert(6, changed.pipeline["resume_candidate"].clone());
        f.runtime.write_run_state(id, &changed).unwrap();
        assert_refused(f, id, binding, field);
    }
    for case in ["missing", "failed", "copy"] {
        let mut changed = state.clone();
        match case {
            "missing" => {
                changed
                    .pipeline
                    .as_object_mut()
                    .unwrap()
                    .remove("resume_candidate");
            }
            "failed" => {
                changed.step_states.insert(6, JobRunState::Failed);
            }
            "copy" => {
                changed.step_outputs.insert(6, json!({}));
            }
            _ => unreachable!(),
        }
        f.runtime.write_run_state(id, &changed).unwrap();
        assert_refused(f, id, binding, case);
    }
    f.runtime.write_run_state(id, &state).unwrap();
    let run = f.runtime.show_job_run_observed(id).unwrap();
    for case in [
        "admission",
        "task",
        "prior_run",
        "pr",
        "completion",
        "retry",
    ] {
        let mut changed = run.clone();
        let input = changed.input.as_mut().unwrap();
        match case {
            "admission" => input["__source_repair"]["authorized_by"] = json!("other-operator"),
            "task" => input["__source_repair"]["binding"]["task_id"] = json!("other-task"),
            "prior_run" => input["__source_repair"]["binding"]["source_run_id"] = json!(id),
            "pr" => input["__source_repair"]["binding"]["pull_request"] = json!("18"),
            "completion" => input["completion"] = json!("done"),
            "retry" => changed.retry_source_run_id = Some(id.into()),
            _ => unreachable!(),
        }
        f.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &changed, None)
            .unwrap();
        assert_refused(f, id, binding, case);
    }
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    // Task ownership is still exact after a successful adoption, and the
    // superseded original source cannot authorize a fresh request either.
    assert!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
            .is_err()
    );
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                job_run_id: Some(f.source.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_refused(f, id, binding, "task binding");
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                job_run_id: Some(id.into()),
                ..Default::default()
            },
        )
        .unwrap();
}

pub(super) fn cancelled_published_repair_accepts_fresh_generation() {
    let f = Fixture::with_policy("before-landing");
    let original = f.submit(f.inspect());
    let first = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", original.clone())
        .unwrap();
    let id = first["run_id"].as_str().unwrap();
    let head = publish_and_cancel(&f, id, 1);
    assert_ne!(head, f.head);
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", original)
        .unwrap();
    assert_eq!(replay["run_id"], id);
    assert_eq!(replay["state"], "cancelled");
    let inspected = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", inspect_source(&f, id))
        .unwrap();
    assert_eq!(inspected["binding"]["head_sha"], head);
    assert_eq!(inspected["binding"]["branch"], f.branch);
    assert_eq!(inspected["binding"]["pull_request"], "17");
    changed_adoption_and_admission_refuse(&f, id, &inspected["binding"]);
    // One automatic fix and two explicit generations share the same budget;
    // cancelling either generation must never refund its reservation.
    f.runtime.apply_task_automation_update(&f.task, TaskAutomationUpdate {
        append_history: vec![orbit_types::task::TaskHistoryEntry {
            at: Utc::now(), by: "system".into(), event: "pr_auto_fix_admitted".into(),
            note: Some(json!({"pr_number":"17","head_sha":f.base,"blocker":"failed_checks","attempt":1,"budget":3,"run_id":"earlier-automatic-fix","crew":"fixture","crew_source":"fixture"}).to_string()),
            from_status: None, to_status: None, operation_id: None, decision: None,
        }], ..Default::default()
    }).unwrap();
    let mut submit = f.submit(inspected["binding"].clone());
    submit["id"] = json!(id);
    submit["source_repair"]["request_key"] = json!("repair-2");
    let second = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit.clone())
        .unwrap();
    assert_ne!(second["run_id"], id);
    assert_eq!(second["retry_source_run_id"], id);
    let second_id = second["run_id"].as_str().unwrap();
    let second_head = publish_and_cancel(&f, second_id, 2);
    assert_ne!(head, second_head);
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit)
        .unwrap();
    assert_eq!(replay["run_id"], second_id);
    assert_eq!(replay["state"], "cancelled");
    let inspected = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", inspect_source(&f, second_id))
        .unwrap();
    assert_eq!(inspected["binding"]["head_sha"], second_head);
    let mut third = f.submit(inspected["binding"].clone());
    third["id"] = json!(second_id);
    third["source_repair"]["request_key"] = json!("repair-3");
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", third)
        .unwrap_err();
    assert!(error.to_string().contains("budget exhausted"), "{error}");
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == "source_repair_reserved")
            .count(),
        2
    );
    // A conflicting durable link must not let an otherwise intact admission
    // authenticate a different run. Append-only history is never repaired here.
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: "source_repair_linked".into(),
                    note: Some(json!({"request_key":"repair-2","run_id":id}).to_string()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", inspect_source(&f, second_id))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("prior repair admission or run link changed"),
        "{error}"
    );
    f.assert_original();
}
