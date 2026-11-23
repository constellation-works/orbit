use super::*;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::workflow::{DeliveryRefusal, JobRun, JobTargetType};

fn held_completion_fixture() -> Fixture {
    held_completion_fixture_with(DeliveryRefusal::CheckEvidenceInconclusive)
}

fn held_completion_fixture_with(code: DeliveryRefusal) -> Fixture {
    held_completion_fixture_with_policy(code, "none")
}

fn held_completion_fixture_with_policy(code: DeliveryRefusal, timing: &str) -> Fixture {
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
                error_code: Some(code.error_code().into()),
                error_message: Some(format!("auto_fix_stopped: {}", code.error_code())),
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

fn setup_fake_codex_implement_pipeline(f: &mut Fixture, captured_prompt: &std::path::Path) {
    use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind};
    use std::os::unix::fs::PermissionsExt;

    let global = f._root.path().join("global");
    // Preserve the source run's original step layout via snapshot before
    // updating `task_pr_pipeline.yaml` to run `implement_bundle` after `resume_candidate`.
    let jobs_dir = global.join("resources/jobs");
    let source_yaml = std::fs::read_to_string(jobs_dir.join("task_pr_pipeline.yaml")).unwrap();
    let job_runs_dir = &f.runtime.paths().job_runs_dir;
    std::fs::create_dir_all(job_runs_dir).unwrap();
    std::fs::write(
        job_runs_dir.join(format!("{}.job.yaml", f.source.run_id)),
        &source_yaml,
    )
    .unwrap();

    let activities_dir = global.join("resources/activities");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/activities/agent_implement.yaml"),
        activities_dir.join("agent_implement.yaml"),
    )
    .unwrap();

    let codex_path = f._root.path().join("bin/codex");
    let envelope = json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {"summary": "applied operator diagnosis repair"},
        "error": null
    })
    .to_string();
    std::fs::write(
        &codex_path,
        format!(
            "#!/bin/sh\ncat > '{}'\ncat <<'STDOUT'\n{envelope}\nSTDOUT\n",
            captured_prompt.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut executor: orbit_types::resource::ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/executors/codex.yaml"),
        )
        .unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(codex_path.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    f.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let repair_yaml = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_pr_pipeline\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - {id: worktree, target: 'activity:worktree_setup', default_input: {task_ids: '{{ input.task_ids }}', base: '{{ input.base_branch }}', base_sync: '{{ input.base_sync }}'}}\n    - {id: resume_candidate, target: 'activity:candidate_resume', default_input: {job_run_id: '{{ steps.worktree.output.job_run_id }}', task_ids: '{{ input.task_ids }}', workspace_path: '{{ steps.worktree.output.workspace_path }}', base_sha: '{{ steps.worktree.output.base_sha }}', prior_job_run_id: '{{ steps.worktree.output.prior_job_run_id }}'}}\n    - id: implement_bundle\n      when: \"{{ steps.resume_candidate.output.implement }} == true\"\n      loop:\n        items: \"{{ input.task_ids }}\"\n        max_iterations: 1\n        steps:\n          - id: implement_one\n            target: activity:agent_implement\n            default_input:\n              task_id: \"{{ item }}\"\n              workspace_path: \"{{ steps.worktree.output.workspace_path }}\"\n              repo_root: \"{{ steps.worktree.output.workspace_path }}\"\n              base_sha: \"{{ steps.worktree.output.base_sha }}\"\n              resume_candidate: \"{{ steps.resume_candidate.output.repair }}\"\n";
    std::fs::write(jobs_dir.join("task_pr_pipeline.yaml"), repair_yaml).unwrap();
    // Reinstall the worker launcher to avoid a bare exit-0 stub; we run the worker in-process.
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
}

fn execute_pending_repair_worker(f: &Fixture, run_id: &str) {
    let mut run = f.runtime.show_job_run_observed(run_id).unwrap();
    run.state = JobRunState::Pending;
    run.started_at = None;
    run.finished_at = None;
    run.duration_ms = None;
    run.pid = None;
    run.pid_start_time = None;
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    f.runtime
        .execute_pipeline_run_worker(run_id)
        .expect("repair worker should execute worktree, candidate_resume and agent_implement");
}

fn assert_captured_prompt_carries_diagnosis(
    captured_prompt: &std::path::Path,
    diagnosis: &str,
    excluded: &[&str],
) {
    let prompt = std::fs::read_to_string(captured_prompt)
        .expect("agent_implement must invoke codex and capture the prompt");
    assert!(
        prompt.contains(diagnosis),
        "repair agent prompt must contain operator diagnosis `{diagnosis}`; got:\n{prompt}"
    );
    let envelope: Value = prompt
        .lines()
        .find_map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .filter(|v| v.get("schemaVersion").is_some())
        })
        .expect("repair agent prompt must contain the execution envelope JSON");
    let resume_output = envelope["input"]["resume_candidate"]["output"]
        .as_str()
        .expect("resume_candidate.output must carry the repair prompt payload");
    assert!(resume_output.contains(diagnosis), "got:\n{resume_output}");
    for forbidden in excluded {
        assert!(
            !resume_output.contains(forbidden),
            "unexpected content `{forbidden}` in repair diagnosis payload: {resume_output}"
        );
    }
}

