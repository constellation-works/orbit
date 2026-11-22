use super::*;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::workflow::{DeliveryRefusal, JobTargetType};

fn held_completion_fixture() -> Fixture {
    held_completion_fixture_with_policy("none")
}

fn held_completion_fixture_with_policy(timing: &str) -> Fixture {
    let mut f = Fixture::with_policy(timing);
    f.source.state = JobRunState::Held;
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &f.source, None)
        .unwrap();
    // Completion failed after publication. The worker leaves the last
    // successful checkpoints intact and appends a run-level diagnostic.
    f.runtime
        .complete_job_run_step(
            &f.source.run_id,
            &JobRunStepParams {
                step_index: 6,
                target_type: JobTargetType::Job,
                target_id: f.source.job_id.clone(),
                state: JobRunState::Held,
                started_at: f.source.created_at,
                finished_at: f.source.finished_at.unwrap(),
                duration_ms: None,
                exit_code: None,
                lock_wait_ms: None,
                agent_response_json: None,
                error_code: Some(
                    DeliveryRefusal::CheckEvidenceInconclusive
                        .error_code()
                        .into(),
                ),
                error_message: Some(
                    "auto_fix_stopped: required-check evidence is unavailable".into(),
                ),
            },
        )
        .unwrap();
    f
}

fn published_completion_hold_starts_repair_at_published_head() {
    let f = held_completion_fixture();
    let binding = f.inspect();
    assert_eq!(binding["head_sha"], f.head);
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(f.inspect(), binding);
    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
        .unwrap();
    let id = admitted["run_id"].as_str().unwrap();
    assert_eq!(admitted["retry_source_run_id"], f.source.run_id);
    assert_eq!(admitted["completion"], "review");
    f.wait(id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,
            "base_sha":setup["base_sha"],"prior_job_run_id":f.source.run_id}),
    );
    assert_eq!(resumed["implement"], true);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["source_sha"], f.head);
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(git(&checkout, &["branch", "--show-current"]), f.branch);
    // Source repair reapplies the published changes as uncommitted edits on
    // the current base. In this fixture the base is unchanged.
    git(&checkout, &["diff", "--exit-code", &f.head, "--"]);
    f.assert_original();
}

fn unpublished_held_run_cannot_authorize_repair() {
    let f = held_completion_fixture();
    git(&f.repo, &["push", "origin", "--delete", &f.branch]);
    let mut state = f.state.clone();
    let outputs = state.pipeline.as_object_mut().unwrap();
    outputs.remove("push");
    outputs.remove("pr_open");
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.replace_refs(Vec::new());
    let task = f.runtime.get_task(&f.task).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap_err();
    assert!(
        error.to_string().contains("source lacks push evidence"),
        "{error}"
    );
    assert_eq!(f.runtime.get_task(&f.task).unwrap(), task);
    assert_eq!(f.runtime.get_task_history(&f.task).unwrap(), history);
}

