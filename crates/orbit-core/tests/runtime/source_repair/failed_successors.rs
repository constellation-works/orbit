//! Failed pre-adoption successors are acknowledged explicitly, as a chain.
//!
//! A published owner-local source can be followed by runs that failed at
//! candidate adoption, each settled by final recovery, before an operator
//! inspects explicit source repair. Every such binding is set aside only
//! while it provably produced no source; any other newer run still refuses.
use super::failed_publication::escalate;
use super::published_owner::{next, publication, requeue, save_run};
use super::*;
use orbit_types::workflow::JobRun;

/// The fixture job's `resume_candidate` (candidate adoption) step and the
/// step after it.
const ADOPTION: u32 = 6;
const AFTER_ADOPTION: u32 = 7;

/// Record one step-start audit row for `run`, as the host writes before the
/// step can checkpoint.
fn audit_started(f: &Fixture, run: &JobRun, step_id: &str) {
    use orbit_types::workflow::activity_job::{V2AuditEnvelope, V2AuditEvent, V2AuditEventKind};
    let kind = V2AuditEventKind::StepStarted {
        step_id: step_id.into(),
    };
    let event = V2AuditEvent {
        envelope: V2AuditEnvelope {
            schema_version: 1,
            event_type: kind.event_type().into(),
            event_id: format!("{}-{step_id}-started", run.run_id),
            ts: Utc::now(),
            run_id: run.run_id.clone(),
            agent_identity: "system".into(),
            parent_event_id: None,
            workspace_path: None,
        },
        kind,
    };
    f.runtime
        .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
            workspace_id: f.runtime.workspace_id().unwrap(),
            event_id: event.envelope.event_id.clone(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: event.envelope.event_type.clone(),
            ts: event.envelope.ts,
            run_id: run.run_id.clone(),
            agent_identity: "system".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: serde_json::to_string(&event).unwrap(),
        })
        .unwrap();
}

/// A dispatched run that set up its worktree, failed at candidate adoption
/// and stopped, with its final recovery settled when `settle` is set.
fn failed_pre_adoption(f: &Fixture, settle: bool) -> (JobRun, PathBuf) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
    let (mut run, setup) = next(f);
    let mut state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    state.record_step(ADOPTION, JobRunState::Failed, None, None);
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    for step in ["worktree", "resume_candidate"] {
        audit_started(f, &run, step);
    }
    run.state = JobRunState::Failed;
    run.pid = None;
    run.finished_at = Some(Utc::now());
    save_run(f, &run);
    if settle {
        escalate(f, &run.run_id);
    }
    (
        run,
        PathBuf::from(setup["workspace_path"].as_str().unwrap()),
    )
}

#[track_caller]
/// Record a host step row for `run`, or clear its rows when `target` is
/// `None`.
fn step_row(f: &Fixture, run: &JobRun, index: usize, target: Option<&str>) {
    let Some(target) = target else {
        f.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute("DELETE FROM job_run_steps WHERE run_id = ?1", [&run.run_id])
                    .unwrap();
                Ok(())
            })
            .unwrap();
        return;
    };
    f.runtime
        .complete_job_run_step(
            &run.run_id,
            &orbit_store::contracts::JobRunStepParams {
                step_index: index,
                target_type: orbit_types::workflow::JobTargetType::Activity,
                target_id: target.into(),
                state: JobRunState::Failed,
                started_at: Utc::now(),
                finished_at: Utc::now(),
                duration_ms: Some(1),
                exit_code: Some(1),
                lock_wait_ms: None,
                agent_response_json: None,
                error_code: None,
                error_message: Some("failed".into()),
            },
        )
        .unwrap();
}

fn refused_inspection(f: &Fixture, case: &str) -> String {
    match f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
    {
        Ok(inspection) => panic!("{case}: inspection was accepted: {inspection}"),
        Err(error) => error.to_string(),
    }
}

