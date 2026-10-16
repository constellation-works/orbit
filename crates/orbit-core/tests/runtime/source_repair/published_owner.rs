//! Public lifecycle regression: publication, observation failure, requeue, adoption.
use super::*;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use orbit_types::workflow::{JobRun, PipelineState};

pub(super) fn save_run(f: &Fixture, run: &JobRun) {
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), run, None)
        .unwrap();
}

pub(super) fn checkpoint(f: &Fixture, run: &str, index: u32, id: &str, output: Value) {
    let mut state = f.runtime.read_run_state(run).unwrap().unwrap();
    state.record_step(index, JobRunState::Success, Some(output.clone()), None);
    state.record_pipeline_output(id, output);
    f.runtime.write_run_state(run, &state).unwrap();
    f.runtime
        .complete_job_run_step(
            run,
            &orbit_store::contracts::JobRunStepParams {
                step_index: index as usize,
                target_type: orbit_types::workflow::JobTargetType::Activity,
                target_id: id.into(),
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

/// Drive real host commit, push and PR opening, using a local Git forge and
/// a deterministic GitHub process adapter. No load or external service is used.
fn publication(f: &mut Fixture) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let gh = f._root.path().join("bin/gh");
    let created = f._root.path().join("created");
    let denied = f._root.path().join("observation-denied");
    std::fs::write(&gh, format!("#!/bin/sh\ncase \"$1 $2\" in\n 'pr list') if [ -f '{}' ]; then printf '['; cat '{}'; printf ']'; else printf '[]'; fi;;\n 'pr create') touch '{}'; echo https://github.com/owner/repository/pull/17;;\n 'pr view') if [ -f '{}' ]; then echo 'TLS handshake timeout' >&2; exit 1; fi; cat '{}';;\n *) exit 99;;\nesac\n", created.display(), f.answer.display(), created.display(), denied.display(), f.answer.display())).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Replace the fixture's illustrative historical run with one whose host
    // actions actually produce every candidate/publication checkpoint.
    let mut run = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids":[f.task],"completion":"done","auto_fix":false})),
            None,
        )
        .unwrap();
    run.state = JobRunState::Running;
    run.pid = Some(std::process::id());
    run.started_at = Some(Utc::now());
    save_run(f, &run);
    f.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(
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
        json!({"job_run_id":run.run_id,"run_id":run.run_id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    checkpoint(f, &run.run_id, 0, "worktree", setup.clone());
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    std::fs::write(
        checkout.join("source.txt"),
        "complete published candidate\n",
    )
    .unwrap();
    f.runtime.apply_task_automation_update(&f.task, TaskAutomationUpdate { execution_summary:Some("Outcome: success\nChanges: complete the source repair and preserve its public behavior.\nAssessment: candidate ready for host validation.".into()), ..Default::default() }).unwrap();
    let commit = f.action("git_commit", json!({"job_run_id":run.run_id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    checkpoint(f, &run.run_id, 1, "commit", commit);
    let prepared = f.action("pr_prepare", json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],"base":"main","base_sync":"remote"}));
    checkpoint(f, &run.run_id, 2, "prepare_branch", prepared.clone());
    let mut sync_input = prepared.clone();
    sync_input["job_run_id"] = json!(run.run_id);
    sync_input["workspace_path"] = json!(checkout);
    sync_input["completed_task_ids"] = json!([f.task]);
    sync_input["preparation"] = prepared;
    let sync = f.action("git_rebase", sync_input);
    checkpoint(f, &run.run_id, 3, "sync_base", sync);
    f.branch = git(&checkout, &["branch", "--show-current"]);
    f.head = git(&checkout, &["rev-parse", "HEAD"]);
    let push = f.action("git_push", json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    checkpoint(f, &run.run_id, 4, "push", push);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefName"] = json!(f.branch);
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]}));
    assert_eq!(opened["pr_created"], true);
    checkpoint(f, &run.run_id, 5, "pr_open", opened);
    // A completion observation loses connectivity after the actual publication.
    std::fs::write(&denied, "offline").unwrap();
    let failure = orbit_engine::test_support::with_vcs_retry_wait(
        |_| {},
        || {
            execute_deterministic_action(&f.runtime, "pr_complete", &json!({}),
            &json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,
                "pr_number":"17","completion":"done","auto_fix":false,"head":f.branch,
                "published_head_sha":f.head,"base":"main","max_wait_seconds":0}),
            false, &HashMap::new(), None).unwrap_err()
        },
    );
    assert!(
        failure.to_string().contains("TLS handshake timeout"),
        "{failure}"
    );
    std::fs::rename(&denied, denied.with_extension("recovered")).unwrap();
    assert_eq!(
        f.runtime
            .run_private_vcs_operation("pr.status", json!({"pr":"17","workspace_path":f.repo}))
            .unwrap()["pull_request"]["headRefOid"],
        json!(f.head),
        "final recovery verifies connectivity and the existing PR"
    );
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    f.state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    checkout
}

