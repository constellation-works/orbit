//! A recovery host carries evidence to its claim owner without task-write tools.

use super::claimed_review::ToOwner;
use super::*;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{FinalRecoveryCheckpoint, FinalRecoveryKey};

#[test]
fn final_recovery_handoff_persists_only_its_claimed_tasks_evidence() {
    if !isolated(
        module_path!(),
        "final_recovery_handoff_persists_only_its_claimed_tasks_evidence",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let other = pair.tasks.iter().find(|id| **id != task).unwrap();
    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: task.clone(),
                claim_id: claim.claim_id,
                execution: claim.executed_on,
                bound_run_id: leaf.clone(),
            },
            Arc::new(ToOwner(pair.wire.owner.clone())),
        )
        .unwrap();
    let mut state = PipelineState::new(leaf.clone(), "claimed_task_pipeline".into(), json!({}));
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        fault: Default::default(),
        key: FinalRecoveryKey {
            run_id: leaf.clone(),
            attempt: 1,
        },
        failed_step_id: "implement".into(),
        task_id: task.clone(),
        observed: None,
        repair_commit: None,
        resume_context: None,
        base_ref: Some("main".into()),
        admitted_at: Utc::now(),
        decision: None,
        outcome: None,
    });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    let scratch = pair.follower_repo.join(".orbit/tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("repair.log"), b"verified claimed repair\n").unwrap();
    let evidence = json!({"execution_summary": "Outcome: success\nChanges: repaired claimed candidate.",
        "task_artifacts": [{"path": "recovery/repair.log", "source_path": ".orbit/tmp/repair.log"}]});
    let persist = |id: &str| {
        RuntimeHost::persist_final_recovery_evidence(
            &bound,
            &leaf,
            id,
            &pair.follower_repo,
            &evidence,
        )
    };
    assert!(
        persist(other).is_err(),
        "the recovery cannot write another owner's task"
    );
    persist(&task).expect("host sends bounded evidence through the authenticated claim route");
    let owner = &pair.wire.owner;
    assert_eq!(
        owner.get_task(&task).unwrap().execution_summary,
        "Outcome: success\nChanges: repaired claimed candidate."
    );
    assert_eq!(
        owner
            .get_task_artifact(&task, "recovery/repair.log")
            .unwrap()
            .unwrap()
            .content,
        b"verified claimed repair\n"
    );
    assert!(
        owner
            .get_task_artifact(other, "recovery/repair.log")
            .unwrap()
            .is_none()
    );
    assert!(owner.get_task(other).unwrap().execution_summary.is_empty());
    // A replay after recovery decides must not reopen its evidence authority.
    state.final_recovery.as_mut().unwrap().decision =
        Some(orbit_types::workflow::FinalRecoveryDecision::Resume {
            step_id: "implement".into(),
            rationale: "repair complete".into(),
        });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    assert!(
        persist(&task).is_err(),
        "decided recovery has no evidence-write authority"
    );
}
