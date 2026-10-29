//! Production-shaped legacy reservations through the public operator route.
use super::*;
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    SourceRepairAdmission, source_repair_authorizations, source_repair_reservations,
};

const PLACEHOLDER: &str = "To be authored by executing agent at start time.";

fn plan(f: &Fixture, value: &str) {
    f.runtime
        .update_task_as_human(
            &f.task,
            TaskUpdateParams {
                plan: Some(value.into()),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
}

fn append(f: &Fixture, event: &str, value: Value) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.into(),
                    note: Some(value.to_string()),
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

/// Seed only an isolated host's historical record, never edit a real receipt.
/// Its shape is exactly the old post-reservation lifecycle failure.
fn legacy(f: &Fixture, missing: &str) -> (Value, Value) {
    plan(f, missing);
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    let request = super::inline_publication::rework(f);
    let payload = &request["source_repair"];
    let record = SourceRepairAdmission {
        request_key: "repair-1".into(),
        binding: serde_json::from_value(inspection["binding"].clone()).unwrap(),
        diagnosis: format!(
            "{}\n\nUnmet acceptance evidence:\n{}",
            payload["diagnosis"].as_str().unwrap(),
            payload["unmet_acceptance"].as_str().unwrap()
        ),
        unmet_acceptance: Some(payload["unmet_acceptance"].as_str().unwrap().into()),
        policy_digest: Some(inspection["policy_digest"].as_str().unwrap().into()),
        authorized_by: f.runtime.actor().resolve_write_label(None, None).unwrap(),
        authorizer_provenance: "operator-override".into(),
        authorized_at: Utc::now(),
        attempt: orbit_types::workflow::pr_auto_fix_attempts(
            &f.runtime.get_task_history(&f.task).unwrap(),
            "17",
        )
        .len() as u32
            + 1,
        review: serde_json::from_value(inspection["review"].clone()).unwrap(),
    };
    let record = serde_json::to_value(record).unwrap();
    append(f, "source_repair_reserved", record.clone());
    (request, record)
}

fn reconcile(f: &Fixture, original: &Value) -> Value {
    let inspected = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    let mut request = super::inline_publication::rework(f);
    request["source_repair"]["action"] = json!("reconcile");
    request["source_repair"]["reservation_digest"] =
        inspected["reservation_digests"][original["request_key"].as_str().unwrap()].clone();
    request
}

fn snapshot(f: &Fixture) -> Value {
    json!({"task":f.runtime.get_task(&f.task).unwrap(), "history":f.runtime.get_task_history(&f.task).unwrap(),
        "runs":f.runtime.job_history("task_pr_pipeline").unwrap(),
        "source":f.runtime.read_run_state(&f.source.run_id).unwrap(), "refs":git(&f.repo, &["ls-remote", "origin"])})
}

fn refuse(f: &Fixture, input: Value, case: &str) -> String {
    let before = snapshot(f);
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input)
        .unwrap_err()
        .to_string();
    assert_eq!(
        snapshot(f),
        before,
        "{case}: refusal must retain all evidence and admit nothing"
    );
    error
}

fn count_worker_starts(marker: &std::path::Path) {
    orbit_core::test_support::install_substitute_pipeline_worker([
        "sh".to_string(),
        "-c".into(),
        r#"printf 'started\n' >> "$1""#.into(),
        "source-repair-worker".into(),
        marker.to_string_lossy().into_owned(),
    ]);
}

fn legacy_plan_correction_gets_one_fresh_same_pr_authorization() {
    for (status, missing) in [(TaskStatus::Blocked, ""), (TaskStatus::Review, PLACEHOLDER)] {
        let (f, _, _) = super::inline_publication::supported_incident();
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .unwrap();
        let (old, original) = legacy(&f, missing);
        let receipts = f.runtime.read_run_state(&f.source.run_id).unwrap();
        let before_runs = f.runtime.job_history("task_pr_pipeline").unwrap();
        let lineage_receipts = before_runs
            .iter()
            .map(|run| {
                (
                    run.run_id.clone(),
                    f.runtime.read_run_state(&run.run_id).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        plan(
            &f,
            "Repair the observed compiler defect and validate the existing PR",
        );
        assert!(refuse(&f, old, "old consent").contains("stale"));
        let request = reconcile(&f, &original);
        let worker_starts = f._root.path().join("repair-worker-starts");
        count_worker_starts(&worker_starts);
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
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
        assert_eq!(results.0["run_id"], results.1["run_id"]);
        let id = results.0["run_id"].as_str().unwrap();
        f.wait(id);
        let again = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", request)
            .unwrap();
        assert_eq!(again["run_id"], id);
        assert_eq!(
            std::fs::read_to_string(&worker_starts).unwrap(),
            "started\n",
            "concurrent and lost-answer replays spawn only one worker"
        );
        assert_eq!(
            f.runtime.job_history("task_pr_pipeline").unwrap().len(),
            before_runs.len() + 1
        );
        let history = f.runtime.get_task_history(&f.task).unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|e| e.event == "source_repair_reserved")
                .count(),
            1
        );
        assert_eq!(
            history
                .iter()
                .filter(|e| e.event == "source_repair_reconciled")
                .count(),
            1
        );
        assert_eq!(
            history
                .iter()
                .filter(|e| e.event == "source_repair_linked")
                .count(),
            1
        );
        let originals = source_repair_reservations(&history).unwrap();
        assert_eq!(serde_json::to_value(&originals[0]).unwrap(), original);
        let authorized = source_repair_authorizations(&history).unwrap();
        assert_eq!(
            authorized[0].attempt, originals[0].attempt,
            "no second charge"
        );
        assert_ne!(
            authorized[0].binding.task_digest,
            originals[0].binding.task_digest
        );
        assert!(authorized[0].authorized_at >= originals[0].authorized_at);
        let run = f.runtime.show_job_run_observed(id).unwrap();
        assert_eq!(run.input.as_ref().unwrap()["completion"], "review");
        assert_eq!(run.input.as_ref().unwrap()["auto_fix"], false);
        assert_eq!(
            run.input.as_ref().unwrap()["__source_repair"],
            serde_json::to_value(&authorized[0]).unwrap()
        );
        f.start_actions(id);
        let setup = f.action(
            "worktree_setup",
            json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
        );
        let resumed = f.action("candidate_resume", json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],"prior_job_run_id":f.source.run_id}));
        assert_eq!(resumed["implement"], true);
        assert_eq!(resumed["prior_pull_request"], "17");
        assert_eq!(resumed["reused_head_sha"], f.head);
        assert_eq!(
            f.runtime.read_run_state(&f.source.run_id).unwrap(),
            receipts
        );
        for (id, before) in lineage_receipts {
            assert_eq!(
                f.runtime.read_run_state(&id).unwrap(),
                before,
                "historical lineage receipts and deadlines remain immutable"
            );
        }
    }
}