fn held_review_task_diagnosis_reaches_agent_prompt_and_spends_budget() {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_types::workflow::{PR_AUTO_FIX_BUDGET, source_repair_reservations};

    let mut f = held_completion_fixture();
    let captured_prompt = f._root.path().join("captured-agent-prompt.txt");
    setup_fake_codex_implement_pipeline(&mut f, &captured_prompt);

    let inert_comment = "Inert comment only: should neither trigger nor rewrite repair";
    f.runtime
        .update_task_as_human(
            &f.task,
            TaskUpdateParams {
                comment: Some(inert_comment.into()),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    let runs_before = f.runtime.job_history("task_pr_pipeline").unwrap().len();
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Review
    );
    assert!(
        source_repair_reservations(&f.runtime.get_task_history(&f.task).unwrap())
            .unwrap()
            .is_empty(),
        "a task comment alone must not record a repair or spend budget"
    );

    let diagnosis =
        "Operator diagnosis: fix visibility in source.txt without replacing PR #17 [held-diag-42].";
    let binding = f.inspect();
    let mut submit = f.submit(binding);
    submit["source_repair"]["diagnosis"] = json!(diagnosis);
    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit)
        .unwrap();
    let repair_run_id = admitted["run_id"].as_str().unwrap();
    f.wait(repair_run_id);

    // Posting another comment after submission must not alter the admitted repair.
    f.runtime
        .update_task_as_human(
            &f.task,
            TaskUpdateParams {
                comment: Some("Post-submit inert comment".into()),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();

    let history = f.runtime.get_task_history(&f.task).unwrap();
    let reservations = source_repair_reservations(&history).unwrap();
    assert_eq!(reservations.len(), 1);
    assert_eq!(reservations[0].diagnosis, diagnosis);
    assert_eq!(reservations[0].attempt, 1);
    assert_eq!(reservations[0].binding.pull_request, "17");

    // Verify PR budget decreased by 1 (and adding 2 auto-fix attempts exhausts it).
    assert_eq!(
        f.runtime.job_history("task_pr_pipeline").unwrap().len(),
        runs_before + 1
    );
    let mut exhausted_history = history.clone();
    for i in 0..(PR_AUTO_FIX_BUDGET - 1) {
        exhausted_history.push(orbit_types::task::TaskHistoryEntry {
            at: Utc::now(),
            by: "system".into(),
            event: orbit_types::workflow::PR_AUTO_FIX_ADMITTED_EVENT.into(),
            note: Some(
                json!({
                    "pr_number": "17",
                    "head_sha": format!("{i:040x}"),
                    "blocker": "failed_checks",
                    "attempt": i + 2,
                    "budget": PR_AUTO_FIX_BUDGET,
                    "run_id": format!("autofix-{i}"),
                    "crew": "fixture",
                    "crew_source": "fixture"
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        });
    }
    let remaining_budget = PR_AUTO_FIX_BUDGET.saturating_sub(
        source_repair_reservations(&exhausted_history)
            .unwrap()
            .len() as u32,
    );
    let decision = orbit_types::workflow::decide_pr_auto_fix(
        &orbit_types::workflow::pr_auto_fix_attempts(&exhausted_history, "17"),
        &orbit_types::workflow::PrAutoFixClaim {
            pr_number: "17",
            head_sha: &f.head,
            blocker: orbit_types::workflow::PrAutoFixBlocker::FailedChecks,
            run_id: "next-run",
        },
        remaining_budget,
        "fixture",
        "fixture",
    );
    assert!(
        matches!(
            decision,
            orbit_types::workflow::PrAutoFixDecision::Exhausted(_)
        ),
        "expected PR budget to reflect the spent source repair: {decision:?}"
    );

    execute_pending_repair_worker(&f, repair_run_id);
    assert_captured_prompt_carries_diagnosis(
        &captured_prompt,
        diagnosis,
        &[inert_comment, "Post-submit inert comment"],
    );
    assert_eq!(
        source_repair_reservations(&f.runtime.get_task_history(&f.task).unwrap())
            .unwrap()
            .len(),
        1
    );
}

/// A completion retry of `f.source` that stopped `Held` under `code`, reusing the
/// source's published checkpoints and original job definition snapshot. The
/// task stays bound to `f.source`.
fn insert_held_retry(f: &Fixture, input: Value, code: DeliveryRefusal) -> JobRun {
    let mut retry = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            2,
            Utc::now() + chrono::Duration::seconds(1),
            Some(input),
            Some(f.source.run_id.clone()),
        )
        .unwrap();
    // Preserve the original catalog definition snapshot for the retry as well.
    let job_runs_dir = &f.runtime.paths().job_runs_dir;
    std::fs::create_dir_all(job_runs_dir).unwrap();
    let source_snapshot = job_runs_dir.join(format!("{}.job.yaml", f.source.run_id));
    if !source_snapshot.exists() {
        std::fs::copy(
            f._root
                .path()
                .join("global/resources/jobs/task_pr_pipeline.yaml"),
            &source_snapshot,
        )
        .unwrap();
    }
    std::fs::copy(
        &source_snapshot,
        job_runs_dir.join(format!("{}.job.yaml", retry.run_id)),
    )
    .unwrap();
    retry.state = JobRunState::Held;
    retry.finished_at = Some(Utc::now() + chrono::Duration::seconds(2));
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &retry, None)
        .unwrap();
    let mut retry_state = f.state.clone();
    retry_state.run_id = retry.run_id.clone();
    f.runtime
        .write_run_state(&retry.run_id, &retry_state)
        .unwrap();
    f.runtime
        .complete_job_run_step(
            &retry.run_id,
            &JobRunStepParams {
                step_index: 6,
                target_type: JobTargetType::Job,
                target_id: retry.job_id.clone(),
                state: JobRunState::Held,
                started_at: retry.created_at,
                finished_at: retry.finished_at.unwrap(),
                duration_ms: None,
                exit_code: None,
                lock_wait_ms: None,
                agent_response_json: None,
                error_code: Some(code.error_code().into()),
                error_message: Some(format!("auto_fix_stopped: {}", code.error_code())),
            },
        )
        .unwrap();
    retry
}

fn held_completion_retry_on_review_task_carries_diagnosis_and_follows_pushed_head() {
    use orbit_types::workflow::{
        PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixBlocker, PrAutoFixPush, source_repair_reservations,
    };

    let mut f = held_completion_fixture();
    let captured_prompt = f._root.path().join("captured-retry-prompt.txt");
    setup_fake_codex_implement_pipeline(&mut f, &captured_prompt);

    // Advance the branch via an auto-fix push recorded on a completion retry run R2.
    std::fs::write(f.repo.join("source.txt"), "auto-fix attempt before hold\n").unwrap();
    git(&f.repo, &["add", "source.txt"]);
    git(&f.repo, &["commit", "-qm", "auto-fix repair"]);
    let advanced_head = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["push", "-q", "origin", &f.branch]);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(advanced_head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();

    let retry = insert_held_retry(
        &f,
        json!({"task_ids":[f.task],"completion":"done","auto_fix":true}),
        DeliveryRefusal::RepairDispatched,
    );
    let push_note = serde_json::to_string(&PrAutoFixPush {
        pr_number: "17".into(),
        from_head_sha: f.head.clone(),
        to_head_sha: advanced_head.clone(),
        blocker: PrAutoFixBlocker::FailedChecks,
        run_id: retry.run_id.clone(),
    })
    .unwrap();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: PR_AUTO_FIX_PUSHED_EVENT.into(),
                    note: Some(push_note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    // task.job_run_id remains bound to the checkpoint owner f.source.run_id, while
    // the held run presented to the operator is `retry.run_id`.
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().job_run_id.as_deref(),
        Some(f.source.run_id.as_str())
    );

    let inspect_req = json!({
        "id": retry.run_id,
        "source_repair": {"action": "inspect", "task_id": f.task}
    });
    let inspected = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", inspect_req)
        .unwrap();
    assert_eq!(inspected["binding"]["source_run_id"], retry.run_id);
    assert_eq!(inspected["binding"]["head_sha"], advanced_head);

    let diagnosis =
        "Operator diagnosis on held retry: update test fixture threshold [retry-diag-7].";
    let submit_req = json!({
        "id": retry.run_id,
        "source_repair": {
            "action": "submit",
            "task_id": f.task,
            "request_key": "held-retry-1",
            "binding": inspected["binding"],
            "diagnosis": diagnosis
        }
    });
    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit_req)
        .unwrap();
    let repair_run_id = admitted["run_id"].as_str().unwrap();
    f.wait(repair_run_id);

    let reservations =
        source_repair_reservations(&f.runtime.get_task_history(&f.task).unwrap()).unwrap();
    assert_eq!(reservations.len(), 1);
    assert_eq!(reservations[0].diagnosis, diagnosis);
    assert_eq!(reservations[0].attempt, 1);

    execute_pending_repair_worker(&f, repair_run_id);
    assert_captured_prompt_carries_diagnosis(&captured_prompt, diagnosis, &[]);
}

fn held_review_task_empty_plan_refuses_naming_missing_plan_and_update_call() {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_types::workflow::source_repair_reservations;

    for missing in ["", "To be authored by executing agent at start time."] {
        let f = held_completion_fixture();
        f.runtime
            .update_task_with_identity(
                &f.task,
                TaskUpdateParams {
                    plan: Some(missing.into()),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        let binding = f.inspect();
        let history_before = f.runtime.get_task_history(&f.task).unwrap();
        let runs_before = f.runtime.job_history("task_pr_pipeline").unwrap().len();

        let error = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding.clone()))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("execution plan") && error.contains("orbit.task.update"),
            "refusal must name missing plan and one-call fix: {error}"
        );
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Review
        );
        assert_eq!(f.runtime.get_task_history(&f.task).unwrap(), history_before);
        assert_eq!(
            f.runtime.job_history("task_pr_pipeline").unwrap().len(),
            runs_before
        );

        // Apply the named one-call fix and confirm the submission now succeeds.
        f.runtime
            .run_tool(
                "orbit.task.update",
                json!({"id": f.task, "plan": "Apply operator diagnosis and revalidate PR #17"}),
            )
            .unwrap();
        let admitted = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.submit(f.inspect()))
            .unwrap();
        f.wait(admitted["run_id"].as_str().unwrap());
        assert_eq!(
            source_repair_reservations(&f.runtime.get_task_history(&f.task).unwrap())
                .unwrap()
                .len(),
            1
        );
    }
}