fn resident_starts_preserved_delivery_on_current_base() {
    use crate::resident_cycle::{call, resident_test_broker};
    use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
    use orbit_core::runtime::plugin::broker::{ForwardCallError, forward_call_with_status};
    use orbit_engine::PluginBrokerHandle;
    use orbit_types::task::TaskArtifact;
    use orbit_types::tool::McpCapability;
    use orbit_types::workflow::{SOURCE_REPAIR_KEY, SourceRepairAdmission};

    let f = held_completion_fixture_with_policy("before-pr");
    let block = || {
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    block();
    git(&f.repo, &["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("current-base.txt"), "new current base\n").unwrap();
    git(&f.repo, &["add", "current-base.txt"]);
    git(
        &f.repo,
        &["commit", "-qm", "advance after held publication"],
    );
    git(&f.repo, &["push", "-q", "origin", "main"]);
    let current_base = git(&f.repo, &["rev-parse", "HEAD"]);
    let workspace = f.runtime.workspace_id().unwrap();
    let make_cycle = || {
        let mandate = f
            .runtime
            .add_task(TaskAddParams {
                title: "[orchestration]: exercise preserved delivery recovery".into(),
                tags: vec!["resident".into()],
                ..Default::default()
            })
            .unwrap()
            .id;
        f.runtime
            .update_task_as_human(
                &mandate,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        "resident/mandate.json",
                        json!({
                            "schema": "resident.mandate/v1",
                            "goal": "Recover the held delivery",
                            "exit_criteria": ["The candidate passes review"],
                            "workspaces": [workspace],
                            "watch": {"tasks": [f.task]},
                            "crew": "fixture",
                            "cadence": {"heartbeat_minutes": 120}
                        })
                        .to_string(),
                    )],
                    ..Default::default()
                },
                "human:operator".into(),
            )
            .unwrap();
        call(
            &f.runtime,
            "orbit.resident.grant",
            json!({"task_id": mandate, "add": ["recover"], "workspaces": [workspace]}),
            McpCapability::Operator,
        )
        .unwrap();
        let run = f
            .runtime
            .insert_job_run(
                "resident_cycle_pipeline",
                1,
                Utc::now(),
                Some(json!({"task_ids": [mandate]})),
                None,
            )
            .unwrap();
        f.runtime
            .begin_resident_cycle(&run.run_id, Utc::now())
            .unwrap();
        let broker = resident_test_broker(&f.runtime, &mandate, &run.run_id);
        (mandate, broker)
    };
    let recover = |broker: &orbit_core::runtime::plugin::broker::PluginBroker| {
        forward_call_with_status(
            broker.socket_path(),
            "orbit.task.resume_delivery",
            json!({
                "id": f.task, "run_id": f.source.run_id, "workspace": workspace,
                "diagnosis": "Finish the preserved compiler repair on the current base",
            }),
            &std::env::current_dir().unwrap(),
            Some(&workspace),
            "mcp",
        )
        .map_err(|error| match error {
            ForwardCallError::CallerAudit(error) | ForwardCallError::BrokerAudit(error) => error,
        })
    };
    let (mandate, broker) = make_cycle();
    let admitted = recover(&broker).unwrap_or_else(|error| panic!("{error:?}"));
    let id = admitted["run_id"].as_str().unwrap();
    assert_eq!(admitted["retry_source_run_id"], f.source.run_id);
    assert_eq!(admitted["completion"], "review");
    let key = admitted["resident_action_key"].as_str().unwrap();
    assert!(
        key.starts_with(&format!("resident:{mandate}:")),
        "{admitted}"
    );
    let run = f.runtime.show_job_run_observed(id).unwrap();
    let input = run.input.unwrap();
    let reservation: SourceRepairAdmission =
        serde_json::from_value(input[SOURCE_REPAIR_KEY].clone()).unwrap();
    assert_eq!(reservation.request_key, key);
    assert!(reservation.review.gates_pr());
    assert!(reservation.resident_repair_deadline().is_some());
    assert_eq!(input["completion"], "review");
    assert_eq!(input["auto_fix"], false);

    let refused = recover(&broker).unwrap_err();
    assert!(
        format!("{refused:?}").contains("resident_recovery_limit"),
        "{refused:?}"
    );
    f.wait(id);
    f.start_actions(id);
    let setup = f.action("worktree_setup", json!({
        "job_run_id": id, "run_id": id, "task_ids": [f.task], "base": "main", "base_sync": "remote",
    }));
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    assert_eq!(setup["base_sha"], current_base);
    let resumed = f.action(
        "candidate_resume",
        json!({
            "job_run_id": id, "task_ids": [f.task], "workspace_path": checkout,
            "base_sha": setup["base_sha"], "prior_job_run_id": f.source.run_id,
        }),
    );
    assert_eq!(resumed["source_sha"], f.head);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(resumed["implement"], true);
    assert_eq!(
        resumed["repair"]["failed_step_id"],
        "resident_source_repair"
    );
    assert!(checkout.join("current-base.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(checkout.join("source.txt")).unwrap(),
        "candidate needing repair\n"
    );
    std::fs::write(
        checkout.join("source.txt"),
        "candidate repaired on the current base\n",
    )
    .unwrap();
    f.action(
        "git_commit",
        json!({
            "job_run_id": id, "scope": "all", "workspace_path": checkout,
            "base_sha": setup["base_sha"], "base_ref": setup["base_ref"],
        }),
    );
    let validated = f.action(
        "candidate_validate",
        json!({
            "job_run_id": id, "completed_task_ids": [f.task], "workspace_path": checkout,
            "base_sha": setup["base_sha"], "base_ref": setup["base_ref"],
        }),
    );
    assert_eq!(validated["decision"], "passed");
    assert_ne!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
    f.assert_original();

    block();
    let (_, other_mandate) = make_cycle();
    let refused = recover(&other_mandate).unwrap_err();
    assert!(
        format!("{refused:?}").contains("resident_recovery_limit"),
        "{refused:?}"
    );
}

crate::scenario_table!(
    held_publication_source_repair,
    published_completion_hold_starts_repair_at_published_head,
    unpublished_held_run_cannot_authorize_repair,
    resident_starts_preserved_delivery_on_current_base,
);