fn requeue(f: &mut Fixture) {
    let observed = f
        .runtime
        .final_recovery_revision(&f.runtime.get_task(&f.task).unwrap())
        .unwrap();
    let request = FinalRecoveryRequest {
        task_id: f.task.clone(),
        run_id: f.source.run_id.clone(),
        observed,
        repo_root: f.repo.clone(),
        base_ref: "origin/main".into(),
        completion: FinalRecoveryCompletion::Review,
        requeue_bound: Default::default(),
    };
    let output =
        json!({"decision":"requeue","reason":"provider observation connectivity recovered"});
    let barrier = std::sync::Barrier::new(2);
    let runtime = &f.runtime;
    std::thread::scope(|scope| {
        let apply = || {
            barrier.wait();
            runtime
                .apply_final_recovery(&request, Some(&output))
                .unwrap()
        };
        let first = scope.spawn(apply);
        let second = scope.spawn(apply);
        assert_eq!(first.join().unwrap(), FinalRecoveryOutcome::Requeued);
        assert_eq!(second.join().unwrap(), FinalRecoveryOutcome::Requeued);
    });
    assert_eq!(
        f.runtime
            .apply_final_recovery(&request, Some(&output))
            .unwrap(),
        FinalRecoveryOutcome::Requeued,
        "lost reply replays the same durable decision"
    );
    f.source.state = JobRunState::Failed;
    f.source.pid = None;
    f.source.finished_at = Some(Utc::now());
    save_run(f, &f.source);
    f.state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    assert!(f.state.failure_activity_checkpoint.is_none());
}

fn next(f: &Fixture) -> (JobRun, Value) {
    let mut run = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids":[f.task]})),
            None,
        )
        .unwrap();
    run.state = JobRunState::Running;
    run.pid = Some(std::process::id());
    run.started_at = Some(Utc::now());
    save_run(f, &run);
    f.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(
                run.run_id.clone(),
                run.job_id.clone(),
                run.input.clone().unwrap(),
            ),
        )
        .unwrap();
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":run.run_id,"run_id":run.run_id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    checkpoint(f, &run.run_id, 0, "worktree", setup.clone());
    (run, setup)
}

pub(super) fn published_completion_requeue_adopts_without_reimplementation() {
    let mut f = Fixture::new();
    let original_checkout = publication(&mut f);
    requeue(&mut f);
    let before = serde_json::to_value(&f.state).unwrap();
    let (run, setup) = next(&f);
    let resumed = f.action("candidate_resume", json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}));
    assert_eq!(resumed["outcome"], "resumed_carried");
    assert_eq!(resumed["implement"], false);
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == "published_owner_requeued")
            .count(),
        1
    );
    checkpoint(&f, &run.run_id, 6, "resume_candidate", resumed);
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    assert!(
        git(&original_checkout, &["branch", "--show-current"])
            .starts_with(&format!("{}-superseded-", f.branch)),
        "the old checkout and its head remain intact"
    );
    assert_eq!(git(&original_checkout, &["rev-parse", "HEAD"]), f.head);
    let commit = f.action("git_commit", json!({"job_run_id":run.run_id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    assert_eq!(commit["committed"], true);
    let validated = f.action("candidate_validate", json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    assert_eq!(validated["decision"], "passed");
    let pushed = f.action("git_push", json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    assert_eq!(pushed["branch"], f.branch);
    assert_eq!(
        pushed["remote_sha_before"], f.head,
        "publication retains the original branch lease even when the commit is unchanged"
    );
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(git(&checkout, &["rev-parse", "HEAD"]));
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]}));
    assert_eq!(opened["pr_reused"], true);
    assert_eq!(opened["pr_number"], "17");
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap()).unwrap(),
        before,
        "publication/review/execution evidence is immutable"
    );
}

