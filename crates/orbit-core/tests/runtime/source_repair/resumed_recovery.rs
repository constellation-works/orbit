//! A generic resume's final-recovery repair, refused at the commit boundary
//! and then published by the failure hook, is recovered under its original
//! checkpoint owner through the explicit operator path.
use super::failed_publication::{provider_refusals, refused_without_writes};
use super::*;
use orbit_engine::{
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied,
};
use orbit_types::workflow::{FinalRecoveryDecision, FinalRecoveryRepairCommit, PipelineState};

struct Resumed {
    f: Fixture,
    owner: String,
    owner_head: String,
}

impl Resumed {
    fn owner_state(&self) -> Value {
        serde_json::to_value(self.f.runtime.read_run_state(&self.owner).unwrap()).unwrap()
    }
}

/// Owner O commits and is interrupted; generic resume R reuses O's worktree
/// checkpoint, and R's own final recovery commits a repair and resumes the
/// commit step. The commit boundary of the time consulted only O and refused
/// that HEAD; R's failure hook then pushed it and opened the PR.
fn resumed_fixture() -> Resumed {
    let mut f = Fixture::handoff();
    let config = f.repo.join(".orbit/config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "[workflow]",
        "[workflow]\nfinal_recovery_crews = [\"fixture\"]",
    );
    std::fs::write(&config, text).unwrap();
    f.runtime = orbit_core::OrbitRuntime::from_roots(
        &f._root.path().join("global"),
        &f.repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
        machine_id: "hm_fixture".into(),
        machine_name: None,
    }));
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

    let input =
        json!({"task_ids":[f.task],"base_branch":"main","completion":"review","auto_fix":false});
    let mut owner = f
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    owner.state = JobRunState::Running;
    owner.pid = Some(std::process::id());
    published_owner::save_run(&f, &owner);
    let owner_id = owner.run_id.clone();
    f.runtime
        .write_run_state(
            &owner_id,
            &PipelineState::new(owner_id.clone(), owner.job_id.clone(), input.clone()),
        )
        .unwrap();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(owner_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":owner_id,"run_id":owner_id,
        "task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    published_owner::checkpoint(&f, &owner_id, 0, "worktree", setup.clone());
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
    let commit_input = json!({"job_run_id":owner_id,"scope":"all","workspace_path":checkout,
        "base_sha":setup["base_sha"],"base_ref":setup["base_ref"]});
    let commit = f.action("git_commit", commit_input.clone());
    published_owner::checkpoint(&f, &owner_id, 1, "commit", commit);
    let owner_head = git(&checkout, &["rev-parse", "HEAD"]);
    stop(&f, &owner_id, JobRunState::Interrupted);

    // Generic resume: checkpoints are copied and the worktree keeps naming O.
    let resumed = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            2,
            Utc::now(),
            Some(input),
            Some(owner_id.clone()),
        )
        .unwrap()
        .run_id;
    f.runtime
        .mark_job_run_running(&resumed, Utc::now(), std::process::id())
        .unwrap();
    let mut state = f.runtime.read_run_state(&owner_id).unwrap().unwrap();
    state.run_id = resumed.clone();
    state.record_step(
        8,
        JobRunState::Failed,
        None,
        Some(json!("required validation failed")),
    );
    f.runtime.write_run_state(&resumed, &state).unwrap();

    // R's own final recovery: admitted once, repairs, resumes the commit step.
    let admission = FinalRecoveryAdmissionRequest {
        task_id: f.task.clone(),
        failed_step_id: "validate".into(),
        base_ref: Some("origin/main".into()),
    };
    assert_eq!(
        f.runtime
            .admit_final_recovery(&resumed, &admission)
            .unwrap(),
        FinalRecoveryAdmission::Admitted
    );
    std::fs::write(
        checkout.join("qa-fixed.txt"),
        "resumed final-recovery repair\n",
    )
    .unwrap();
    git(&checkout, &["add", "qa-fixed.txt"]);
    git(&checkout, &["commit", "-qm", "final-recovery repair"]);
    let repair = git(&checkout, &["rev-parse", "HEAD"]);
    let application = FinalRecoveryApplication {
        task_id: f.task.clone(),
        failed_step_id: "validate".into(),
        decision: FinalRecoveryDecision::Resume {
            step_id: "commit".into(),
            rationale: "inventory repaired".into(),
        },
        resume_step_index: Some(1),
        repair_commit: Some(FinalRecoveryRepairCommit {
            // Same spelling production `observe_repair_commit` stores. The
            // fixture root comes from `bulk_root`, so this still matches the
            // checkpoint after `worktree remove` drops the directory
            // authentication would otherwise canonicalize.
            workspace_path: checkout.canonicalize().unwrap(),
            head_sha_before: owner_head.clone(),
            head_sha: repair.clone(),
        }),
        resume_context: None,
        workspace_path: checkout.clone(),
        completion_done: false,
        infrastructure: None,
    };
    assert_eq!(
        RuntimeHost::apply_final_recovery(&f.runtime, &resumed, &application).unwrap(),
        FinalRecoveryApplied::Resume
    );

    // The commit boundary of the time read only the owner's state.
    let error = execute_deterministic_action(
        &f.runtime,
        "git_commit",
        &json!({}),
        &commit_input,
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "{error}"
    );
    let mut state = f.runtime.read_run_state(&resumed).unwrap().unwrap();
    state.record_step(1, JobRunState::Failed, None, Some(json!(error.to_string())));
    f.runtime.write_run_state(&resumed, &state).unwrap();
    let handoff = f.action(
        "pr_failure_handoff",
        json!({"run_id":resumed,"failed_step_id":"commit",
        "activity_name":"git_commit","error_code":"execution","error_message":error.to_string(),
        "job_input":state.initial_input,"pipeline":state.pipeline}),
    );
    assert_eq!(handoff["decision"], "blocked_failure_pr");
    assert_eq!(handoff["checkpoint_owner"], owner_id);
    assert_eq!(handoff["preservation_commit_created"], false);
    assert_eq!(handoff["head_sha"], repair);
    state.record_failure_activity("pr_failure_handoff".into(), "commit".into(), handoff);
    f.runtime.write_run_state(&resumed, &state).unwrap();
    stop(&f, &resumed, JobRunState::Failed);
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            },
        )
        .unwrap();
    f.head = repair;
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefName"] = json!(f.branch);
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    f.source = f.runtime.show_job_run_observed(&resumed).unwrap();
    f.state = f.runtime.read_run_state(&resumed).unwrap().unwrap();
    // Recovery reconstructs from durable objects and the remote lease alone.
    git(&f.repo, &["worktree", "remove", checkout.to_str().unwrap()]);
    git(&f.repo, &["branch", "-D", &f.branch]);
    Resumed {
        f,
        owner: owner_id,
        owner_head,
    }
}