fn reconciliation_lost_spawn_answer_does_not_admit_twice() {
    let (f, _, _) = super::inline_publication::supported_incident();
    let (_, original) = legacy(&f, "");
    plan(&f, "Repair the observed defect");
    let request = reconcile(&f, &original);
    let missing = f._root.path().join("missing-worker");
    orbit_core::test_support::install_substitute_pipeline_worker([missing
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
    let before_runs = f.runtime.job_history("task_pr_pipeline").unwrap().len();
    let worker_starts = f._root.path().join("unexpected-replay-worker");
    count_worker_starts(&worker_starts);
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", request.clone())
        .unwrap();
    assert_eq!(result["replayed"], true);
    assert_eq!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", request)
            .unwrap()["run_id"],
        result["run_id"]
    );
    assert_eq!(
        f.runtime.job_history("task_pr_pipeline").unwrap().len(),
        before_runs
    );
    assert!(
        !worker_starts.exists(),
        "a lost-start reply must never spawn a replacement worker"
    );
    let history = f.runtime.get_task_history(&f.task).unwrap();
    for event in ["source_repair_reserved", "source_repair_reconciled"] {
        assert_eq!(history.iter().filter(|e| e.event == event).count(), 1);
    }
}

fn reconciliation_refuses_adjacent_identity_and_meaning_drift() {
    let (f, _, _) = super::inline_publication::supported_incident();
    let (_, original) = legacy(&f, "");
    plan(&f, "Repair the observed defect");
    let request = reconcile(&f, &original);
    for field in [
        "task_digest",
        "source_digest",
        "source_run_id",
        "head_sha",
        "base_sha",
        "base_branch",
        "branch",
        "remote_url",
        "repository",
        "pull_request",
        "pull_request_url",
        "machine_id",
        "workspace_id",
        "superseded",
    ] {
        let mut changed = request.clone();
        changed["source_repair"]["binding"][field] = json!("changed");
        refuse(&f, changed, field);
    }
    for field in [
        "reservation_digest",
        "policy_digest",
        "diagnosis",
        "unmet_acceptance",
        "request_key",
    ] {
        let mut changed = request.clone();
        changed["source_repair"][field] = json!("changed");
        refuse(&f, changed, field);
    }
    let task = f.runtime.get_task(&f.task).unwrap();
    for params in [
        TaskUpdateParams {
            title: Some("Other task meaning".into()),
            ..Default::default()
        },
        TaskUpdateParams {
            context_files: Some(vec!["file:other.txt".into()]),
            ..Default::default()
        },
        TaskUpdateParams {
            tags: Some(vec!["changed-meaning".into()]),
            ..Default::default()
        },
    ] {
        f.runtime
            .update_task_as_human(&f.task, params, "operator".into())
            .unwrap();
        // Fresh inspection cannot bless unrelated meaning changes.
        let fresh = reconcile(&f, &original);
        refuse(&f, fresh, "unrelated task meaning");
        f.runtime
            .update_task_as_human(
                &f.task,
                TaskUpdateParams {
                    title: Some(task.title.clone()),
                    context_files: Some(task.context_files.clone()),
                    tags: Some(task.tags.clone()),
                    ..Default::default()
                },
                "operator".into(),
            )
            .unwrap();
    }
    plan(&f, "");
    refuse(&f, reconcile(&f, &original), "uncorrected plan");
}

fn reconciliation_rechecks_live_authority_policy_and_lease() {
    let (mut f, _, _) = super::inline_publication::supported_incident();
    let (_, original) = legacy(&f, "");
    plan(&f, "Repair the observed defect");
    let request = reconcile(&f, &original);
    let before = snapshot(&f);
    let other = f
        .runtime
        .clone()
        .with_actor(orbit_core::ActorIdentity::human("human:another-operator"));
    assert!(
        other
            .run_tool_as_human("orbit.workflow.run.resume", request.clone())
            .is_err()
    );
    assert_eq!(snapshot(&f), before);
    let agent = f
        .runtime
        .clone()
        .with_actor(orbit_core::ActorIdentity::agent("codex"));
    assert!(
        agent
            .run_tool_as_human("orbit.workflow.run.resume", request.clone())
            .is_err()
    );
    assert_eq!(snapshot(&f), before);
    {
        use orbit_types::tool::{McpCapability, ToolSessionContext};
        let result = f.runtime.execute_tool_command_with_session_context(
            "orbit.workflow.run.resume",
            request.clone(),
            None,
            None,
            ToolSessionContext {
                effective_capabilities: std::collections::BTreeSet::from([McpCapability::Operator]),
                ..Default::default()
            },
        );
        assert!(
            result.is_err(),
            "different authenticated provenance must refuse"
        );
        assert_eq!(snapshot(&f), before);
    }
    let answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    for (field, value) in [
        ("headRefOid", json!(f.base)),
        ("headRefName", json!("other")),
        ("number", json!(18)),
        ("url", json!("https://github.com/foreign/repo/pull/17")),
        ("autoMergeRequest", json!({"enabledAt":"now"})),
    ] {
        let mut changed = answer.clone();
        changed[field] = value;
        std::fs::write(&f.answer, changed.to_string()).unwrap();
        refuse(&f, request.clone(), field);
        std::fs::write(&f.answer, answer.to_string()).unwrap();
    }
    let task = f.runtime.get_task(&f.task).unwrap();
    let successor = f
        .runtime
        .show_job_run_observed(task.job_run_id.as_deref().unwrap())
        .unwrap();
    for run in [f.source.clone(), successor] {
        for case in ["active", "claimed", "unfinished"] {
            let mut changed = run.clone();
            match case {
                "active" => changed.pid = Some(std::process::id()),
                "claimed" => changed.input.as_mut().unwrap()["claimed"] = json!(true),
                "unfinished" => changed.finished_at = None,
                _ => unreachable!(),
            }
            super::published_owner::save_run(&f, &changed);
            refuse(&f, request.clone(), case);
            super::published_owner::save_run(&f, &run);
        }
    }
    let foreign = f.runtime.clone().with_automation_execution_location(Some(
        orbit_types::task::ExecutionLocation {
            machine_id: "hm_foreign".into(),
            machine_name: None,
        },
    ));
    let before = snapshot(&f);
    assert!(
        foreign
            .run_tool_as_human("orbit.workflow.run.resume", request.clone())
            .is_err()
    );
    assert_eq!(
        snapshot(&f),
        before,
        "foreign authority must preserve all work"
    );
    let config = f.repo.join(".orbit/config.toml");
    let original_config = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        original_config.replace("before_landing = false", "before_landing = true"),
    )
    .unwrap();
    f.runtime = orbit_core::OrbitRuntime::from_roots(
        &f._root.path().join("global"),
        &f.repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
        machine_id: "hm_fixture".into(),
        machine_name: None,
    }));
    refuse(&f, request, "current policy drift");
}

