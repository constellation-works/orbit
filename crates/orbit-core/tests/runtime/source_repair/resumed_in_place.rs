//! A final-recovery `resume` applied in the publishing run itself, then a later
//! auto-fix publication and failure with an expired completion deadline, still
//! leaves the published PR repairable through explicit fresh operator authority.
use super::inline_publication::{refused_without_effects, reload, rework};
use super::published_owner::{move_published_branch, publication, record_auto_fix_push, save_run};
use super::*;
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryDecision, FinalRecoveryKey, FinalRecoveryRepairCommit,
    PR_AUTO_FIX_ADMITTED_EVENT,
};
use std::path::Path;

/// The failed run's inherited receipt, as the engine records an applied `resume`.
fn resume_receipt(f: &Fixture) -> FinalRecoveryCheckpoint {
    FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: f.source.run_id.clone(),
            attempt: 1,
        },
        failed_step_id: "complete_pr".into(),
        task_id: f.task.clone(),
        observed: None,
        base_ref: Some("origin/main".into()),
        admitted_at: Utc::now(),
        decision: Some(FinalRecoveryDecision::Resume {
            step_id: "complete_pr".into(),
            rationale: "the completion step can observe the published PR again".into(),
        }),
        repair_commit: None,
        resume_context: None,
        outcome: Some("resume".into()),
    }
}

fn record_admitted_attempt(f: &Fixture, attempt: u32, head: &str) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: PR_AUTO_FIX_ADMITTED_EVENT.into(),
                    note: Some(
                        json!({"pr_number":"17","head_sha":head,"blocker":"failed_checks",
                            "attempt":attempt,"budget":3,"run_id":f.source.run_id,
                            "crew":"fixture","crew_source":"pool"})
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

/// Publication, an applied in-run final-recovery `resume`, two admitted
/// auto-fix attempts (the first published a repaired head, the second failed),
/// an expired completion deadline, and the run's failure hook keeping the
/// publication. Returns the fixture, its retained checkout, the originally
/// published head and the repaired head the PR carries now.
fn incident() -> (Fixture, PathBuf, String, String) {
    let mut f = Fixture::new();
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let yaml = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        yaml.replace(
            "  task_delivery: {}",
            "  task_delivery: {}\n  failure_activity: pr_failure_handoff",
        ),
    )
    .unwrap();
    let checkout = publication(&mut f);
    let published = f.head.clone();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.final_recovery = Some(resume_receipt(&f));
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    record_admitted_attempt(&f, 1, &published);
    let repaired = move_published_branch(&f, &checkout, "repaired check\n");
    record_auto_fix_push(&f, "system", &published, &repaired, &f.source.run_id);
    record_admitted_attempt(&f, 2, &repaired);

    let output = f.action("pr_failure_handoff", json!({"run_id":f.source.run_id,
        "failed_step_id":"complete_pr","activity_name":"pr_complete","error_code":"execution",
        "error_message":"completion deadline expired","job_input":f.source.input,"pipeline":state.pipeline}));
    assert_eq!(output["decision"], "review_completion_failure");
    assert_eq!(output["candidate_preserved"], true);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), repaired);
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.completion_deadlines.insert("17".into(), 1);
    state.record_step(
        7,
        JobRunState::Failed,
        None,
        Some(json!("completion deadline expired")),
    );
    state.record_failure_activity("pr_failure_handoff".into(), "complete_pr".into(), output);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.source.state = JobRunState::Failed;
    f.source.pid = None;
    f.source.finished_at = Some(Utc::now());
    save_run(&f, &f.source);
    f.state = state;
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    (f, checkout, published, repaired)
}

fn history_events(f: &Fixture, event: &str) -> usize {
    f.runtime
        .get_task_history(&f.task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == event)
        .count()
}

