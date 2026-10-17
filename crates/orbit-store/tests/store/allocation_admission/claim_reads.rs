//! Targeted claim reads and batched lifecycle inspection through composition.

use orbit_common::ClaimRefusalKind;
use orbit_store::contracts::{BranchObservationOutcome, DeferredBranchObservation};

use super::*;

const CLAIM: &str = "distributed-execution-claim-v1";
const STATE: &str = "distributed-claim-lifecycle-v1";

#[test]
fn targeted_claim_reads_ignore_unrelated_payloads_and_refuse_missing_claims() {
    if !allocation_admission_isolated(
        "claim_reads::targeted_claim_reads_ignore_unrelated_payloads_and_refuse_missing_claims",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task_in("first", &["src/a.rs"]);
    owner.create_task_in("second", &["src/b.rs"]);
    let request = owner_request("target");
    let target = owner.pull(&request).claim.unwrap();
    let unrelated = owner.pull(&owner_request("unrelated")).claim.unwrap();
    let conn = rusqlite::Connection::open(root.path().join("state.sqlite")).unwrap();
    conn.execute(
        "UPDATE task_coordination_rows SET payload_json = '{}' WHERE kind = ?1 AND row_id = ?2",
        rusqlite::params![CLAIM, unrelated.claim_id],
    )
    .unwrap();
    let identity = AdmissionIdentity::trusted_local(target.executed_on.clone());
    let lookup = owner
        .backends
        .commit_boundary
        .lookup_admission(&identity, &request.request_id)
        .unwrap();
    let AdmissionLookup::Found { current_claim, .. } = lookup else {
        panic!("expected the target receipt");
    };
    assert_eq!(current_claim.as_deref(), Some(&target));

    let outcome = owner
        .backends
        .task
        .task
        .record_deferred_branch_observation(&DeferredBranchObservation {
            schema_version: 1,
            task_id: target.task_id.clone(),
            run_id: serde_json::json!(1),
            job_id: serde_json::json!(2),
            artifact_path: "ci/observation.json".into(),
            content: "failure evidence".into(),
            claiming_run_id: None,
            applied: false,
        })
        .unwrap();
    assert_eq!(
        outcome,
        BranchObservationOutcome::Deferred {
            claiming_run_id: target.run_context.run_id.clone(),
        }
    );

    let auth = ClaimInvocation::trusted_worker(
        target.task_id.clone(),
        target.claim_id.clone(),
        target.executed_on.machine_id.clone(),
        None,
    );
    let bind = ClaimMutation::Bind {
        run: leaf_run(&target),
        ship: request.ship,
    };
    owner
        .backends
        .commit_boundary
        .mutate_execution_claim(Some(&auth), "bind", &bind)
        .unwrap();
    let AdmissionLookup::Found { current_claim, .. } = owner
        .backends
        .commit_boundary
        .lookup_admission(&identity, &request.request_id)
        .unwrap()
    else {
        panic!("expected the target receipt after binding");
    };
    assert_eq!(current_claim.unwrap().phase, ExecutionClaimPhase::Running);

    conn.execute(
        "DELETE FROM task_coordination_rows WHERE kind = ?1 AND row_id = ?2",
        rusqlite::params![CLAIM, target.claim_id],
    )
    .unwrap();
    let AdmissionLookup::Found { current_claim, .. } = owner
        .backends
        .commit_boundary
        .lookup_admission(&identity, &request.request_id)
        .unwrap()
    else {
        panic!("the original receipt survives a missing claim");
    };
    assert!(current_claim.is_none());
    let refused = owner
        .backends
        .commit_boundary
        .mutate_execution_claim(Some(&auth), "bind", &bind)
        .expect_err("missing claim must not replay launch authority");
    assert_eq!(refused.claim_refusal(), Some(ClaimRefusalKind::StaleClaim));
}

#[test]
fn inspection_joins_only_matching_states_and_preserves_legacy_defaults() {
    if !allocation_admission_isolated("claim_reads::inspection_joins_only_matching_states_and_preserves_legacy_defaults")
    {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task_in("bound", &["src/a.rs"]);
    owner.create_task_in("legacy", &["src/b.rs"]);
    let request = owner_request("bound");
    let bound = owner.pull(&request).claim.unwrap();
    let auth = ClaimInvocation::trusted_worker(
        bound.task_id.clone(),
        bound.claim_id.clone(),
        bound.executed_on.machine_id.clone(),
        None,
    );
    let run = leaf_run(&bound);
    owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&auth),
            "bind",
            &ClaimMutation::Bind {
                run: run.clone(),
                ship: request.ship,
            },
        )
        .unwrap();
    let legacy = owner.pull(&owner_request("legacy")).claim.unwrap();
    let conn = rusqlite::Connection::open(root.path().join("state.sqlite")).unwrap();
    let created = (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
    conn.execute(
        "UPDATE task_coordination_rows
         SET payload_json = json_set(payload_json, '$.claim.phase', 'claimed', '$.created_at', ?1)
         WHERE kind = ?2 AND row_id = ?3",
        rusqlite::params![created, STATE, bound.claim_id],
    )
    .unwrap();
    // A same-id state in another workspace must not join a legacy claim.
    conn.execute(
        "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at)
         SELECT 'other-workspace', ?1, row_id, '{}', journal_id, created_at
         FROM task_coordination_rows WHERE kind = ?2 AND row_id = ?3",
        rusqlite::params![STATE, CLAIM, legacy.claim_id],
    ).unwrap();
    // Nor should inspection decode an orphan lifecycle row in its workspace.
    conn.execute(
        "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at)
         SELECT workspace_id, ?1, row_id || ':orphan', '{}', journal_id, created_at
         FROM task_coordination_rows WHERE kind = ?2 AND row_id = ?3",
        rusqlite::params![STATE, CLAIM, bound.claim_id],
    ).unwrap();

    for states in [
        owner
            .backends
            .commit_boundary
            .inspect_execution_claims()
            .unwrap(),
        owner
            .backends
            .commit_boundary
            .resolve_execution_claims()
            .unwrap(),
    ] {
        assert_eq!(states.len(), 2);
        assert!(
            states
                .windows(2)
                .all(|pair| pair[0].claim.claim_id < pair[1].claim.claim_id)
        );
        let persisted = states
            .iter()
            .find(|state| state.claim.claim_id == bound.claim_id)
            .unwrap();
        assert_eq!(persisted.bound_run.as_ref(), Some(&run));
        assert_eq!(
            persisted.claim.phase,
            ExecutionClaimPhase::Running,
            "the claim row overrides a stale claim embedded in lifecycle state"
        );
        assert_eq!(persisted.created_at, created);
        assert_eq!(persisted.last_event, "claim_bound");
        assert!(persisted.age_seconds.unwrap() >= 60);
        let missing = states
            .iter()
            .find(|state| state.claim.claim_id == legacy.claim_id)
            .unwrap();
        let expected_created = chrono::DateTime::parse_from_rfc3339(&legacy.reservation_expires_at)
            .unwrap()
            - chrono::Duration::seconds(14_400);
        assert_eq!(missing.claim, legacy);
        assert_eq!(missing.created_at, expected_created.to_rfc3339());
        assert_eq!(missing.updated_at, missing.created_at);
        assert_eq!(missing.last_event, "claimed");
        assert!(
            missing.bound_run.is_none()
                && missing.release.is_none()
                && missing.settlement.is_none()
        );
        assert!(missing.age_seconds.is_some());
    }
}