pub(super) fn failed_requeued_publisher_is_inspected_and_submitted_without_a_successor() {
    let mut f = Fixture::new();
    publication(&mut f);
    requeue(&mut f);

    let task = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(task.status, TaskStatus::Backlog);
    assert_eq!(task.job_run_id.as_deref(), Some(f.source.run_id.as_str()));

    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    let binding = inspection["binding"].clone();
    assert_eq!(binding["source_run_id"], f.source.run_id);
    assert!(binding["superseded"].is_null());
    let still_directly_bound = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(still_directly_bound.status, TaskStatus::Backlog);
    assert_eq!(
        still_directly_bound.job_run_id.as_deref(),
        Some(f.source.run_id.as_str()),
        "inspection must not require or create a successor"
    );

    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
        .unwrap();
    assert_eq!(admitted["retry_source_run_id"], f.source.run_id);
    assert_ne!(admitted["run_id"], f.source.run_id);
    let run_id = admitted["run_id"].as_str().unwrap();
    f.wait(run_id);
    assert_eq!(
        f.runtime
            .show_job_run_observed(run_id)
            .unwrap()
            .retry_source_run_id
            .as_deref(),
        Some(f.source.run_id.as_str())
    );
    assert!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "source_repair_reserved")
    );
}

pub(super) fn failed_requeued_direct_publisher_retains_authority_and_drift_refusals() {
    for case in ["live", "foreign", "changed", "unacknowledged"] {
        let mut f = Fixture::new();
        publication(&mut f);

        if case == "unacknowledged" {
            f.source.state = JobRunState::Failed;
            f.source.pid = None;
            f.source.finished_at = Some(Utc::now());
            save_run(&f, &f.source);
            f.runtime
                .apply_task_automation_update(
                    &f.task,
                    TaskAutomationUpdate {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                )
                .unwrap();
        } else {
            requeue(&mut f);
            match case {
                "live" => {
                    f.source.pid = Some(std::process::id());
                    save_run(&f, &f.source);
                }
                "foreign" => {
                    f.runtime
                        .sqlite_store()
                        .unwrap()
                        .with_transaction(|tx| {
                            tx.connection()
                                .execute(
                                    "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                                    [
                                        json!({"machine_id":"hm_foreign","machine_name":null})
                                            .to_string(),
                                        f.source.run_id.clone(),
                                    ],
                                )
                                .unwrap();
                            Ok(())
                        })
                        .unwrap();
                }
                "changed" => {}
                _ => unreachable!(),
            }
        }

        if case == "changed" {
            let binding = f.inspect();
            f.runtime
                .update_task_with_identity(
                    &f.task,
                    orbit_core::application::task::TaskUpdateParams {
                        description: Some("changed after source inspection".into()),
                        ..Default::default()
                    },
                    None,
                    Some("codex".into()),
                )
                .unwrap();
            let refused = f
                .runtime
                .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
                .unwrap_err();
            assert!(
                refused.to_string().contains("binding is stale"),
                "{refused}"
            );
        } else {
            let refused = f
                .runtime
                .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
                .unwrap_err();
            assert!(refused.to_string().contains("refused"), "{case}: {refused}");
        }
        assert!(
            !f.runtime
                .get_task_history(&f.task)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "source_repair_reserved")
        );
    }
}