fn resumed_in_place_publication_gets_fresh_same_pr_repair() {
    let (f, _, published, repaired) = incident();
    assert_ne!(published, repaired);
    let original = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let ledger = f.runtime.get_task_history(&f.task).unwrap();
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    // The binding is the PR's current head, proven through the host-recorded
    // auto-fix chain, never the head the source originally pushed.
    assert_eq!(inspection["binding"]["source_run_id"], f.source.run_id);
    assert_eq!(inspection["binding"]["head_sha"], repaired);
    assert_eq!(inspection["binding"]["pull_request"], "17");
    // The lineage digest binds the source itself: no later run exists.
    assert_eq!(
        inspection["binding"]["superseded"]["run_id"],
        f.source.run_id
    );
    assert!(inspection["policy_digest"].is_string());

    // Bare failure evidence is not authority: fresh review-only rework needs
    // explicit acceptance evidence and the inspected current policy.
    let bare = f.submit(inspection["binding"].clone());
    let error = refused_without_effects(&f, bare, "no explicit rework authority");
    assert!(error.contains("unmet_acceptance"), "{error}");

    let input = rework(&f);
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    assert_eq!(result["completion"], "review");
    assert_eq!(result["replayed"], false);
    f.wait(id);
    let run = f.runtime.show_job_run_observed(id).unwrap();
    let admitted = run.input.as_ref().unwrap();
    assert_eq!(
        run.retry_source_run_id.as_deref(),
        Some(f.source.run_id.as_str())
    );
    assert_eq!(admitted["completion"], "review");
    assert_eq!(admitted["auto_fix"], false);
    assert!(admitted.get("out_of_run_auto_fix").is_none());
    assert_eq!(admitted["__source_repair"]["binding"]["head_sha"], repaired);
    // Two attempts were spent of the PR's budget of three; the explicit
    // generation is the third and resets nothing.
    assert_eq!(admitted["__source_repair"]["attempt"], 3);
    assert_eq!(admitted["review"], admitted["__source_repair"]["review"]);
    let fresh = f.runtime.read_run_state(id).unwrap().unwrap();
    assert!(fresh.step_states.is_empty());
    assert!(fresh.completion_deadlines.is_empty());
    assert!(fresh.final_recovery.is_none());

    // The historical receipt, expired deadline, hook and every ledger entry
    // stay exactly as the host wrote them.
    let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    assert_eq!(after, original);
    assert_eq!(after.completion_deadlines["17"], 1);
    assert_eq!(after.final_recovery, Some(resume_receipt_of(&original)));
    assert_eq!(history_events(&f, PR_AUTO_FIX_ADMITTED_EVENT), 2);
    assert_eq!(
        history_events(&f, orbit_types::workflow::PR_AUTO_FIX_PUSHED_EVENT),
        1
    );
    for entry in &ledger {
        assert!(
            f.runtime
                .get_task_history(&f.task)
                .unwrap()
                .iter()
                .any(|kept| kept.at == entry.at
                    && kept.event == entry.event
                    && kept.note == entry.note),
            "{} was rewritten",
            entry.event
        );
    }

    // Idempotent: replaying the same submission returns the same run and
    // starts no second worker.
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input)
        .unwrap();
    assert_eq!(replay["run_id"], id);
    assert_eq!(history_events(&f, "source_repair_reserved"), 1);
    let calls = std::fs::read_to_string(f._root.path().join("gh-calls")).unwrap_or_default();
    assert!(!calls.lines().any(|line| line.starts_with("pr merge ")));
}

fn resume_receipt_of(state: &orbit_types::workflow::PipelineState) -> FinalRecoveryCheckpoint {
    state.final_recovery.clone().unwrap()
}