/// The owner `f.source` held as `delivery_completion_budget_expired`, then the
/// drain's out-of-run auto-fix hand-back held as `delivery_repair_dispatched`.
fn owner_held_with_handback_held() -> (Fixture, JobRun) {
    let f = held_completion_fixture_with(DeliveryRefusal::CompletionBudgetExpired);
    let handback = insert_held_retry(
        &f,
        json!({
            "task_ids": [f.task],
            "completion": "done",
            "auto_fix": true,
            "footprint_deferred": true,
            "out_of_run_auto_fix": {
                "pr_number": "17",
                "blocker": "failed_checks",
                "head_sha": f.head,
                "source_run_id": f.source.run_id,
                "drain_run_id": "drain-1",
            },
        }),
        DeliveryRefusal::RepairDispatched,
    );
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().job_run_id.as_deref(),
        Some(f.source.run_id.as_str()),
        "a resume leaves the task bound to the owner"
    );
    (f, handback)
}

fn handback_request(f: &Fixture, run_id: &str, action: &str) -> Value {
    json!({"id": run_id, "source_repair": {"action": action, "task_id": f.task}})
}

fn spend_pr_auto_fix_budget(f: &Fixture) {
    use orbit_types::workflow::{PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_BUDGET};
    let entries = (0..PR_AUTO_FIX_BUDGET)
        .map(|i| orbit_types::task::TaskHistoryEntry {
            at: Utc::now(),
            by: "system".into(),
            event: PR_AUTO_FIX_ADMITTED_EVENT.into(),
            note: Some(
                json!({
                    "pr_number": "17",
                    "head_sha": format!("{i:040x}"),
                    "blocker": "failed_checks",
                    "attempt": i + 1,
                    "budget": PR_AUTO_FIX_BUDGET,
                    "run_id": format!("autofix-{i}"),
                    "crew": "fixture",
                    "crew_source": "fixture"
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        })
        .collect();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: entries,
                ..Default::default()
            },
        )
        .unwrap();
}

fn held_owner_and_held_auto_fix_handback_admit_repair_on_the_handback() {
    let (f, handback) = owner_held_with_handback_held();
    let inspected = f
        .runtime
        .run_tool_as_human(
            "orbit.workflow.run.resume",
            handback_request(&f, &handback.run_id, "inspect"),
        )
        .unwrap();
    assert_eq!(inspected["binding"]["source_run_id"], handback.run_id);
    assert_eq!(inspected["binding"]["head_sha"], f.head);
    let mut submit = handback_request(&f, &handback.run_id, "submit");
    submit["source_repair"]["binding"] = inspected["binding"].clone();
    submit["source_repair"]["request_key"] = json!("handback-1");
    submit["source_repair"]["diagnosis"] = json!("Repair the held hand-back's published head.");
    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit)
        .unwrap();
    assert_eq!(admitted["retry_source_run_id"], handback.run_id);
}