/// Finalize through the host, then release the fixture's own live PID: a
/// stopped worker no longer holds it.
fn stop(f: &Fixture, id: &str, state: JobRunState) {
    RuntimeHost::finalize_job_run(&f.runtime, id, state, Utc::now(), None).unwrap();
    let mut run = f.runtime.show_job_run_observed(id).unwrap();
    run.pid = None;
    published_owner::save_run(f, &run);
}

fn stopped_resumed_repair_is_recovered_under_its_checkpoint_owner() {
    let r = resumed_fixture();
    let f = &r.f;
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().job_run_id.as_deref(),
        Some(r.owner.as_str()),
        "generic resume leaves the task bound to the checkpoint owner"
    );
    let binding = f.inspect();
    assert_eq!(binding["head_sha"], f.head);
    assert_eq!(binding["branch"], f.branch);
    assert_eq!(binding["pull_request"], "17");
    assert_eq!(binding["source_run_id"], f.source.run_id);
    assert_eq!(binding["checkpoint_owner"]["run_id"], r.owner);
    assert!(binding.get("superseded").is_none());
    let owner_before = r.owner_state();
    let input = f.submit(binding);
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    assert_eq!(replay["run_id"], id);
    assert_eq!(replay["replayed"], true);
    let mut changed = input;
    changed["source_repair"]["diagnosis"] = json!("different authority");
    assert!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", changed)
            .unwrap_err()
            .to_string()
            .contains("different input")
    );
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"run_id":id,
        "task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":id,"task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"]}),
    );
    assert_eq!(resumed["source_sha"], f.head);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(
        std::fs::read_to_string(checkout.join("qa-fixed.txt")).unwrap(),
        "resumed final-recovery repair\n",
        "the exact repair is adopted, not reimplemented"
    );
    f.action(
        "git_commit",
        json!({"job_run_id":id,"run_id":id,"scope":"all","workspace_path":checkout,
        "base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    let validated = f.action(
        "candidate_validate",
        json!({"job_run_id":id,"completed_task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    assert_eq!(validated["decision"], "passed");
    f.assert_original();
    assert_eq!(
        r.owner_state(),
        owner_before,
        "owner checkpoints stay immutable"
    );
}

fn stopped_resumed_repair_refuses_unauthenticated_lineage() {
    let r = resumed_fixture();
    let f = &r.f;
    let source = f.source.run_id.clone();
    let binding = f.inspect();
    let original = f.state.clone();
    let checkpoint = original.final_recovery.clone().unwrap();
    type Tamper = fn(&mut PipelineState, &str);
    let receipts: [(&str, Tamper); 9] = [
        ("missing_receipt", |s, _| s.final_recovery = None),
        ("foreign_key", |s, _| {
            s.final_recovery.as_mut().unwrap().key.run_id = "foreign-run".into()
        }),
        ("other_task", |s, _| {
            s.final_recovery.as_mut().unwrap().task_id = "foreign-task".into()
        }),
        ("unapplied", |s, _| {
            s.final_recovery.as_mut().unwrap().outcome = None
        }),
        ("escalated", |s, _| {
            s.final_recovery.as_mut().unwrap().decision = Some(FinalRecoveryDecision::Escalate {
                diagnosis: "no".into(),
                human_action: "no".into(),
            })
        }),
        ("no_repair", |s, _| {
            s.final_recovery.as_mut().unwrap().repair_commit = None
        }),
        ("wrong_parent", |s, base| {
            s.final_recovery
                .as_mut()
                .unwrap()
                .repair_commit
                .as_mut()
                .unwrap()
                .head_sha_before = base.into()
        }),
        ("wrong_workspace", |s, _| {
            s.final_recovery
                .as_mut()
                .unwrap()
                .repair_commit
                .as_mut()
                .unwrap()
                .workspace_path = "/elsewhere".into()
        }),
        ("passed_boundary", |s, _| {
            s.step_states.insert(2, JobRunState::Success);
        }),
    ];
    for (case, tamper) in receipts {
        let mut state = original.clone();
        tamper(&mut state, &f.base);
        f.runtime.write_run_state(&source, &state).unwrap();
        refused_without_writes(f, &source, &binding, case);
    }
    // A repair head that is not the published head is an arbitrary commit.
    let mut state = original.clone();
    let repair = state
        .final_recovery
        .as_mut()
        .unwrap()
        .repair_commit
        .as_mut()
        .unwrap();
    repair.head_sha = r.owner_head.clone();
    f.runtime.write_run_state(&source, &state).unwrap();
    refused_without_writes(f, &source, &binding, "arbitrary_head");
    f.runtime.write_run_state(&source, &original).unwrap();
    assert_eq!(
        f.runtime
            .read_run_state(&source)
            .unwrap()
            .unwrap()
            .final_recovery,
        Some(checkpoint)
    );

    // Lineage: the resume must reach its owner through stopped same-job runs.
    let run = f.runtime.show_job_run_observed(&source).unwrap();
    let owner = f.runtime.show_job_run_observed(&r.owner).unwrap();
    let foreign = f
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), run.input.clone(), None)
        .unwrap();
    stop(f, &foreign.run_id, JobRunState::Interrupted);
    for case in ["unrelated", "detached", "live_owner"] {
        match case {
            "unrelated" => {
                let mut changed = run.clone();
                changed.retry_source_run_id = Some(foreign.run_id.clone());
                published_owner::save_run(f, &changed);
            }
            "detached" => {
                let mut changed = run.clone();
                changed.retry_source_run_id = None;
                published_owner::save_run(f, &changed);
            }
            _ => {
                let mut changed = owner.clone();
                changed.state = JobRunState::Running;
                changed.pid = Some(std::process::id());
                changed.finished_at = None;
                published_owner::save_run(f, &changed);
            }
        }
        refused_without_writes(f, &source, &binding, case);
        published_owner::save_run(f, &run);
        published_owner::save_run(f, &owner);
    }

    // A status or binding edit alone grants no authority.
    for rebound in [source.clone(), foreign.run_id.clone()] {
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    job_run_id: Some(rebound),
                    ..Default::default()
                },
            )
            .unwrap();
        refused_without_writes(f, &source, &binding, "rebound");
    }
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                job_run_id: Some(r.owner.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    provider_refusals(f, &source, &binding);
    let task = f.runtime.get_task(&f.task).unwrap();
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
    refused_without_writes(f, &source, &binding, "task meaning");
    f.runtime
        .update_task_with_identity(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some(task.description.clone()),
                ..Default::default()
            },
            None,
            Some("codex".into()),
        )
        .unwrap();

    // The owner's checkpoints are bound: a change after inspection is stale.
    let binding = f.inspect();
    let owner_state = f.runtime.read_run_state(&r.owner).unwrap().unwrap();
    let mut changed = owner_state.clone();
    changed.pipeline["commit"]["note"] = json!("rewritten after inspection");
    f.runtime.write_run_state(&r.owner, &changed).unwrap();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding.clone()))
        .unwrap_err();
    assert!(
        error.to_string().contains("source repair refused"),
        "{error}"
    );
    assert!(
        !f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|e| e.event == "source_repair_reserved"),
        "a changed owner reserves nothing"
    );
    f.runtime.write_run_state(&r.owner, &owner_state).unwrap();

    // Concurrent admissions of one binding produce one successor.
    let input = f.submit(binding);
    let barrier = std::sync::Barrier::new(2);
    let ids = std::thread::scope(|scope| {
        let submit = || {
            barrier.wait();
            f.runtime
                .run_tool_as_human("orbit.workflow.run.resume", input.clone())
                .map(|result| result["run_id"].as_str().unwrap().to_string())
        };
        let first = scope.spawn(submit);
        let second = scope.spawn(submit);
        [first.join().unwrap(), second.join().unwrap()]
    });
    let admitted: std::collections::BTreeSet<_> = ids.iter().flatten().collect();
    assert_eq!(admitted.len(), 1, "{ids:?}");
    for id in &admitted {
        f.wait(id);
    }
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == "source_repair_reserved")
            .count(),
        1
    );
    let mut collision = input;
    collision["source_repair"]["request_key"] = json!("repair-2");
    assert!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", collision)
            .is_err(),
        "a second key cannot reserve the same published head"
    );

    // The admitted run revalidates the bound owner before any source effect.
    let id = admitted.into_iter().next().unwrap();
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"run_id":id,
        "task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    f.runtime.write_run_state(&r.owner, &changed).unwrap();
    let error = execute_deterministic_action(
        &f.runtime,
        "candidate_resume",
        &json!({}),
        &json!({"job_run_id":id,"task_ids":[f.task],
            "workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("original checkpoint owner changed"),
        "{error}"
    );
}

crate::scenario_table!(
    resumed_final_recovery_publication_scenarios,
    stopped_resumed_repair_is_recovered_under_its_checkpoint_owner,
    stopped_resumed_repair_refuses_unauthenticated_lineage
);
