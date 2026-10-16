//! Stopped failed publications receive fresh authority through the public tool.
use super::*;
use orbit_core::application::task::{FinalRecoveryCompletion, FinalRecoveryRequest};

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
    f.runtime.apply_final_recovery(&request, Some(&json!({"decision":"escalate",
        "diagnosis":"publication needs explicit source repair", "human_action":"inspect retained host publication"}))).unwrap();
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
    let mut f = Fixture::handoff();
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
    let mut state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    state.record_step(8, JobRunState::Failed, None, Some(json!(error.to_string())));
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    // Recovery repaired the inventory after that failed check. The hook commits
    // the repair, so the original validation head must never be selected.
    std::fs::write(
        checkout.join("qa-fixed.txt"),
        "retained QA inventory repair\n",
    )
    .unwrap();
    let handoff = f.action("pr_failure_handoff", json!({"run_id":run.run_id,"failed_step_id":"validate",
        "activity_name":"candidate_validate","error_code":"execution","error_message":error.to_string(),
        "job_input":run.input,"pipeline":state.pipeline}));
    assert_eq!(handoff["decision"], "blocked_failure_pr");
    assert_eq!(handoff["preservation_commit_created"], true);
    assert_ne!(handoff["head_sha"], old_head);
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        "validate".into(),
        handoff.clone(),
    );
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    f.head = handoff["head_sha"].as_str().unwrap().into();
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefName"] = json!(f.branch);
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    run.state = JobRunState::Failed;
    run.pid = None;
    run.finished_at = Some(Utc::now());
    published_owner::save_run(&f, &run);
    f.source = run;
    f.state = state;
    escalate(&f, &f.source.run_id);
    // Reconstruct from durable objects and remote identity, with no retained
    // worktree or branch. Git removes only this disposable fixture's worktree.
    git(&f.repo, &["worktree", "remove", checkout.to_str().unwrap()]);
    git(&f.repo, &["branch", "-D", &f.branch]);
    (f, old_head)
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

fn refused_without_writes(f: &Fixture, source: &str, binding: &Value, case: &str) {
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

fn provider_refusals(f: &Fixture, id: &str, binding: &Value) {
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