fn held_auto_fix_handback_with_spent_budget_refuses_as_budget_exhausted() {
    let (f, handback) = owner_held_with_handback_held();
    spend_pr_auto_fix_budget(&f);
    let inspected = f
        .runtime
        .run_tool_as_human(
            "orbit.workflow.run.resume",
            handback_request(&f, &handback.run_id, "inspect"),
        )
        .unwrap();
    let mut submit = handback_request(&f, &handback.run_id, "submit");
    submit["source_repair"]["binding"] = inspected["binding"].clone();
    submit["source_repair"]["request_key"] = json!("handback-spent");
    submit["source_repair"]["diagnosis"] = json!("Repair the held hand-back's published head.");
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("pull request repair budget exhausted"),
        "{error}"
    );
}

fn held_owner_refused_while_its_handback_is_newer_names_the_handback() {
    let (f, handback) = owner_held_with_handback_held();
    let error = f
        .runtime
        .run_tool_as_human(
            "orbit.workflow.run.resume",
            handback_request(&f, &f.source.run_id, "inspect"),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("newer execution makes publication association ambiguous")
            && error.contains(&handback.run_id)
            && error.contains("source_repair"),
        "{error}"
    );
}

fn held_run_outside_the_task_lineage_is_still_refused_as_unbound() {
    let f = held_completion_fixture_with(DeliveryRefusal::CompletionBudgetExpired);
    let mut stray = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            2,
            Utc::now() + chrono::Duration::seconds(1),
            Some(json!({"task_ids":[f.task],"completion":"done","auto_fix":true})),
            None,
        )
        .unwrap();
    stray.state = JobRunState::Held;
    stray.finished_at = Some(Utc::now() + chrono::Duration::seconds(2));
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &stray, None)
        .unwrap();
    let error = f
        .runtime
        .run_tool_as_human(
            "orbit.workflow.run.resume",
            handback_request(&f, &stray.run_id, "inspect"),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("held publication is no longer the task's bound run"),
        "{error}"
    );
}

fn resident_starts_preserved_delivery_on_current_base() {
    use crate::resident_cycle::{call, resident_test_broker};
    use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
    use orbit_core::runtime::plugin::broker::{ForwardCallError, forward_call_with_status};
    use orbit_engine::PluginBrokerHandle;
    use orbit_types::task::TaskArtifact;
    use orbit_types::tool::McpCapability;
    use orbit_types::workflow::{SOURCE_REPAIR_KEY, SourceRepairAdmission};

    let f = held_completion_fixture_with_policy(
        DeliveryRefusal::CheckEvidenceInconclusive,
        "before-pr",
    );
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
    held_review_task_diagnosis_reaches_agent_prompt_and_spends_budget,
    held_completion_retry_on_review_task_carries_diagnosis_and_follows_pushed_head,
    held_review_task_empty_plan_refuses_naming_missing_plan_and_update_call,
    held_owner_and_held_auto_fix_handback_admit_repair_on_the_handback,
    held_auto_fix_handback_with_spent_budget_refuses_as_budget_exhausted,
    held_owner_refused_while_its_handback_is_newer_names_the_handback,
    held_run_outside_the_task_lineage_is_still_refused_as_unbound,
    resident_starts_preserved_delivery_on_current_base,
);