pub(super) fn failed_pre_adoption_chain_rebinds_the_original_publication() {
    let mut f = Fixture::new();
    publication(&mut f);
    requeue(&mut f);
    let source_before =
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let (mut first, _) = failed_pre_adoption(&f, true);
    let (second, _) = failed_pre_adoption(&f, true);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().job_run_id.as_deref(),
        Some(second.run_id.as_str())
    );

    let binding = f.inspect();
    assert_eq!(binding["source_run_id"], f.source.run_id);
    assert_eq!(binding["head_sha"], f.head);
    assert_eq!(
        binding["superseded"]["run_id"], second.run_id,
        "the current binding names the acknowledged chain"
    );
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
        .unwrap();
    assert_eq!(result["retry_source_run_id"], f.source.run_id);
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let resume_input = json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]});
    let resumed = f.action("candidate_resume", resume_input.clone());
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");

    // The digest covers the older member too, not only the current binding.
    first.duration_ms = Some(42);
    save_run(&f, &first);
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
        refused.to_string().contains("newer execution changed"),
        "a change to an earlier chain member invalidates the binding: {refused}"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        source_before,
        "the original publication's evidence is untouched"
    );
}

pub(super) fn failed_successor_chain_keeps_every_neighboring_refusal() {
    let mut f = Fixture::new();
    publication(&mut f);
    requeue(&mut f);
    let (first, first_checkout) = failed_pre_adoption(&f, false);
    let (second, _) = failed_pre_adoption(&f, false);

    // 6: no settled final recovery, on the current binding and then on an
    // earlier member (7).
    let refused = refused_inspection(&f, "unsettled current binding");
    assert!(refused.contains("no settled final recovery"), "{refused}");
    escalate(&f, &second.run_id);
    let refused = refused_inspection(&f, "unsettled earlier member");
    assert!(
        refused.contains("no settled final recovery"),
        "every member of the chain is checked: {refused}"
    );
    escalate(&f, &first.run_id);
    let accepted = f.inspect();
    assert_eq!(accepted["superseded"]["run_id"], second.run_id);

    let first_state = f.runtime.read_run_state(&first.run_id).unwrap().unwrap();
    let second_state = f.runtime.read_run_state(&second.run_id).unwrap().unwrap();
    let stopped = "conclusively stopped cancelled or failed owner-local replacement";
    let evidence = "candidate/publication evidence";
    let passed = "passed candidate adoption";
    type Case = (
        &'static str,
        bool,
        fn(&mut JobRun, &mut orbit_types::workflow::PipelineState),
    );
    let cases: [(Case, &str); 14] = [
        // 1: live or not conclusively stopped.
        (
            ("live", false, |run, _| run.pid = Some(std::process::id())),
            stopped,
        ),
        (
            ("unfinished", true, |run, _| run.finished_at = None),
            stopped,
        ),
        (
            ("running", false, |run, _| run.state = JobRunState::Running),
            stopped,
        ),
        // 2: claimed or claimed-job runs.
        (
            ("claimed", true, |run, _| {
                run.input.as_mut().unwrap()["claimed"] = json!(true)
            }),
            stopped,
        ),
        (
            ("claimed job", true, |run, _| {
                run.job_id = "task_claimed_pr_pipeline".into()
            }),
            stopped,
        ),
        // 3: carried tasks, job, origin and order.
        (
            ("mixed tasks", false, |run, _| {
                run.input =
                    Some(json!({"task_ids":[run.input.as_ref().unwrap()["task_ids"][0], "ORB-0"]}))
            }),
            stopped,
        ),
        (
            ("local job", true, |run, _| {
                run.job_id = "task_local_pipeline".into()
            }),
            stopped,
        ),
        (
            ("older than source", false, |run, _| {
                run.created_at -= chrono::Duration::days(1)
            }),
            stopped,
        ),
        // 4: candidate or publication evidence, including the failure handoff.
        (
            ("commit checkpoint", true, |_, state| {
                state.record_pipeline_output("commit", json!({"committed":true}))
            }),
            evidence,
        ),
        (
            ("handoff publication", false, |_, state| {
                state.record_failure_activity(
                    "pr_failure_handoff".into(),
                    "resume_candidate".into(),
                    json!({"phase":"failure_handoff","decision":"published"}),
                )
            }),
            evidence,
        ),
        (
            ("commit action", true, |_, state| {
                state.record_step(1, JobRunState::Failed, None, None)
            }),
            "candidate/publication action",
        ),
        // 5: a run that adopted a candidate or went past adoption.
        (
            ("adopted", true, |_, state| {
                state.record_step(ADOPTION, JobRunState::Success, None, None)
            }),
            passed,
        ),
        (
            ("past adoption", false, |_, state| {
                state.record_step(AFTER_ADOPTION, JobRunState::Failed, None, None)
            }),
            passed,
        ),
        // A failure handoff that found nothing to publish stays acceptable,
        // so this row must not refuse.
        (
            ("no-candidate handoff", true, |_, state| {
                state.record_failure_activity(
                    "pr_failure_handoff".into(),
                    "resume_candidate".into(),
                    json!({"phase":"failure_handoff","decision":"no_candidate"}),
                )
            }),
            "",
        ),
    ];
    for ((case, older, mutate), expected) in cases {
        let (member, member_state) = if older {
            (&first, &first_state)
        } else {
            (&second, &second_state)
        };
        let mut run = f.runtime.show_job_run_observed(&member.run_id).unwrap();
        let mut state = member_state.clone();
        mutate(&mut run, &mut state);
        save_run(&f, &run);
        f.runtime.write_run_state(&member.run_id, &state).unwrap();
        if expected.is_empty() {
            assert_eq!(f.inspect()["superseded"]["run_id"], second.run_id, "{case}");
        } else {
            let refused = refused_inspection(&f, case);
            assert!(refused.contains(expected), "{case}: {refused}");
        }
        save_run(&f, member);
        f.runtime
            .write_run_state(&member.run_id, member_state)
            .unwrap();
    }
    f.inspect();

    // 2 and 3: an earlier member that ran on another machine.
    let origin = |machine: &str| {
        f.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                        [
                            json!({"machine_id":machine,"machine_name":null}).to_string(),
                            first.run_id.clone(),
                        ],
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
    };
    origin("hm_foreign");
    let refused = refused_inspection(&f, "foreign origin");
    assert!(refused.contains(stopped), "{refused}");
    origin("hm_fixture");
    f.inspect();

    // 5: a host row naming a step after adoption refuses; the worker's
    // job-level diagnostic row, indexed after every step, does not.
    step_row(
        &f,
        &first,
        AFTER_ADOPTION as usize + 1,
        Some("task_pr_pipeline"),
    );
    f.inspect();
    step_row(&f, &first, 0, Some("complete_pr"));
    let refused = refused_inspection(&f, "later step row");
    assert!(refused.contains(passed), "{refused}");
    step_row(&f, &first, 0, None);
    f.inspect();

    // 5: work a failed member left uncommitted is a resume candidate.
    let retained = first_checkout.join("retained.txt");
    std::fs::write(&retained, "unfinished newer work\n").unwrap();
    let refused = refused_inspection(&f, "uncommitted work");
    assert!(refused.contains("left uncommitted work"), "{refused}");
    std::fs::remove_file(&retained).unwrap();
    f.inspect();

    // 8: the publication itself is still authenticated against the forge.
    let original: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    let mut moved = original.clone();
    moved["headRefOid"] = json!(f.base);
    std::fs::write(&f.answer, moved.to_string()).unwrap();
    let refused = refused_inspection(&f, "moved head");
    assert!(refused.contains("refused"), "{refused}");
    std::fs::write(&f.answer, original.to_string()).unwrap();
    f.inspect();

    // 4 and 5 before checkpointing: step-start audit rows are permanent, so
    // they come last. The current binding is read first.
    audit_started(&f, &first, "complete_pr");
    let refused = refused_inspection(&f, "implementation audit");
    assert!(
        refused.contains("may have started implementation before checkpointing"),
        "{refused}"
    );
    audit_started(&f, &second, "commit");
    let refused = refused_inspection(&f, "commit audit");
    assert!(
        refused.contains("may have written a candidate before checkpointing"),
        "{refused}"
    );
    assert!(
        !f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "source_repair_reserved"),
        "inspection reserves nothing"
    );
}
