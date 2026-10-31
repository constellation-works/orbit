//! An active execution claim fences ordinary task commits, while settled
//! phases keep the store's existing unscoped-write behavior.

use super::*;

fn append_unscoped_history(owner: &Coordinated, task_id: &str) -> Result<(), OrbitError> {
    owner
        .backends
        .commit_boundary
        .commit_task_transition(&orbit_store::contracts::TaskCoordinationCommitParams {
            task_id: task_id.into(),
            actor: "test".into(),
            append_history: vec![orbit_types::task::TaskHistoryEntry {
                at: Utc::now(),
                by: "test".into(),
                event: "unscoped_claim_guard_probe".into(),
                note: Some("verify ordinary task commit boundary".into()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        })
        .map(|_| ())
}

fn assert_unscoped_history_refused(owner: &Coordinated, task_id: &str, phase: ExecutionClaimPhase) {
    let error = append_unscoped_history(owner, task_id)
        .expect_err("protecting claims reject ordinary task history commits");
    assert!(
        error
            .to_string()
            .contains("active execution claim requires a claim-scoped mutation"),
        "{phase:?} keeps the claim-scoped mutation fence: {error}"
    );
}

fn assert_unscoped_history_allowed(owner: &Coordinated, task_id: &str, phase: ExecutionClaimPhase) {
    append_unscoped_history(owner, task_id)
        .unwrap_or_else(|error| panic!("{phase:?} keeps unscoped commit behavior: {error}"));
}

#[test]
fn unscoped_task_commits_follow_every_protecting_claim_phase() {
    if !allocation_admission_isolated(
        "unscoped_claim_writes::unscoped_task_commits_follow_every_protecting_claim_phase",
    ) {
        return;
    }

    let claimed_owner = Coordinated::fixture();
    let claimed_task = claimed_owner.create_task("claimed phase");
    let claimed = claimed_owner
        .pull(&owner_request("claimed-phase"))
        .claim
        .expect("claim admitted");
    assert_eq!(claimed.phase, ExecutionClaimPhase::Claimed);
    assert_unscoped_history_refused(
        &claimed_owner,
        &claimed_task.id,
        ExecutionClaimPhase::Claimed,
    );

    let running = Delivery::admit(done_ship());
    assert_eq!(
        running.owner.claims()[0].phase,
        ExecutionClaimPhase::Running
    );
    assert_unscoped_history_refused(
        &running.owner,
        &running.claim.task_id,
        ExecutionClaimPhase::Running,
    );

    let handed_off = Delivery::admit(done_ship());
    handed_off
        .accept(Some(OWNER_POLICY))
        .expect("handoff accepted");
    assert_eq!(
        handed_off.owner.claims()[0].phase,
        ExecutionClaimPhase::HandedOff
    );
    assert_unscoped_history_refused(
        &handed_off.owner,
        &handed_off.claim.task_id,
        ExecutionClaimPhase::HandedOff,
    );

    let repair = Delivery::admit(done_ship());
    repair.accept(Some(OWNER_POLICY)).expect("handoff accepted");
    let accepted = repair
        .owner
        .backends
        .commit_boundary
        .accepted_handoff(&repair.claim.claim_id)
        .expect("accepted handoff");
    let operator = repair.operator();
    repair
        .owner
        .mutate(
            &operator,
            "dispatch-guard-probe",
            &ClaimMutation::DispatchLanding {
                handoff_id: accepted.handoff_id.clone(),
                job_run_id: None,
            },
        )
        .expect("landing attempt dispatched");
    repair
        .owner
        .mutate(
            &operator,
            "stop-guard-probe",
            &ClaimMutation::StopLanding {
                handoff_id: accepted.handoff_id,
                reason: "candidate needs a repair".into(),
                repairable: true,
                requeue: false,
            },
        )
        .expect("repairable landing stop");
    assert_eq!(
        repair.owner.claims()[0].phase,
        ExecutionClaimPhase::RepairPending
    );
    assert_unscoped_history_refused(
        &repair.owner,
        &repair.claim.task_id,
        ExecutionClaimPhase::RepairPending,
    );
}

#[test]
fn settled_claim_phases_keep_unscoped_task_commits() {
    if !allocation_admission_isolated(
        "unscoped_claim_writes::settled_claim_phases_keep_unscoped_task_commits",
    ) {
        return;
    }

    let failed = Delivery::admit(done_ship());
    failed
        .owner
        .mutate(
            &failed.worker(),
            "fail-guard-probe",
            &ClaimMutation::Fail(ClaimEvidence {
                summary: Some("the candidate failed".into()),
                ..Default::default()
            }),
        )
        .expect("claim failed");
    assert_eq!(failed.owner.claims()[0].phase, ExecutionClaimPhase::Failed);
    assert_unscoped_history_allowed(
        &failed.owner,
        &failed.claim.task_id,
        ExecutionClaimPhase::Failed,
    );

    let revoked = Delivery::admit(done_ship());
    revoked
        .owner
        .mutate(
            &revoked.worker(),
            "release-guard-probe",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("the executor returned the claim".into()),
                ..Default::default()
            }),
        )
        .expect("claim released");
    assert_eq!(
        revoked.owner.claims()[0].phase,
        ExecutionClaimPhase::Revoked
    );
    assert_unscoped_history_allowed(
        &revoked.owner,
        &revoked.claim.task_id,
        ExecutionClaimPhase::Revoked,
    );

    let landed = Delivery::admit(done_ship());
    landed.accept(Some(OWNER_POLICY)).expect("handoff accepted");
    let accepted = landed
        .owner
        .backends
        .commit_boundary
        .accepted_handoff(&landed.claim.claim_id)
        .expect("accepted handoff");
    let operator = landed
        .operator()
        .with_handoff_observation(landed.observation(Some(OWNER_POLICY)));
    landed
        .owner
        .mutate(
            &operator,
            "dispatch-landed-guard-probe",
            &ClaimMutation::DispatchLanding {
                handoff_id: accepted.handoff_id.clone(),
                job_run_id: None,
            },
        )
        .expect("landing attempt dispatched");
    landed
        .owner
        .mutate(
            &operator,
            "complete-landed-guard-probe",
            &ClaimMutation::CompleteLanding {
                handoff_id: accepted.handoff_id,
                evidence: "verified merge".into(),
            },
        )
        .expect("handoff landed");
    assert_eq!(landed.owner.claims()[0].phase, ExecutionClaimPhase::Landed);
    assert_unscoped_history_allowed(
        &landed.owner,
        &landed.claim.task_id,
        ExecutionClaimPhase::Landed,
    );
}