pub(super) fn published_requeue_refuses_changed_scope_head_and_incomplete_source() {
    for case in ["scope", "head", "before_commit", "lost_checkpoint"] {
        let mut f = Fixture::new();
        publication(&mut f);
        requeue(&mut f);
        match case {
            "scope" => {
                f.runtime
                    .update_task_with_identity(
                        &f.task,
                        orbit_core::application::task::TaskUpdateParams {
                            description: Some("changed mandate".into()),
                            ..Default::default()
                        },
                        None,
                        Some("codex".into()),
                    )
                    .unwrap();
            }
            "head" => {
                let mut answer: Value =
                    serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
                answer["headRefOid"] = json!(f.base);
                std::fs::write(&f.answer, answer.to_string()).unwrap();
            }
            "before_commit" => {
                f.state.pipeline.as_object_mut().unwrap().remove("commit");
                f.state.step_states.insert(1, JobRunState::Failed);
                f.state.step_outputs.remove(&1);
                f.runtime
                    .write_run_state(&f.source.run_id, &f.state)
                    .unwrap();
            }
            "lost_checkpoint" => {
                f.state.step_states.insert(5, JobRunState::Failed);
                f.runtime
                    .write_run_state(&f.source.run_id, &f.state)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (run, setup) = next(&f);
        let error = execute_deterministic_action(&f.runtime,"candidate_resume",&json!({}),&json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],"prior_job_run_id":setup["prior_job_run_id"]}),false,&HashMap::new(),None).unwrap_err();
        assert!(error.to_string().contains("refused"), "{case}: {error}");
        assert_eq!(
            git(
                &PathBuf::from(setup["workspace_path"].as_str().unwrap()),
                &["status", "--porcelain"]
            ),
            "",
            "refusal occurs before replacement implementation"
        );
    }
}

pub(super) fn stopped_successor_is_explicitly_bound_when_repairing_older_publication() {
    let mut f = Fixture::new();
    publication(&mut f);
    requeue(&mut f);
    let source_before =
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let (mut successor, setup) = next(&f);
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    std::fs::write(checkout.join("retained.txt"), "interrupted newer work\n").unwrap();
    successor.state = JobRunState::Cancelled;
    successor.pid = None;
    successor.finished_at = Some(Utc::now());
    save_run(&f, &successor);
    let binding = f.inspect();
    assert_eq!(binding["superseded"]["run_id"], successor.run_id);
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
    assert_eq!(
        replay["run_id"], id,
        "unknown reply cannot admit a second recovery"
    );
    f.start_actions(id);
    let repair_setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let resume_input = json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":repair_setup["workspace_path"],"base_sha":repair_setup["base_sha"]});
    let resumed = f.action("candidate_resume", resume_input.clone());
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    assert_eq!(resumed["prior_pull_request"], "17");
    let successor_state = f
        .runtime
        .read_run_state(&successor.run_id)
        .unwrap()
        .unwrap();
    let mut changed = successor_state.clone();
    changed.step_states.insert(1, JobRunState::Running);
    f.runtime
        .write_run_state(&successor.run_id, &changed)
        .unwrap();
    let refused = execute_deterministic_action(
        &f.runtime,
        "candidate_resume",
        &json!({}),
        &resume_input,
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        refused.to_string().contains("candidate/publication action"),
        "an attempt before checkpointing invalidates the inspected binding: {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("retained.txt")).unwrap(),
        "interrupted newer work\n"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        source_before
    );
}

pub(super) fn legacy_publication_behind_a_stopped_successor_cannot_start_fresh() {
    // A pre-preservation publication has host evidence and a task PR reference,
    // but no new preservation event. A stopped fresh successor must not hide it.
    let f = Fixture::new();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
    let (mut successor, _) = next(&f);
    successor.state = JobRunState::Cancelled;
    successor.pid = None;
    successor.finished_at = Some(Utc::now());
    save_run(&f, &successor);
    let binding = f.inspect();
    assert_eq!(binding["source_run_id"], f.source.run_id);
    assert_eq!(binding["superseded"]["run_id"], successor.run_id);
    let (incoming, setup) = next(&f);
    let error = execute_deterministic_action(&f.runtime,"candidate_resume",&json!({}),&json!({"job_run_id":incoming.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],"prior_job_run_id":setup["prior_job_run_id"]}),false,&HashMap::new(),None).unwrap_err();
    assert!(
        error.to_string().contains("explicit source repair"),
        "known legacy publication requires explicit source selection: {error}"
    );
}
