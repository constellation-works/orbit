//! Reads and retention of the follower's admission table.

use rusqlite::params;

use super::super::pull::TERMINAL_ROWS_RETAINED;
use super::backend::{isolated_pull_test, pull_fixture, pull_receipt};
use crate::contracts::{
    AdmissionReceipt, AdmissionRequest, ClaimEvidence, ClaimMutation, JobRunStoreBackend,
    LocalPullAdmission, LocalPullMutation as M, LocalPullPhase as P, PullDestination,
};

fn request_named(request: &AdmissionRequest, id: &str) -> AdmissionRequest {
    let mut named = request.clone();
    named.request_id = id.into();
    named
}

/// A claim receipt for `request`, with a claim ID of its own: claim IDs are
/// unique per workspace.
fn claim_receipt(request: &AdmissionRequest) -> AdmissionReceipt {
    let mut receipt = pull_receipt(request);
    if let Some(claim) = receipt.claim.as_mut() {
        claim.claim_id = format!("claim-{}", request.request_id);
    }
    receipt
}

/// A receipt for an owner with nothing ready.
fn idle_receipt(request: &AdmissionRequest) -> AdmissionReceipt {
    let mut receipt = pull_receipt(request);
    receipt.claim = None;
    receipt.task = None;
    receipt
}

fn advance(
    store: &dyn JobRunStoreBackend,
    destination: &PullDestination,
    id: &str,
    mutations: impl IntoIterator<Item = M>,
) {
    for mutation in mutations {
        store
            .mutate_local_pull(destination, id, &mutation)
            .expect("transition");
    }
}

fn fail() -> M {
    M::Settle(Box::new(ClaimMutation::Fail(ClaimEvidence {
        summary: Some("failed".into()),
        ..Default::default()
    })))
}

fn ids(records: &[LocalPullAdmission]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.request.request_id.clone())
        .collect()
}

/// Only admissions that still hold a slot are read for settlement and for the
/// capacity check, whatever history sits beside them.
#[test]
fn unsettled_and_capacity_read_only_the_admissions_holding_a_slot() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::pull::unsettled_and_capacity_read_only_the_admissions_holding_a_slot",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    let allocate = |id: &str| {
        store
            .allocate_pull_request(&destination, &request_named(&request, id), 10)
            .expect("allocate")
            .expect("slot")
    };
    for id in ["idle", "refused", "settled", "claimed", "requested"] {
        allocate(id);
    }
    let named = |id: &str| request_named(&request, id);
    advance(
        &store,
        &destination,
        "idle",
        [M::Receive(Box::new(idle_receipt(&named("idle"))))],
    );
    advance(&store, &destination, "refused", [M::Refuse("no".into())]);
    advance(
        &store,
        &destination,
        "settled",
        [
            M::Receive(Box::new(claim_receipt(&named("settled")))),
            M::CreateLeaf,
            M::Bound,
            fail(),
            M::Settled,
        ],
    );
    advance(
        &store,
        &destination,
        "claimed",
        [M::Receive(Box::new(claim_receipt(&named("claimed"))))],
    );

    let all = store.local_pull_admissions().expect("all");
    assert_eq!(
        all.iter().map(|record| record.phase).collect::<Vec<_>>(),
        [P::Idle, P::Refused, P::Settled, P::Claimed, P::Requested]
    );
    let unsettled = store.unsettled_local_pull_admissions().expect("unsettled");
    assert_eq!(ids(&unsettled), ["claimed", "requested"]);
    assert_eq!(
        ids(&unsettled),
        ids(&all
            .into_iter()
            .filter(LocalPullAdmission::holds_capacity)
            .collect::<Vec<_>>()),
        "the SQL narrowing agrees with the slot-holding phases"
    );
    assert_eq!(store.drain_leaf_occupancy().expect("occupancy").occupied, 2);
}

/// A drain that polls an idle owner leaves a row per poll. Those, and refused
/// requests, are pruned to a bounded tail when the next request is allocated;
/// settled claims and anything still holding a slot are never touched.
#[test]
fn allocation_prunes_old_idle_and_refused_rows_but_keeps_settled_history() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::pull::allocation_prunes_old_idle_and_refused_rows_but_keeps_settled_history",
    ) {
        return;
    }
    let (temp, store, destination, request) = pull_fixture();
    let named = |id: &str| request_named(&request, id);
    for id in ["settled", "live"] {
        store
            .allocate_pull_request(&destination, &named(id), 10)
            .expect("allocate")
            .expect("slot");
    }
    advance(
        &store,
        &destination,
        "settled",
        [
            M::Receive(Box::new(claim_receipt(&named("settled")))),
            M::CreateLeaf,
            M::Bound,
            fail(),
            M::Settled,
        ],
    );

    // Bulk history, oldest first, written the way earlier passes left it.
    let excess = 50;
    let history = TERMINAL_ROWS_RETAINED + excess;
    let mut conn = rusqlite::Connection::open(temp.path().join("pull.db")).expect("open");
    let tx = conn.transaction().expect("tx");
    for index in 0..history {
        let id = format!("poll-{index}");
        let refused = index % 2 == 1;
        let record = LocalPullAdmission {
            destination: destination.clone(),
            request: named(&id),
            receipt: (!refused).then(|| idle_receipt(&named(&id))),
            leaf_run_id: None,
            phase: if refused { P::Refused } else { P::Idle },
            settlement: None,
            refusal: refused.then(|| "version_mismatch".into()),
        };
        tx.execute(
            "INSERT INTO local_pull_admissions VALUES (?1,?2,?3,?4,?5,NULL,NULL,?6)",
            params![
                "ws",
                destination.owner_machine_id,
                destination.owner_workspace_id,
                destination.execution_machine_id,
                id,
                serde_json::to_string(&record).expect("record json"),
            ],
        )
        .expect("history row");
    }
    tx.commit().expect("commit history");
    drop(conn);
    let total_before = store.local_pull_admissions().expect("all").len();
    assert_eq!(total_before, history + 2);

    store
        .allocate_pull_request(&destination, &named("fresh"), 10)
        .expect("allocate")
        .expect("slot");

    let all = store.local_pull_admissions().expect("all");
    let terminal: Vec<_> = all
        .iter()
        .filter(|record| matches!(record.phase, P::Idle | P::Refused))
        .collect();
    assert_eq!(terminal.len(), TERMINAL_ROWS_RETAINED);
    assert_eq!(
        terminal[0].request.request_id,
        format!("poll-{excess}"),
        "the oldest rows went first"
    );
    assert_eq!(
        terminal[terminal.len() - 1].request.request_id,
        format!("poll-{}", history - 1)
    );
    assert!(
        terminal.iter().any(|record| record.phase == P::Refused)
            && terminal.iter().any(|record| record.phase == P::Idle),
        "both terminal kinds are pruned by age together"
    );
    let kept: Vec<_> = all
        .iter()
        .filter(|record| !matches!(record.phase, P::Idle | P::Refused))
        .map(|record| (record.request.request_id.as_str(), record.phase))
        .collect();
    assert_eq!(
        kept,
        [
            ("settled", P::Settled),
            ("live", P::Requested),
            ("fresh", P::Requested)
        ]
    );
}