fn resumed_in_place_publication_refuses_unproven_shapes_without_effects() {
    let (mut f, checkout, published, repaired) = incident();
    let request = rework(&f);
    let original = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();

    // The resume receipt is the only thing that makes this failed run
    // repairable here; every other receipt shape keeps the refusal.
    for case in [
        "no receipt",
        "another run",
        "another task",
        "not applied",
        "requeue",
        "repair commit",
    ] {
        let mut changed = original.clone();
        let receipt = changed.final_recovery.as_mut().unwrap();
        match case {
            "no receipt" => changed.final_recovery = None,
            "another run" => receipt.key.run_id = "jrun-elsewhere".into(),
            "another task" => receipt.task_id = "ORB-0".into(),
            "not applied" => receipt.outcome = None,
            "requeue" => {
                receipt.decision = Some(FinalRecoveryDecision::Requeue {
                    reason: "retry".into(),
                });
                receipt.outcome = Some("requeued".into());
            }
            "repair commit" => {
                receipt.repair_commit = Some(FinalRecoveryRepairCommit {
                    workspace_path: checkout.clone(),
                    head_sha_before: published.clone(),
                    head_sha: repaired.clone(),
                });
            }
            _ => unreachable!(),
        }
        f.runtime
            .write_run_state(&f.source.run_id, &changed)
            .unwrap();
        refused_without_effects(&f, f.request("inspect"), case);
        refused_without_effects(&f, request.clone(), case);
        f.runtime
            .write_run_state(&f.source.run_id, &original)
            .unwrap();
    }
    // The failure hook's kept receipt is required as well.
    let mut changed = original.clone();
    changed.failure_activity_checkpoint = None;
    f.runtime
        .write_run_state(&f.source.run_id, &changed)
        .unwrap();
    refused_without_effects(&f, f.request("inspect"), "missing kept receipt");
    // So is the host publication proof.
    let mut changed = original.clone();
    changed.pipeline.as_object_mut().unwrap().remove("push");
    f.runtime
        .write_run_state(&f.source.run_id, &changed)
        .unwrap();
    refused_without_effects(&f, f.request("inspect"), "missing publication proof");
    f.runtime
        .write_run_state(&f.source.run_id, &original)
        .unwrap();

    // Active, claimed, unfinished and foreign sources refuse.
    for case in ["active", "claimed", "unfinished"] {
        let mut changed = f.source.clone();
        match case {
            "active" => changed.pid = Some(std::process::id()),
            "claimed" => changed.input.as_mut().unwrap()["claimed"] = json!(true),
            "unfinished" => changed.finished_at = None,
            _ => unreachable!(),
        }
        save_run(&f, &changed);
        refused_without_effects(&f, f.request("inspect"), case);
        refused_without_effects(&f, request.clone(), case);
        save_run(&f, &f.source);
    }
    let set_origin = |machine: &str| {
        f.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                        [
                            json!({"machine_id":machine,"machine_name":null}).to_string(),
                            f.source.run_id.clone(),
                        ],
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
    };
    let origin = f.source.executed_on.clone().unwrap();
    set_origin("hm_foreign");
    refused_without_effects(&f, f.request("inspect"), "foreign source");
    set_origin(&origin.machine_id);

    // A moved head refuses unless the host's own ledger recorded the move.
    let answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    for (field, value, case) in [
        ("headRefOid", json!(f.base), "changed provider head"),
        ("state", json!("MERGED"), "merged"),
        ("state", json!("CLOSED"), "closed"),
        ("autoMergeRequest", json!({"enabledAt":"now"}), "auto merge"),
    ] {
        let mut changed = answer.clone();
        changed[field] = value;
        std::fs::write(&f.answer, changed.to_string()).unwrap();
        refused_without_effects(&f, f.request("inspect"), case);
        refused_without_effects(&f, request.clone(), case);
        std::fs::write(&f.answer, answer.to_string()).unwrap();
    }
    let moved = move_published_branch(&f, &checkout, "unrecorded edit\n");
    refused_without_effects(&f, f.request("inspect"), "unrecorded branch move");
    record_auto_fix_push(&f, "human:someone", &repaired, &moved, &f.source.run_id);
    record_auto_fix_push(&f, "system", &repaired, &moved, "jrun-elsewhere");
    refused_without_effects(
        &f,
        f.request("inspect"),
        "move recorded by another run or actor",
    );
    // The attempt that moved the head is the source's own ledger entry; once it
    // is recorded, the new head is the proven one and the old binding is stale.
    record_auto_fix_push(&f, "system", &repaired, &moved, &f.source.run_id);
    let error = refused_without_effects(&f, request.clone(), "stale binding after recorded push");
    assert!(
        error.contains("stale") || error.contains("binding"),
        "{error}"
    );
    assert_eq!(f.inspect()["head_sha"], moved);

    // Changed task specification refuses, and a clean worktree is required.
    let task = f.runtime.get_task(&f.task).unwrap();
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some("A different scope".into()),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    refused_without_effects(&f, f.request("inspect"), "changed specification");
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some(task.description),
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    let retained = Path::new(
        original.pipeline["worktree"]["workspace_path"]
            .as_str()
            .unwrap(),
    )
    .join("retained.txt");
    std::fs::write(&retained, "unfinished repair\n").unwrap();
    refused_without_effects(&f, f.request("inspect"), "uncommitted candidate");
    std::fs::remove_file(retained).unwrap();
    f.inspect();
    // Nothing above reset the source's historical evidence.
    assert_eq!(
        f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap(),
        original
    );
    reload(&mut f);
    f.inspect();
}

crate::scenario_table!(
    resumed_in_place_scenarios,
    resumed_in_place_publication_gets_fresh_same_pr_repair,
    resumed_in_place_publication_refuses_unproven_shapes_without_effects
);
