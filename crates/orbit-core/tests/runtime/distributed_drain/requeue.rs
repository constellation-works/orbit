//! Machine-qualified requeue through the public tool boundary.

use super::*;

/// A settled follower claim fences its leaf even while an unrelated owner
/// run with the same ID is live or has published its own candidate.
#[test]
fn requeue_ignores_a_colliding_owner_run_for_a_settled_follower() {
    if !isolated(
        module_path!(),
        "requeue_ignores_a_colliding_owner_run_for_a_settled_follower",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task_id = pair.claimed_task(&leaf);
    pair.leaf_fails_with(&leaf, "candidate implementation failed");
    pair.pass(&drain);
    assert_eq!(pair.owner_status(&task_id), "blocked");
    assert_eq!(pair.owner_claim_for_leaf(&leaf)["claim"]["phase"], "failed");
    let owner = &pair.wire.owner;
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let unrelated = jobs
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    owner
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET run_id = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    [
                        leaf.as_str(),
                        owner.workspace_id().unwrap().as_str(),
                        unrelated.run_id.as_str(),
                    ],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .unwrap();
    for publication in ["live", "pipeline", "failure_handoff"] {
        if publication != "live" {
            jobs.finalize_job_run(&leaf, JobRunState::Failed, Utc::now(), None)
                .unwrap();
            let mut state = PipelineState::new(leaf.clone(), "task_pr_pipeline".into(), json!({}));
            if publication == "pipeline" {
                state.pipeline["push"] = json!({"published": true});
            } else {
                state.failure_activity_checkpoint =
                    Some(orbit_types::workflow::FailureActivityCheckpoint {
                        activity_name: "pr_failure_handoff".into(),
                        failed_step_id: "validate".into(),
                        output: json!({
                            "decision": "blocked_failure_pr", "pr_number": "17",
                            "branch": "owner/candidate", "head_sha": "owner-head",
                            "task_spec_digest": "owner-spec",
                        }),
                    });
            }
            owner.write_run_state(&leaf, &state).unwrap();
        }
        let requeued = requeue(owner, &task_id).unwrap_or_else(|error| panic!(
            "ORB-16822: {publication} owner run must never stand in for a settled follower leaf: {error}"
        ));
        assert_eq!(pair.owner_status(&task_id), "backlog");
        assert_eq!(requeued["resumes"], Value::Null, "{requeued}");
        assert_eq!(
            owner
                .get_task(&task_id)
                .unwrap()
                .job_run_machine
                .unwrap()
                .machine_id,
            FOLLOWER
        );
        assert_eq!(
            jobs.get_job_run(&leaf).unwrap().unwrap().state,
            if publication == "live" {
                JobRunState::Running
            } else {
                JobRunState::Failed
            }
        );
        owner
            .apply_task_automation_update(
                &task_id,
                orbit_engine::TaskAutomationUpdate {
                    status: Some(orbit_types::task::TaskStatus::Blocked),
                    status_event: Some("claim_failed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
    }
}

fn requeue(owner: &OrbitRuntime, task: &str) -> Result<Value, OrbitError> {
    owner.run_tool_with_context_and_role(
        "orbit.task.requeue",
        json!({"id": task, "reason": "retry after the delivery stops"}),
        Role::Admin,
        ToolContext {
            trusted_actor_label: Some("human".into()),
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

/// Each safety refusal names its evidence and leaves the blocked task alone.
#[test]
fn requeue_names_the_live_run_or_pr_pipeline() {
    if !isolated(module_path!(), "requeue_names_the_live_run_or_pr_pipeline") {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let owner = &pair.wire.owner;
    pair.leaf_fails_with(&leaf, "candidate implementation failed");
    let refused = |check: &str, run: &str| {
        let before = owner.get_task(&task).unwrap();
        let history = owner.get_task_history(&task).unwrap();
        let error = requeue(owner, &task)
            .expect_err("delivery must stop first")
            .to_string();
        assert!(
            error.contains(check) && error.contains(run),
            "ORB-16822: the refusal must identify the {check} and its run {run}: {error}"
        );
        assert_eq!(owner.get_task(&task).unwrap().updated_at, before.updated_at);
        assert_eq!(owner.get_task_history(&task).unwrap(), history);
        assert_eq!(pair.owner_status(&task), "blocked");
    };
    pair.pass(&drain);
    assert_eq!(pair.owner_claim_for_leaf(&leaf)["claim"]["phase"], "failed");

    // Explicit local and legacy locations both retain their local safety checks.
    for location in [
        Some(orbit_types::task::ExecutionLocation {
            machine_id: OWNER.into(),
            machine_name: None,
        }),
        None,
    ] {
        let jobs = orbit_store::compose::workspace_job_run_store(
            owner.sqlite_store().unwrap(),
            owner.workspace_id().unwrap(),
        )
        .with_execution_location(location.clone());
        let local = jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"task_ids": [task]})),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&local.run_id, Utc::now(), std::process::id())
            .unwrap();
        owner
            .apply_task_automation_update(
                &task,
                orbit_engine::TaskAutomationUpdate {
                    job_run_id: Some(local.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(owner.get_task(&task).unwrap().job_run_machine, location);
        refused("its run", &local.run_id);
        jobs.finalize_job_run(&local.run_id, JobRunState::Failed, Utc::now(), None)
            .unwrap();
        let retry = jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"task_ids": [task]})),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&retry.run_id, Utc::now(), std::process::id())
            .unwrap();
        refused("task_pr_pipeline run", &retry.run_id);
        jobs.finalize_job_run(&retry.run_id, JobRunState::Failed, Utc::now(), None)
            .unwrap();
    }
}
