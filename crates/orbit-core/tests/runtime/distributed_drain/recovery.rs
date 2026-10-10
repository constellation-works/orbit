//! Final recovery of claimed leaves.

use super::*;

/// [ORB-13964, ORB-14635] The owner recovers the settled follower failure,
/// even when an unrelated local run has the same machine-local ID. Explicit
/// local and legacy bindings still use their own recorded run evidence.
#[test]
#[cfg(unix)]
fn recovery_uses_machine_bound_run_evidence_and_its_own_crew_draw() {
    use std::os::unix::fs::PermissionsExt;

    use orbit_core::application::task::{
        BLOCKED_TASK_RECOVERY_JOB, BlockedRecoveryInput, EpisodeDisposition, FinalRecoveryRecord,
    };
    use orbit_types::workflow::{ExecutorSandboxKind, FINAL_RECOVERY_CREWS_KEY};

    if !isolated(
        module_path!(),
        "recovery_uses_machine_bound_run_evidence_and_its_own_crew_draw",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task_id = pair.claimed_task(&leaf);
    let failure = "validation failed; preserved candidate branch: fixture/candidate";
    pair.leaf_fails_with(&leaf, failure);
    pair.pass(&drain);
    assert_eq!(pair.owner_status(&task_id), "blocked");
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "failed");

    let owner_repo = pair.wire.owner.paths().repo_root.clone();
    std::fs::write(owner_repo.join(".gitignore"), ".orbit/\n").unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", ".gitignore", "src"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "base",
        ],
        vec!["branch", "fixture/candidate"],
    ] {
        let output = orbit_common::fs::git::run_git(&owner_repo, &args).expect("seed owner Git");
        assert!(output.success, "{args:?}: {}", output.stderr);
    }
    std::fs::write(
        owner_repo.join(".orbit/config.toml"),
        r#"[workflow]
default_crew = "recovery_a"
final_recovery_crews = ["recovery_a:7", "recovery_b:3"]

[crews.sol]
provider = "codex"
model = "fixture-task"

[crews.recovery_a]
provider = "codex"
model = "fixture-a"

[crews.recovery_b]
provider = "codex"
model = "fixture-b"
"#,
    )
    .unwrap();
    orbit_core::bootstrap::init::init_workspace_at_root(
        &pair.wire.owner.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("seed shipped recovery job and activities");
    let owner = calm_host(
        OrbitRuntime::from_roots(&pair.wire.owner.global_root(), &owner_repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some(OWNER.into())),
    );
    assert!(owner.read_run_state(&leaf).unwrap().is_none());
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    assert!(jobs.get_job_run(&leaf).unwrap().is_none());
    let unrelated = jobs
        .insert_job_run("owner_unrelated_job", 1, Utc::now(), None, None)
        .unwrap();
    let workspace_id = owner.workspace_id().unwrap();
    owner.sqlite_store().unwrap().with_transaction(|tx| {
        tx.connection().execute(
            "UPDATE job_runs SET run_id = ?1, resolved_crew = 'owner_only_crew', crew_model = 'gpt-6-sol' WHERE workspace_id = ?2 AND run_id = ?3",
            [leaf.as_str(), workspace_id.as_str(), unrelated.run_id.as_str()],
        ).unwrap();
        Ok(())
    }).unwrap();
    jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .unwrap();
    let now = Utc::now();
    jobs.complete_job_run_step(
        &leaf,
        &JobRunStepParams {
            step_index: 0,
            target_type: JobTargetType::Activity,
            target_id: "owner_only_step".into(),
            started_at: now,
            finished_at: now,
            duration_ms: None,
            exit_code: Some(1),
            agent_response_json: None,
            state: JobRunState::Failed,
            error_code: Some("owner_only_error".into()),
            error_message: Some("unrelated owner diagnostic".into()),
        },
    )
    .unwrap();
    jobs.finalize_job_run(&leaf, JobRunState::Failed, now, None)
        .unwrap();

    let shown = owner
        .run_tool("orbit.task.show", json!({"id": task_id}))
        .unwrap();
    assert_eq!(shown["job_run_machine"]["machine_id"], FOLLOWER);
    assert_eq!(
        shown["resolved_crew"], "sol",
        "ORB-14635: task show must not project the colliding owner's crew"
    );
    assert_eq!(shown["crew_model"], "fixture-task");
    for input in [
        json!({"task_id": task_id}),
        json!({"task_id": task_id, "run_id": leaf}),
    ] {
        assert_eq!(
            owner.activity_implementer_identity(&input).unwrap(),
            (None, None),
            "ORB-14635: task-derived attribution must not read a foreign run locally"
        );
    }

    // The real dispatcher selects the crew and injects execution identity;
    // only the provider's decision is deterministic in this fixture.
    let provider = pair._root.path().join("codex");
    std::fs::write(
        &provider,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"decision\":\"requeue\",\"reason\":\"fixture prerequisite repaired\"},\"error\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    follower_cli(&owner, "codex", "sh");
    let mut executor = owner.get_executor_def("codex").unwrap().unwrap();
    executor.command = Some(provider.to_string_lossy().to_string());
    executor.sandbox = Some(ExecutorSandboxKind::Off);
    owner.upsert_executor_def(&executor).unwrap();

    let views = owner.blocked_recovery_view(Utc::now()).unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.disposition, EpisodeDisposition::Eligible);
    assert_eq!(view.episode.failed_run_id.as_deref(), Some(leaf.as_str()));
    let input = BlockedRecoveryInput {
        task_id: task_id.clone(),
        episode_key: view.episode.key(),
        block_source: view.episode.source.as_str().to_string(),
        failed_run_id: view.episode.failed_run_id.clone(),
        observed: owner
            .final_recovery_revision(&owner.get_task(&task_id).unwrap())
            .unwrap(),
    };
    let job = owner
        .show_job_catalog_entry(BLOCKED_TASK_RECOVERY_JOB)
        .unwrap();
    let recovery = owner
        .run_job_v2_from_yaml(&job.path, input.to_json())
        .unwrap();
    assert!(recovery.success, "{recovery:#?}");
    let prepared = &recovery.pipeline["prepare"]["recovery"];
    assert_eq!(prepared["run_id"], leaf);
    assert_eq!(prepared["failed_step_id"], "claim_failed");
    assert_eq!(prepared["activity_name"], "");
    assert!(
        prepared["error_message"]
            .as_str()
            .unwrap()
            .contains(failure),
        "the owner supplies the settled diagnostic, including available candidate evidence: {prepared}"
    );
    let diagnostic = prepared["error_message"].as_str().unwrap();
    assert!(
        diagnostic.contains(FOLLOWER),
        "ORB-14635: recovery must name the foreign execution machine: {prepared}"
    );
    assert!(
        !diagnostic.contains("unrelated owner diagnostic"),
        "ORB-14635: recovery must not read the colliding local failure: {prepared}"
    );
    assert!(!Path::new(prepared["workspace_path"].as_str().unwrap()).exists());
    let state = owner.read_run_state(&recovery.run_id).unwrap().unwrap();
    let frozen = &state.activity_crew_draws[FINAL_RECOVERY_CREWS_KEY];
    assert_eq!(
        frozen
            .eligible_pool
            .iter()
            .map(|member| (member.name.as_str(), member.weight))
            .collect::<Vec<_>>(),
        [("recovery_a", 7), ("recovery_b", 3)]
    );
    let config = owner
        .agent_crew_config_for_input(&json!({
            "run_id": leaf,
            "job_run_id": recovery.run_id,
            "crew_config_key": FINAL_RECOVERY_CREWS_KEY,
        }))
        .unwrap()
        .unwrap();
    assert_eq!(
        config,
        owner
            .agent_crew_config_for_input(&json!({"crew": frozen.crew}))
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        owner.get_task(&task_id).unwrap().status.to_string(),
        "backlog"
    );
    let record = FinalRecoveryRecord::last(&owner.get_task_comments(&task_id).unwrap()).unwrap();
    assert_eq!(record.run_id, recovery.run_id);
    assert_eq!(record.decision, "requeue");
    assert_eq!(record.outcome, "requeued");
    assert!(owner.read_run_state(&leaf).unwrap().is_none());
    assert_eq!(
        jobs.get_job_run(&leaf).unwrap().unwrap().job_id,
        "owner_unrelated_job"
    );
    let candidate =
        orbit_common::fs::git::run_git(&owner_repo, &["rev-parse", "fixture/candidate"]).unwrap();
    assert!(candidate.success, "candidate evidence is preserved");
    assert_eq!(
        candidate.stdout.trim(),
        prepared["base_sha"].as_str().unwrap()
    );

    // Exercise the same preparation and task-show boundaries for explicit
    // local and absent legacy locations. The settled diagnostic deliberately
    // differs from the local step, so the evidence source is observable.
    for location in [
        Some(orbit_types::task::ExecutionLocation {
            machine_id: OWNER.into(),
            machine_name: Some("owner fixture".into()),
        }),
        None,
    ] {
        let local_jobs = jobs.with_execution_location(location.clone());
        let run = local_jobs
            .insert_job_run("local_failure_job", 1, Utc::now(), None, None)
            .unwrap();
        owner.sqlite_store().unwrap().with_transaction(|tx| {
            tx.connection().execute(
                "UPDATE job_runs SET resolved_crew = 'recorded_local_crew', crew_model = 'gpt-6-sol' WHERE workspace_id = ?1 AND run_id = ?2",
                [workspace_id.as_str(), run.run_id.as_str()],
            ).unwrap();
            Ok(())
        }).unwrap();
        local_jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        local_jobs
            .complete_job_run_step(
                &run.run_id,
                &JobRunStepParams {
                    step_index: 0,
                    target_type: JobTargetType::Activity,
                    target_id: "local_failure_step".into(),
                    started_at: now,
                    finished_at: now,
                    duration_ms: None,
                    exit_code: Some(1),
                    agent_response_json: None,
                    state: JobRunState::Failed,
                    error_code: Some("local_failure".into()),
                    error_message: Some("local step diagnostic".into()),
                },
            )
            .unwrap();
        local_jobs
            .finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), None)
            .unwrap();
        let task = owner
            .add_task(orbit_core::application::task::TaskAddParams {
                title: "Local recovery evidence".into(),
                plan: "Exercise recorded run evidence".into(),
                crew: Some("sol".into()),
                complexity: orbit_types::task::TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap();
        owner
            .apply_task_automation_update(
                &task.id,
                orbit_engine::TaskAutomationUpdate {
                    status: Some(orbit_types::task::TaskStatus::InProgress),
                    job_run_id: Some(run.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        owner
            .apply_task_automation_update(
                &task.id,
                orbit_engine::TaskAutomationUpdate {
                    status: Some(orbit_types::task::TaskStatus::Blocked),
                    status_event: Some("claim_failed".into()),
                    execution_summary: Some("settled fallback diagnostic".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let task = owner.get_task(&task.id).unwrap();
        assert_eq!(task.job_run_machine, location);
        let shown = owner
            .run_tool("orbit.task.show", json!({"id": task.id}))
            .unwrap();
        assert_eq!(shown["resolved_crew"], "recorded_local_crew");
        assert_eq!(shown["crew_model"], "gpt-6-sol");
        assert_eq!(
            owner
                .activity_implementer_identity(&json!({"task_id": task.id}))
                .unwrap(),
            (Some("codex".into()), Some("codex".into()))
        );
        let view = owner
            .blocked_recovery_view(Utc::now())
            .unwrap()
            .into_iter()
            .find(|view| view.episode.task_id == task.id)
            .unwrap();
        let input = BlockedRecoveryInput {
            task_id: task.id.clone(),
            episode_key: view.episode.key(),
            block_source: view.episode.source.as_str().into(),
            failed_run_id: view.episode.failed_run_id,
            observed: owner.final_recovery_revision(&task).unwrap(),
        };
        let recovery = owner
            .run_job_v2_from_yaml(&job.path, input.to_json())
            .unwrap();
        assert!(recovery.success, "{recovery:#?}");
        let prepared = &recovery.pipeline["prepare"]["recovery"];
        assert_eq!(prepared["failed_step_id"], "local_failure_step");
        assert_eq!(prepared["activity_name"], "local_failure_job");
        assert_eq!(prepared["error_message"], "local step diagnostic");
    }
}

/// [ORB-14635] A foreign binding must neither borrow completion evidence
/// from a successful local collision nor be vetoed by its live process.
#[test]
fn a_foreign_run_cannot_supply_or_veto_completion_evidence() {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_types::task::TaskStatus;

    if !isolated(
        module_path!(),
        "a_foreign_run_cannot_supply_or_veto_completion_evidence",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(&leaf, "follower stopped");
    pair.pass(&drain);
    let owner = &pair.wire.owner;
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run("unrelated_delivery", 1, Utc::now(), None, None)
        .unwrap();
    let workspace = owner.workspace_id().unwrap();
    owner
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET run_id = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    [leaf.as_str(), workspace.as_str(), run.run_id.as_str()],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        owner
            .update_task_as_human(
                &task,
                TaskUpdateParams {
                    status: Some(status),
                    plan: Some("Verify completion evidence".into()),
                    execution_summary: Some(String::new()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
    }
    assert!(
        owner
            .update_task_as_human(
                &task,
                TaskUpdateParams {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
                "human:fixture".into()
            )
            .is_err(),
        "ORB-14635: a successful local collision must not supply evidence for a foreign task"
    );
    assert_eq!(owner.get_task(&task).unwrap().status, TaskStatus::Review);

    // Seed the other collision shape with the same verified live process.
    // Re-delivering Start to a terminal run is deliberately a no-op.
    owner.sqlite_store().unwrap().with_transaction(|tx| {
        tx.connection().execute(
            "UPDATE job_runs SET state = 'running', finished_at = NULL, duration_ms = NULL WHERE workspace_id = ?1 AND run_id = ?2",
            [workspace.as_str(), leaf.as_str()],
        ).unwrap();
        Ok(())
    }).unwrap();
    assert_eq!(
        jobs.get_job_run(&leaf).unwrap().unwrap().state,
        JobRunState::Running
    );
    owner
        .update_task_as_human(
            &task,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                execution_summary: Some("Follower stopped; work independently verified".into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .expect("ORB-14635: an unrelated live local run must not veto foreign completion");
    assert_eq!(owner.get_task(&task).unwrap().status, TaskStatus::Done);
    assert_eq!(
        jobs.get_job_run(&leaf).unwrap().unwrap().state,
        JobRunState::Running
    );
}

/// [ORB-13907] A claimed leaf's final-recovery decision never touches the
/// owner's task from the follower: it rides on the leaf's failure settlement,
/// and the owner applies it once its journal has failed the claim. A replayed
/// settlement changes nothing more.
#[test]
fn a_claimed_leaf_final_recovery_decision_is_applied_by_the_owner_through_settlement() {
    if !isolated(
        module_path!(),
        "a_claimed_leaf_final_recovery_decision_is_applied_by_the_owner_through_settlement",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let mut state = pair
        .follower_jobs
        .read_run_state(&leaf)
        .unwrap()
        .unwrap_or_else(|| PipelineState::new(leaf.clone(), LEAF_JOB.into(), json!({})));
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: leaf.clone(),
            attempt: 1,
        },
        failed_step_id: "implement".into(),
        task_id: task.clone(),
        observed: None,
        repair_commit: None,
        base_ref: Some("main".into()),
        admitted_at: Utc::now(),
        decision: Some(FinalRecoveryDecision::Reject {
            reason: "the task contradicts its own criteria".into(),
            evidence: "criterion 1 forbids the change criterion 2 requires".into(),
        }),
        outcome: Some("settled: recorded for the claim settlement".into()),
    });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    // The leaf ran, its final recovery decided, and the run failed.
    pair.leaf_fails_with(&leaf, "implement failed");
    let before = pair.owner_task(&task);

    let settled = pair.pass(&drain);
    assert!(
        !launch_refused(&settled),
        "a failed leaf is settled, not relaunched: {settled}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let carried = &settles[0]["settlement"]["Fail"]["final_recovery"];
    assert_eq!(carried["run_id"], leaf.as_str(), "{settles:?}");
    assert_eq!(carried["decision"]["decision"], "reject", "{settles:?}");
    assert_eq!(
        settles[0]["settlement"]["Fail"]["failure"]["class"], "task_input",
        "a rejected task is the task's own failure: {settles:?}"
    );
    assert_ne!(
        before["status"], "rejected",
        "the follower wrote nothing to the owner's task"
    );

    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "failed");
    let applied = pair.owner_task(&task);
    assert_eq!(applied["status"], "rejected", "{applied:#}");
    let header = format!("final_recovery run_id={leaf} decision=reject");
    let recorded = |task: &Value| {
        task["comments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|comment| {
                comment["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with(&header))
            })
            .count()
    };
    assert_eq!(recorded(&applied), 1, "{applied:#}");

    let replay = pair
        .wire
        .call("", "orbit.drain.claim.settle", settles[0].clone())
        .expect("a replayed settlement answers with the recorded outcome");
    assert_eq!(replay["phase"], "failed", "{replay}");
    let after = pair.owner_task(&task);
    for field in ["status", "comments", "history"] {
        assert_eq!(after[field], applied[field], "{field} changed on replay");
    }
}

/// [ORB-15292] A claimed leaf's final recovery may record a friction. The
/// bridge keeps it on the owner as `claim_friction` history, which advances
/// the task's update time but is no lifecycle change: the recovery's requeue,
/// carried by the failure settlement, still returns the task to the backlog.
#[test]
fn a_friction_the_claimed_recovery_records_leaves_its_requeue_standing() {
    use orbit_core::application::task::FINAL_RECOVERY_REQUEUED_EVENT;
    use orbit_types::task::TaskStatus;
    use orbit_types::tool::WorkerInvocation;

    use super::claimed_review::ToOwner;

    if !isolated(
        module_path!(),
        "a_friction_the_claimed_recovery_records_leaves_its_requeue_standing",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let owner = &pair.wire.owner;
    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    // The leaf's worker, as its final-recovery agent calls the owner.
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: leaf.clone(),
            },
            Arc::new(ToOwner(owner.clone())),
        )
        .unwrap();
    let observed = owner
        .final_recovery_revision(&owner.get_task(&task).unwrap())
        .unwrap();

    bound
        .run_tool(
            "orbit.friction.add",
            json!({"body": "The golden fixtures timed out.", "model": "codex"}),
        )
        .expect("the recovery records a friction during the claim");
    let history = owner.get_task_history(&task).unwrap();
    assert_eq!(
        history.last().map(|entry| entry.event.as_str()),
        Some("claim_friction"),
        "{history:#?}"
    );
    let current = owner
        .final_recovery_revision(&owner.get_task(&task).unwrap())
        .unwrap();
    assert!(
        current.updated_at > observed.updated_at,
        "the friction must advance the owner's update time"
    );
    assert!(
        !current.changed_since(&observed),
        "a friction the recovery records is not a lifecycle change"
    );

    let mut state = pair
        .follower_jobs
        .read_run_state(&leaf)
        .unwrap()
        .unwrap_or_else(|| PipelineState::new(leaf.clone(), LEAF_JOB.into(), json!({})));
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: leaf.clone(),
            attempt: 1,
        },
        failed_step_id: "implement".into(),
        task_id: task.clone(),
        observed: None,
        repair_commit: None,
        base_ref: Some("main".into()),
        admitted_at: Utc::now(),
        decision: Some(FinalRecoveryDecision::Requeue {
            reason: "the goldens now pass on the unchanged candidate".into(),
        }),
        outcome: Some("settled: recorded for the claim settlement".into()),
    });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    pair.leaf_fails_with(&leaf, "golden fixtures failed");
    pair.pass(&drain);

    // The owner applied the requeue; the same pass may then pull the task
    // again under a new claim.
    let claims = pair.owner_claims();
    let settled = claims
        .iter()
        .find(|entry| entry["claim"]["claim_id"] == claim.claim_id.as_str())
        .expect("the leaf's claim");
    assert_eq!(settled["claim"]["phase"], "failed", "{settled:#}");
    let applied = pair.owner_task(&task);
    let comments = comments_of(&applied);
    assert!(
        comments.contains(&format!(
            "final_recovery run_id={leaf} decision=requeue outcome=requeued"
        )),
        "{comments}"
    );
    let requeued = owner
        .get_task_history(&task)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == FINAL_RECOVERY_REQUEUED_EVENT)
        .expect("the requeue is recorded");
    assert_eq!(
        requeued.to_status,
        Some(TaskStatus::Backlog),
        "{requeued:?}"
    );
}