fn reconciliation_refuses_missing_ambiguous_and_unreadable_run_evidence() {
    for case in ["missing", "ambiguous", "unreadable", "mismatched"] {
        let (f, _, _) = super::inline_publication::supported_incident();
        let (_, original) = legacy(&f, "");
        plan(&f, "Repair the observed defect");
        let request = reconcile(&f, &original);
        if case == "missing" {
            append(
                &f,
                "source_repair_linked",
                json!({"request_key":"repair-1","run_id":"missing-run"}),
            );
        } else {
            for _ in 0..if case == "ambiguous" { 2 } else { 1 } {
                let mut input =
                    json!({"task_ids":[f.task],"__source_repair":{"request_key":"repair-1"}});
                if case == "unreadable" {
                    input["__source_repair"] = json!({"binding":{}});
                }
                let mut run = f
                    .runtime
                    .insert_job_run(
                        "task_pr_pipeline",
                        2,
                        Utc::now(),
                        Some(input),
                        Some(f.source.run_id.clone()),
                    )
                    .unwrap();
                run.state = JobRunState::Failed;
                run.finished_at = Some(Utc::now());
                super::published_owner::save_run(&f, &run);
            }
        }
        refuse(&f, request, case);
    }
}

fn reconciliation_cannot_replace_an_already_valid_plan() {
    let (f, _, _) = super::inline_publication::supported_incident();
    let (_, original) = legacy(&f, "A previously authorized real execution plan");
    plan(&f, "A different real execution plan");
    let error = refuse(&f, reconcile(&f, &original), "valid plan replacement");
    assert!(
        error.contains("only correction of an empty or placeholder plan"),
        "{error}"
    );
}

crate::scenario_table!(
    source_repair_reconciliation_scenarios,
    legacy_plan_correction_gets_one_fresh_same_pr_authorization,
    reconciliation_lost_spawn_answer_does_not_admit_twice,
    reconciliation_refuses_adjacent_identity_and_meaning_drift,
    reconciliation_rechecks_live_authority_policy_and_lease,
    reconciliation_refuses_missing_ambiguous_and_unreadable_run_evidence,
    reconciliation_cannot_replace_an_already_valid_plan
);
