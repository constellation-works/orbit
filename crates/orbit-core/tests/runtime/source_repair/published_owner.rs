//! Public lifecycle regression: publication, observation failure, requeue, adoption.
use super::*;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use orbit_types::workflow::{JobRun, PipelineState};
use std::path::Path;

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
pub(super) fn publication(f: &mut Fixture) -> PathBuf {
    publication_inner(f, false, false, false)
}

pub(super) fn validated_publication(f: &mut Fixture, auto_fix: bool) -> PathBuf {
    publication_inner(f, true, auto_fix, false)
}

fn publication_inner(
    f: &mut Fixture,
    validated: bool,
    auto_fix: bool,
    successful: bool,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let gh = f._root.path().join("bin/gh");
    let created = f._root.path().join("created");
    let denied = f._root.path().join("observation-denied");
    std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1 $2\" in\n 'pr list') if [ -f '{}' ]; then printf '['; cat '{}'; printf ']'; else printf '[]'; fi;;\n 'pr create') touch '{}'; echo https://github.com/owner/repository/pull/17;;\n 'pr view') if [ -f '{}' ]; then echo 'TLS handshake timeout' >&2; exit 1; fi; cat '{}';;\n *) exit 99;;\nesac\n", f._root.path().join("gh-calls").display(), created.display(), f.answer.display(), created.display(), denied.display(), f.answer.display())).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Replace the fixture's illustrative historical run with one whose host
    // actions actually produce every candidate/publication checkpoint.
    let mut run = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids":[f.task],"completion":if successful {"review"} else {"done"},"auto_fix":auto_fix})),
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
    if validated {
        // Reproduce routine preparation against a base that advanced after setup.
        git(&f.repo, &["checkout", "main"]);
        std::fs::write(
            f.repo.join("preparation-base.txt"),
            "advance before publication\n",
        )
        .unwrap();
        git(&f.repo, &["add", "preparation-base.txt"]);
        git(&f.repo, &["commit", "-qm", "advance before preparation"]);
        git(&f.repo, &["push", "origin", "main"]);
        f.base = git(&f.repo, &["rev-parse", "HEAD"]);
    }
    let prepared = f.action("pr_prepare", json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],"base":"main","base_sync":"remote","authenticate_preparation":validated}));
    checkpoint(f, &run.run_id, 2, "prepare_branch", prepared.clone());
    let mut sync_input = prepared.clone();
    sync_input["job_run_id"] = json!(run.run_id);
    sync_input["step_id"] = json!("sync_base");
    sync_input["workspace_path"] = json!(checkout);
    sync_input["completed_task_ids"] = json!([f.task]);
    sync_input["preparation"] = prepared;
    let sync = f.action("git_rebase", sync_input);
    checkpoint(f, &run.run_id, 3, "sync_base", sync);
    f.branch = git(&checkout, &["branch", "--show-current"]);
    f.head = git(&checkout, &["rev-parse", "HEAD"]);
    if validated || successful {
        let validation = f.action("candidate_validate", json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":f.base}));
        let expected = if f.runtime.workflow_required_validation_commands().is_empty() {
            "skipped_no_required_commands"
        } else {
            "passed"
        };
        assert_eq!(validation["decision"], expected);
        if validated {
            checkpoint(f, &run.run_id, 4, "validate", validation);
            let settled = f.action("review_gate_settle", json!({"admission":{"applies":false}}));
            checkpoint(f, &run.run_id, 5, "review_gate_settle", settled);
        }
    }
    let push = f.action("git_push", json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    checkpoint(f, &run.run_id, if validated { 6 } else { 4 }, "push", push);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefName"] = json!(f.branch);
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":f.base}));
    assert_eq!(opened["pr_created"], true);
    checkpoint(
        f,
        &run.run_id,
        if validated { 7 } else { 5 },
        "pr_open",
        opened,
    );
    if validated {
        checkpoint(f, &run.run_id, 8, "resume_candidate", Value::Null);
        let promoted = f.action("pr_promote", json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":f.base,"pr_number":"17","pr_url":"https://github.com/owner/repository/pull/17"}));
        checkpoint(f, &run.run_id, 9, "promote", promoted);
    }
    if successful {
        // As in the review-mode pipeline, promotion finishes the delivery
        // without asking for a merge or running the done-only completion step.
        f.action("pr_promote", json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"],"pr_number":"17","pr_url":"https://github.com/owner/repository/pull/17"}));
        run.state = JobRunState::Success;
        run.pid = None;
        run.finished_at = Some(Utc::now());
        run.duration_ms = Some(
            (run.finished_at.unwrap() - run.started_at.unwrap())
                .num_milliseconds()
                .try_into()
                .unwrap(),
        );
        save_run(f, &run);
        f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
        f.state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
        return checkout;
    }
    // A completion observation loses connectivity after the actual publication.
    std::fs::write(&denied, "offline").unwrap();
    let failure = orbit_engine::test_support::with_vcs_retry_wait(
        |_| {},
        || {
            execute_deterministic_action(&f.runtime, "pr_complete", &json!({}),
            &json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,
                "pr_number":"17","completion":"done","auto_fix":auto_fix,"head":f.branch,
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

pub(super) fn requeue(f: &mut Fixture) {
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

pub(super) fn next(f: &Fixture) -> (JobRun, Value) {
    next_with(f, json!({"task_ids":[f.task]}))
}

fn next_with(f: &Fixture, input: Value) -> (JobRun, Value) {
    let mut run = f
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input), None)
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

fn rework_request(f: &Fixture) -> Value {
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    let mut request = f.submit(inspection["binding"].clone());
    request["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
    request["source_repair"]["diagnosis"] = json!(
        "Reduce fixture work under the current bounded source specification; preserve this PR."
    );
    request["source_repair"]["unmet_acceptance"] = json!(
        "Published head's complete hosted Check took 1473 seconds (run 42, job 7), exceeding its 900-second acceptance. The existing green correctness result does not meet that criterion."
    );
    request
}

fn successful_publication_requires_fresh_same_pr_implementation() {
    let mut f = Fixture::with_policy("before-landing");
    let original_checkout = publication_inner(&mut f, false, false, true);
    let original = serde_json::to_value(&f.source).unwrap();
    let original_log_path = format!("validation/{}/0.json", f.source.run_id);
    let original_validation = f
        .runtime
        .get_task_artifact(&f.task, &original_log_path)
        .unwrap()
        .unwrap()
        .content;
    assert_eq!(
        serde_json::from_slice::<Value>(&original_validation).unwrap()["exit_code"],
        0
    );
    let original_head = f.head.clone();
    let original_branch = f.branch.clone();
    // A current specification is necessary input, but does not itself admit
    // another worker. Only the explicit operator request below grants that.
    f.runtime.update_task_with_identity(&f.task,
        orbit_core::application::task::TaskUpdateParams {
            description: Some("Bounded current specification: reduce redundant fixture work while preserving critical behavioral coverage.".into()),
            ..Default::default()
        }, None, Some("codex".into())).unwrap();
    assert!(
        f.runtime
            .submit_resume_run(&f.source.run_id, Some("operator"), None)
            .is_err()
    );
    assert!(
        f.runtime
            .owner_published_candidate(&f.task, Some(&f.source.run_id))
            .is_err(),
        "automatic reuse cannot authorize rework of a successful publication"
    );
    let request = rework_request(&f);
    let mut implicit = request.clone();
    implicit["source_repair"]
        .as_object_mut()
        .unwrap()
        .remove("unmet_acceptance");
    assert!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", implicit)
            .is_err()
    );

    let barrier = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let submit = || {
            barrier.wait();
            f.runtime
                .run_tool_as_human("orbit.workflow.run.resume", request.clone())
                .unwrap()
        };
        let a = scope.spawn(submit);
        let b = scope.spawn(submit);
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(
        first["run_id"], second["run_id"],
        "concurrent identical requests admit one continuation"
    );
    let id = first["run_id"].as_str().unwrap();
    f.wait(id);
    let run = f.runtime.show_job_run_observed(id).unwrap();
    let input = run.input.as_ref().unwrap();
    assert_eq!(input["completion"], "review");
    assert_eq!(input["auto_fix"], false);
    assert_eq!(input["review"]["timing"], "before-landing");
    assert_eq!(
        input["review"]["required_validation_commands"],
        json!(["true"])
    );
    assert!(
        input["__source_repair"]["diagnosis"]
            .as_str()
            .unwrap()
            .contains("1473")
    );
    assert!(
        f.runtime
            .read_run_state(id)
            .unwrap()
            .unwrap()
            .step_states
            .is_empty(),
        "successful source gates and checkpoints are never inherited"
    );

    for field in ["request_key", "unmet_acceptance", "policy_digest"] {
        let mut divergent = request.clone();
        divergent["source_repair"][field] = json!("divergent");
        assert!(
            f.runtime
                .run_tool_as_human("orbit.workflow.run.resume", divergent)
                .is_err(),
            "{field} cannot duplicate or change a durable authorization"
        );
    }
    let repaired = super::generations::publish_and_stop(&f, id, 1, false);
    assert_ne!(
        repaired, original_head,
        "implementation produces a fresh candidate"
    );
    let adopted =
        f.runtime.read_run_state(id).unwrap().unwrap().pipeline["resume_candidate"].clone();
    assert_eq!(adopted["implement"], true);
    assert_eq!(adopted["reused_branch"], original_branch);
    assert_eq!(adopted["prior_pull_request"], "17");
    let artifact = f
        .runtime
        .get_task_artifact(&f.task, &format!("validation/{id}/0.json"))
        .unwrap()
        .unwrap();
    let validation: Value = serde_json::from_slice(&artifact.content).unwrap();
    assert_eq!(validation["tested_head"], repaired);
    assert_eq!(validation["exit_code"], 0);
    assert_eq!(
        validation["run_id"], id,
        "native evidence belongs to the fresh attempt"
    );
    assert_eq!(
        f.runtime
            .get_task_artifact(&f.task, &original_log_path)
            .unwrap()
            .unwrap()
            .content,
        original_validation,
        "previous green correctness evidence is preserved rather than rewritten as rework acceptance"
    );
    let calls = std::fs::read_to_string(f._root.path().join("gh-calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("pr create "))
            .count(),
        1,
        "the rework reuses the original PR and never creates a replacement"
    );
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request)
        .unwrap();
    assert_eq!(replay["run_id"], id);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        serde_json::to_value(f.runtime.show_job_run_observed(&f.source.run_id).unwrap()).unwrap(),
        original,
        "the original successful execution and timing observations remain immutable"
    );
    assert_eq!(
        git(&original_checkout, &["rev-parse", "HEAD"]),
        original_head
    );
    f.assert_original();

    // A successful repair publication is itself a valid source for a later
    // bounded specification. Its historical admission is provenance, while
    // the new operator decision must bind the current task meaning afresh.
    let mut successful = f.runtime.show_job_run_observed(id).unwrap();
    successful.state = JobRunState::Success;
    successful.duration_ms = Some(1);
    save_run(&f, &successful);
    f.source = successful;
    f.head = repaired;
    f.state = f.runtime.read_run_state(id).unwrap().unwrap();
    f.runtime.update_task_with_identity(&f.task,
        orbit_core::application::task::TaskUpdateParams {
            description: Some("Second bounded specification after a successful same-PR publication; preserve the original provenance.".into()),
            ..Default::default()
        }, None, Some("codex".into())).unwrap();
    let mut next_request = rework_request(&f);
    next_request["source_repair"]["request_key"] = json!("successful-generation-2");
    let next = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", next_request)
        .unwrap();
    let next_id = next["run_id"].as_str().unwrap();
    f.wait(next_id);
    assert_ne!(next_id, id);
    assert!(
        f.runtime
            .read_run_state(next_id)
            .unwrap()
            .unwrap()
            .step_states
            .is_empty()
    );
    f.assert_original();
}

fn successful_rework_refuses_changed_authority_scope_and_publication() {
    for case in [
        "evidence",
        "policy",
        "scope",
        "live",
        "foreign",
        "claimed",
        "merged",
        "head",
        "branch",
        "lease",
        "checkpoint",
        "superseded",
        "terminal",
        "source",
    ] {
        let mut f = Fixture::new();
        publication_inner(&mut f, false, false, true);
        let mut request = rework_request(&f);
        match case {
            "evidence" => request["source_repair"]["unmet_acceptance"] = json!("  "),
            "policy" => request["source_repair"]["policy_digest"] = json!("obsolete"),
            "source" => request["source_repair"]["binding"]["source_digest"] = json!("forged"),
            "scope" => {
                f.runtime
                    .update_task_with_identity(
                        &f.task,
                        orbit_core::application::task::TaskUpdateParams {
                            description: Some("unapproved change since inspection".into()),
                            ..Default::default()
                        },
                        None,
                        Some("codex".into()),
                    )
                    .unwrap();
            }
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
            "claimed" => {
                f.source.input.as_mut().unwrap()["claimed"] = json!(true);
                save_run(&f, &f.source);
            }
            "merged" | "head" | "branch" => {
                let mut answer: Value =
                    serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
                match case {
                    "merged" => {
                        answer["state"] = json!("MERGED");
                        answer["mergeCommit"] = json!({"oid":f.head});
                    }
                    "head" => answer["headRefOid"] = json!(f.base),
                    "branch" => answer["headRefName"] = json!("foreign"),
                    _ => unreachable!(),
                }
                std::fs::write(&f.answer, answer.to_string()).unwrap();
            }
            "lease" => {
                git(
                    &f.repo,
                    &[
                        "push",
                        "--force",
                        "origin",
                        &format!("main:refs/heads/{}", f.branch),
                    ],
                );
            }
            "checkpoint" => {
                f.state.pipeline["push"]["local_sha"] = json!(f.base);
                f.runtime
                    .write_run_state(&f.source.run_id, &f.state)
                    .unwrap();
            }
            "superseded" => {
                let mut newer = f
                    .runtime
                    .insert_job_run(
                        "task_pr_pipeline",
                        2,
                        Utc::now(),
                        Some(json!({"task_ids":[f.task]})),
                        None,
                    )
                    .unwrap();
                newer.state = JobRunState::Cancelled;
                newer.finished_at = Some(Utc::now());
                save_run(&f, &newer);
                f.runtime
                    .apply_task_automation_update(
                        &f.task,
                        TaskAutomationUpdate {
                            job_run_id: Some(newer.run_id),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            "terminal" => {
                f.runtime
                    .apply_task_automation_update(
                        &f.task,
                        TaskAutomationUpdate {
                            status: Some(TaskStatus::Done),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let error = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", request)
            .unwrap_err();
        assert!(error.to_string().contains("refused"), "{case}: {error}");
        assert!(
            !f.runtime
                .get_task_history(&f.task)
                .unwrap()
                .iter()
                .any(|e| e.event == "source_repair_reserved"),
            "{case} must refuse before reserving another worker"
        );
    }
}

fn successful_rework_reconciles_unknown_spawn_reply() {
    let mut f = Fixture::new();
    publication_inner(&mut f, false, false, true);
    let request = rework_request(&f);
    orbit_core::test_support::install_substitute_pipeline_worker([f
        ._root
        .path()
        .join("missing-worker")
        .to_string_lossy()
        .into_owned()]);
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request.clone())
        .unwrap_err();
    assert!(
        error.to_string().contains("spawn pipeline worker"),
        "{error}"
    );
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let first = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request.clone())
        .unwrap();
    let second = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request)
        .unwrap();
    assert_eq!(first["run_id"], second["run_id"]);
    assert_eq!(first["replayed"], true);
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == "source_repair_reserved")
            .count(),
        1
    );
    f.assert_original();
}

fn successful_rework_binds_current_policy_before_submission_and_source_effects() {
    let mut f = Fixture::with_policy("before-landing");
    publication_inner(&mut f, false, false, true);
    let old_request = rework_request(&f);
    let config = f.repo.join(".orbit/config.toml");
    let changed = std::fs::read_to_string(&config).unwrap().replace(
        "before_landing = true",
        "before_landing = false\nbefore_pr = true",
    );
    std::fs::write(&config, changed).unwrap();
    let reload = |f: &mut Fixture| {
        f.runtime = orbit_core::OrbitRuntime::from_roots(
            &f._root.path().join("global"),
            &f.repo.join(".orbit"),
        )
        .unwrap()
        .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
            machine_id: "hm_fixture".into(),
            machine_name: None,
        }));
    };
    reload(&mut f);
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", old_request)
        .unwrap_err();
    assert!(error.to_string().contains("policy_digest"), "{error}");
    assert!(
        !f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|e| e.event == "source_repair_reserved")
    );
    let request = rework_request(&f);
    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request)
        .unwrap();
    let id = admitted["run_id"].as_str().unwrap();
    f.wait(id);
    assert_eq!(
        f.runtime.show_job_run_observed(id).unwrap().input.unwrap()["review"]["timing"],
        "before-pr"
    );
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let changed = std::fs::read_to_string(&config).unwrap().replace(
        "required_validation_commands = [\"true\"]",
        "required_validation_commands = [\"false\"]",
    );
    std::fs::write(&config, changed).unwrap();
    reload(&mut f);
    let error = execute_deterministic_action(&f.runtime, "candidate_resume", &json!({}),
        &json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}),
        false, &HashMap::new(), None).unwrap_err();
    assert!(error.to_string().contains("current policy"), "{error}");
    assert_eq!(
        git(
            PathBuf::from(setup["workspace_path"].as_str().unwrap()).as_path(),
            &["branch", "--show-current"]
        ),
        setup["head_ref"].as_str().unwrap(),
        "policy drift refuses before adopting or modifying the published branch"
    );
    f.assert_original();
}

crate::scenario_table!(
    successful_source_rework_scenarios,
    successful_publication_requires_fresh_same_pr_implementation,
    successful_rework_refuses_changed_authority_scope_and_publication,
    successful_rework_reconciles_unknown_spawn_reply,
    successful_rework_binds_current_policy_before_submission_and_source_effects,
);

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

/// Commit `content` on the published branch in its retained checkout and push
/// it, as Orbit's auto-fix does, and point the PR at the new head.
fn move_published_branch(f: &Fixture, checkout: &Path, content: &str) -> String {
    std::fs::write(checkout.join("auto-fix.txt"), content).unwrap();
    git(checkout, &["add", "auto-fix.txt"]);
    git(checkout, &["commit", "-qm", "repair the failed check"]);
    git(checkout, &["push", "-q", "origin", &f.branch]);
    let moved = git(checkout, &["rev-parse", "HEAD"]);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(moved);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    moved
}

/// Record a repaired head published on the PR, as the completion watcher does.
fn record_auto_fix_push(f: &Fixture, by: &str, from: &str, to: &str, run_id: &str) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: by.into(),
                    event: orbit_types::workflow::PR_AUTO_FIX_PUSHED_EVENT.into(),
                    note: Some(
                        json!({"pr_number":"17","from_head_sha":from,"to_head_sha":to,
                            "blocker":"failed_checks","run_id":run_id})
                        .to_string(),
                    ),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

/// A requeue that preserved its publication is admitted past another task's
/// lock on its own file, its gate defers the footprint, and its next run
/// adopts and reuses the existing PR. The lock is taken before any agent that
/// may edit, and only once the other task releases it.
pub(super) fn published_requeue_is_admitted_past_held_locks_and_reuses_its_pr() {
    let mut f = Fixture::drained();
    publication(&mut f);
    requeue(&mut f);
    let holder = f
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Overlapping work in review".into(),
            description: "Changes the same file".into(),
            acceptance_criteria: vec!["Observable".into()],
            plan: "Fixture".into(),
            context_files: vec!["file:source.txt".into()],
            complexity: orbit_core::TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap()
        .id;
    let set_status = |id: &str, status| {
        f.runtime
            .apply_task_automation_update(
                id,
                TaskAutomationUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    set_status(&holder, TaskStatus::Review);

    let readiness = f
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == f.task)
        .unwrap();
    assert_eq!(
        (&entry["eligible"], &entry["reason"]),
        (&json!(true), &json!("ready")),
        "the held lock does not hold back the published delivery: {entry}"
    );
    assert_eq!(
        entry["published_delivery"]["source_run_id"],
        f.source.run_id
    );
    assert_eq!(entry["published_delivery"]["pull_request"], "17");

    let reserve = |input: Value| {
        f.runtime
            .run_deterministic("reserve_locks", &json!({}), &input, Default::default())
            .unwrap()
    };
    let local = reserve(
        json!({"task_ids":[f.task],"ttl_seconds":120,"delivery_job":"task_local_pipeline"}),
    );
    assert_eq!(
        (&local["reserved"], &local["footprint_deferred"]),
        (&json!(false), &json!(false)),
        "another delivery job waits on the lock as before: {local}"
    );
    let gate =
        reserve(json!({"task_ids":[f.task],"ttl_seconds":120,"delivery_job":"task_pr_pipeline"}));
    assert_eq!(gate["reserved"], true, "{gate}");
    assert_eq!(gate["footprint_deferred"], true);
    assert_eq!(gate["reserved_files"], json!([]), "the grant holds no file");
    assert_eq!(gate["deferred_conflicts"][0]["held_by_id"], holder);

    let (run, setup) = next_with(
        &f,
        json!({"task_ids":[f.task],"footprint_deferred":true,"footprint_wait_seconds":0}),
    );
    let resumed = f.action("candidate_resume", json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}));
    assert_eq!(resumed["outcome"], "resumed_carried");
    assert_eq!(resumed["implement"], false);
    assert_eq!(resumed["prior_pull_request"], "17");
    checkpoint(&f, &run.run_id, 6, "resume_candidate", resumed);

    let refused = f
        .runtime
        .acquire_deferred_footprint(&run.run_id, "agent_implement")
        .unwrap_err();
    assert!(
        refused.to_string().contains("footprint_unavailable")
            && refused.to_string().contains(&holder),
        "an agent that may edit waits for the holder, and names it: {refused}"
    );
    f.runtime
        .acquire_deferred_footprint(&run.run_id, "final_recovery")
        .expect("a decision-only recovery edits nothing and takes no lock");
    set_status(&holder, TaskStatus::Done);
    f.runtime
        .acquire_deferred_footprint(&run.run_id, "agent_implement")
        .expect("the footprint is taken once the holder releases it");
    set_status(&holder, TaskStatus::Backlog);
    let blocked = reserve(json!({"task_ids":[holder],"ttl_seconds":120}));
    assert_eq!(blocked["reserved"], false);
    assert!(
        blocked["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|conflict| conflict["held_by"] == "reservation"),
        "the run's reservation holds the file against another writer: {blocked}"
    );
    f.runtime
        .acquire_deferred_footprint(&run.run_id, "agent_review_repair")
        .expect("a run already holding its footprint reserves nothing again");

    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let commit = f.action("git_commit", json!({"job_run_id":run.run_id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    assert_eq!(commit["committed"], true);
    let pushed = f.action("git_push", json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    assert_eq!(pushed["remote_sha_before"], f.head);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(git(&checkout, &["rev-parse", "HEAD"]));
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]}));
    assert_eq!(opened["pr_reused"], true);
    assert_eq!(opened["pr_number"], "17");
}

/// Two resumed runs that deferred the same footprint behind a third task each
/// hold their task `in-progress` and neither holds a file. Once the holder
/// releases, one run takes the footprint and the other is refused by that
/// reservation alone, and takes it when the first releases — neither waits
/// for the other to time out.
pub(super) fn deferred_resumed_runs_behind_one_holder_take_the_footprint_in_turn() {
    let f = Fixture::drained();
    let add = |title: &str| {
        f.runtime
            .add_task(orbit_core::application::task::TaskAddParams {
                title: title.into(),
                description: "Changes the same file".into(),
                acceptance_criteria: vec!["Observable".into()],
                plan: "Fixture".into(),
                context_files: vec!["file:source.txt".into()],
                complexity: orbit_core::TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
    };
    let set_status = |id: &str, status| {
        f.runtime
            .apply_task_automation_update(
                id,
                TaskAutomationUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    let holder = add("Overlapping work in review");
    let (first, second) = (f.task.clone(), add("Second resumed publication"));
    set_status(&holder, TaskStatus::Review);
    set_status(&first, TaskStatus::InProgress);
    set_status(&second, TaskStatus::InProgress);
    let deferred_run = |task: &str| {
        let mut run = f
            .runtime
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(
                    json!({"task_ids":[task],"footprint_deferred":true,"footprint_wait_seconds":0}),
                ),
                None,
            )
            .unwrap();
        run.state = JobRunState::Running;
        run.pid = Some(std::process::id());
        run.started_at = Some(Utc::now());
        save_run(&f, &run);
        run.run_id
    };
    let (first_run, second_run) = (deferred_run(&first), deferred_run(&second));
    let acquire = |run: &str| f.runtime.acquire_deferred_footprint(run, "agent_implement");

    for run in [&first_run, &second_run] {
        let refused = acquire(run).unwrap_err().to_string();
        assert!(
            refused.contains("footprint_unavailable") && refused.contains(&holder),
            "the holder still holds the footprint: {refused}"
        );
    }
    set_status(&holder, TaskStatus::Done);

    acquire(&first_run).expect("the other waiter holds no file, so it is no holder");
    let refused = acquire(&second_run).unwrap_err().to_string();
    assert!(
        refused.contains("footprint_unavailable") && refused.contains(&first),
        "an in-progress writer that holds the footprint excludes the second run: {refused}"
    );
    // With no status holder left, the writer's reservation alone excludes it.
    set_status(&first, TaskStatus::Backlog);
    let refused = acquire(&second_run).unwrap_err().to_string();
    assert!(
        refused.contains("footprint_unavailable") && refused.contains("reservation-"),
        "the first run's file reservation excludes the second run: {refused}"
    );

    let locks = f
        .runtime
        .run_tool_as_human("orbit.task.locks", json!({}))
        .unwrap();
    let reservation = locks["by_reservation"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["owner_run_id"] == first_run.as_str())
        .expect("the first run holds a reservation with files");
    assert_eq!(reservation["files"], json!(["file:source.txt"]));
    let released = f
        .runtime
        .run_tool(
            "orbit.task.locks.release",
            json!({"reservation_id": reservation["reservation_id"], "model": "codex"}),
        )
        .unwrap();
    assert_eq!(released["released"], true);
    acquire(&second_run).expect("the second run takes the footprint once the first releases it");
}

/// Orbit's own auto-fix moved the published branch before the requeue. The
/// head it recorded publishing is adopted; the next run carries it.
pub(super) fn auto_fix_advanced_publication_is_adopted_at_its_recorded_head() {
    let mut f = Fixture::new();
    let original_checkout = publication(&mut f);
    let published = f.head.clone();
    let repaired = move_published_branch(&f, &original_checkout, "repaired check\n");
    record_auto_fix_push(&f, "system", &published, &repaired, &f.source.run_id);
    requeue(&mut f);
    let (run, setup) = next(&f);
    let resumed = f.action("candidate_resume", json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}));
    assert_eq!(resumed["outcome"], "resumed_carried");
    assert_eq!(resumed["reused_head_sha"], repaired);
    checkpoint(&f, &run.run_id, 6, "resume_candidate", resumed);
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(checkout.join("auto-fix.txt")).unwrap(),
        "repaired check\n",
        "the carried candidate includes the recorded repair"
    );
    let commit = f.action("git_commit", json!({"job_run_id":run.run_id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    assert_eq!(commit["committed"], true);
    let pushed = f.action("git_push", json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    assert_eq!(
        pushed["remote_sha_before"], repaired,
        "the next push replaces exactly the head auto-fix published"
    );
}

/// A branch move nothing recorded — or that another run or actor recorded —
/// is still refused.
pub(super) fn unrecorded_publication_move_is_refused() {
    let mut f = Fixture::new();
    let original_checkout = publication(&mut f);
    let published = f.head.clone();
    let moved = move_published_branch(&f, &original_checkout, "unrecorded edit\n");
    record_auto_fix_push(&f, "system", &published, &moved, "jrun-elsewhere");
    record_auto_fix_push(&f, "human:someone", &published, &moved, &f.source.run_id);
    requeue(&mut f);
    let (run, setup) = next(&f);
    let refused = execute_deterministic_action(&f.runtime,"candidate_resume",&json!({}),&json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}),false,&HashMap::new(),None).unwrap_err();
    assert!(
        refused.to_string().contains("moved"),
        "the unauthenticated move is refused: {refused}"
    );
}
