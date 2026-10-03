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

/// A second drain's parent run, so its admissions can sit in the same table.
fn other_drain_request(
    store: &dyn JobRunStoreBackend,
    template: &AdmissionRequest,
) -> AdmissionRequest {
    let parent = store
        .insert_job_run("workspace_auto_pipeline", 1, chrono::Utc::now(), None, None)
        .expect("other parent");
    store
        .write_run_state(
            &parent.run_id,
            &orbit_types::workflow::PipelineState::new(
                parent.run_id.clone(),
                parent.job_id,
                serde_json::json!({}),
            ),
        )
        .expect("other parent state");
    let mut request = template.clone();
    request.run_context.run_id = parent.run_id;
    request
}

/// A handoff the owner would accept for `record`'s claim.
fn accepted_handoff(record: &LocalPullAdmission) -> ClaimMutation {
    use orbit_types::workflow::ReviewTiming;
    use orbit_types::workflow::automation::SourceRevision;
    use orbit_types::workflow::handoff::{
        HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition, TaskHandoff,
    };
    let claim = record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.as_ref())
        .expect("claim");
    ClaimMutation::AcceptHandoff(TaskHandoff {
        schema_version: 1,
        workspace_id: record.destination.owner_workspace_id.clone(),
        task_id: claim.task_id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: record.leaf_run_id.clone().expect("leaf"),
        candidate: HandoffCandidate {
            repository: "owner/repository".into(),
            source_branch: "orbit/task".into(),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::PullRequest { number: 1 },
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success".into(),
        validation: vec![],
    })
}

/// How one settled claim ended.
enum Ending {
    Failed,
    Handoff,
    /// Failed, then closed because the owner had already ended the claim.
    Obsolete,
}

/// Admit `id` under `request`'s drain, run it to `Settled` the way the drain
/// does, and leave it as the newest row.
fn settle_claim(
    store: &dyn JobRunStoreBackend,
    destination: &PullDestination,
    request: &AdmissionRequest,
    id: &str,
    ending: Ending,
) {
    let named = request_named(request, id);
    store
        .allocate_pull_request(destination, &named, 10)
        .expect("allocate")
        .expect("slot");
    advance(
        store,
        destination,
        id,
        [M::Receive(Box::new(claim_receipt(&named))), M::CreateLeaf],
    );
    let leaf = store
        .mutate_local_pull(destination, id, &M::Bound)
        .expect("record");
    let settlement = match ending {
        Ending::Handoff => M::Settle(Box::new(accepted_handoff(&leaf))),
        Ending::Failed | Ending::Obsolete => fail(),
    };
    advance(store, destination, id, [settlement]);
    let close = match ending {
        Ending::Obsolete => M::SettleObsolete("owner already ended the claim".into()),
        Ending::Failed | Ending::Handoff => M::Settled,
    };
    advance(store, destination, id, [close]);
}

/// The reading the breaker replaced: every admission decoded, then filtered.
fn streak_from_full_history(
    store: &dyn JobRunStoreBackend,
    destination: &PullDestination,
    run_id: &str,
) -> usize {
    store
        .local_pull_admissions()
        .expect("history")
        .iter()
        .filter(|record| {
            record.destination == *destination
                && record.request.run_context.run_id == run_id
                && record.phase == P::Settled
                && record.refusal.is_none()
        })
        .rev()
        .take_while(|record| matches!(record.settlement, Some(ClaimMutation::Fail(_))))
        .count()
}

/// The breaker's streak is this drain's trailing failed settlements. Another
/// drain's history, idle polls, refused requests and unsettled claims are
/// beside it, not in it, and an obsolete claim neither extends nor resets it.
#[test]
fn failure_streak_counts_this_drains_trailing_failed_settlements() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::pull::failure_streak_counts_this_drains_trailing_failed_settlements",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    let drain = request.run_context.run_id.clone();
    let other = other_drain_request(&store, &request);
    let streak = |run_id: &str| {
        let narrow = store
            .consecutive_failed_local_pull_settlements(&destination, run_id)
            .expect("streak");
        assert_eq!(
            narrow,
            streak_from_full_history(&store, &destination, run_id),
            "the narrow read agrees with filtering the whole history"
        );
        narrow
    };

    assert_eq!(streak(&drain), 0, "a drain with no history has no streak");
    settle_claim(&store, &destination, &request, "a1", Ending::Failed);
    settle_claim(&store, &destination, &other, "b1", Ending::Handoff);
    settle_claim(&store, &destination, &request, "a2", Ending::Failed);
    assert_eq!(
        streak(&drain),
        2,
        "another drain's success does not reset it"
    );
    assert_eq!(streak(&other.run_context.run_id), 0);

    // Rows that are not settled claims.
    let idle = request_named(&request, "idle");
    store
        .allocate_pull_request(&destination, &idle, 10)
        .expect("allocate")
        .expect("slot");
    advance(
        &store,
        &destination,
        "idle",
        [M::Receive(Box::new(idle_receipt(&idle)))],
    );
    store
        .allocate_pull_request(&destination, &request_named(&request, "refused"), 10)
        .expect("allocate")
        .expect("slot");
    advance(&store, &destination, "refused", [M::Refuse("no".into())]);
    let live = request_named(&request, "live");
    store
        .allocate_pull_request(&destination, &live, 10)
        .expect("allocate")
        .expect("slot");
    advance(
        &store,
        &destination,
        "live",
        [M::Receive(Box::new(claim_receipt(&live)))],
    );
    assert_eq!(streak(&drain), 2);

    settle_claim(&store, &destination, &request, "a3", Ending::Obsolete);
    assert_eq!(streak(&drain), 2, "an obsolete claim does not extend it");
    settle_claim(&store, &destination, &request, "a4", Ending::Failed);
    assert_eq!(streak(&drain), 3);

    settle_claim(&store, &destination, &request, "a5", Ending::Handoff);
    assert_eq!(streak(&drain), 0, "a handed-off claim ends the streak");
    settle_claim(&store, &destination, &request, "a6", Ending::Failed);
    settle_claim(&store, &destination, &request, "a7", Ending::Obsolete);
    assert_eq!(streak(&drain), 1, "an obsolete claim does not reset it");
    assert_eq!(streak("another-drain"), 0);

    let mut elsewhere = destination.clone();
    elsewhere.selector = "owner/elsewhere".into();
    assert_eq!(
        store
            .consecutive_failed_local_pull_settlements(&elsewhere, &drain)
            .expect("streak"),
        0,
        "a claim against another selector is not this destination's"
    );
}

/// The read is schema-neutral: asking a workspace that never pulled for its
/// streak reports none and does not create the pull tables.
#[test]
fn failure_streak_read_does_not_create_the_pull_schema() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::pull::failure_streak_read_does_not_create_the_pull_schema",
    ) {
        return;
    }
    let (temp, store, destination, request) = pull_fixture();
    assert_eq!(
        store
            .consecutive_failed_local_pull_settlements(&destination, &request.run_context.run_id)
            .expect("streak"),
        0
    );
    let conn = rusqlite::Connection::open(temp.path().join("pull.db")).expect("open");
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='local_pull_admissions'",
            [],
            |row| row.get(0),
        )
        .expect("count tables");
    assert_eq!(tables, 0);
}

/// The streak read hands back only this drain's settled claims, newest first,
/// and stops at the first one that did not fail. Rows outside that, and older
/// than that, are never decoded: a history row this binary could not decode
/// must not break, or slow, a pass that has no use for it.
#[test]
fn failure_streak_decodes_only_the_settled_claims_it_needs() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::pull::failure_streak_decodes_only_the_settled_claims_it_needs",
    ) {
        return;
    }
    let (temp, store, destination, request) = pull_fixture();
    let drain = request.run_context.run_id.clone();
    store.local_pull_admissions().expect("initialize");
    let poison = |id: &str, claim: Option<&str>, json: String| {
        let conn = rusqlite::Connection::open(temp.path().join("pull.db")).expect("open");
        conn.execute(
            "INSERT INTO local_pull_admissions VALUES (?1,?2,?3,?4,?5,?6,NULL,?7)",
            params![
                "ws",
                destination.owner_machine_id,
                destination.owner_workspace_id,
                destination.execution_machine_id,
                id,
                claim,
                json,
            ],
        )
        .expect("poison row");
    };
    // A settled claim of this drain that only decodes if something reads it,
    // older than the handoff below.
    poison(
        "ancient",
        Some("claim-ancient"),
        serde_json::json!({
            "phase": "settled",
            "request": {"run_context": {"run_id": drain}},
            "destination": {"selector": destination.selector},
        })
        .to_string(),
    );
    settle_claim(&store, &destination, &request, "handoff", Ending::Handoff);
    // Idle and refused rows, and a settled claim of another drain, none of
    // which decode as an admission.
    poison("idle", None, r#"{"phase":"idle"}"#.into());
    poison("refused", None, r#"{"phase":"refused"}"#.into());
    poison(
        "stranger",
        Some("claim-stranger"),
        serde_json::json!({
            "phase": "settled",
            "request": {"run_context": {"run_id": "another-drain"}},
            "destination": {"selector": destination.selector},
        })
        .to_string(),
    );
    settle_claim(&store, &destination, &request, "f1", Ending::Failed);
    settle_claim(&store, &destination, &request, "f2", Ending::Failed);

    assert!(
        store.local_pull_admissions().is_err(),
        "the fixture holds rows a full read cannot decode"
    );
    assert_eq!(
        store
            .consecutive_failed_local_pull_settlements(&destination, &drain)
            .expect("streak"),
        2
    );
}

/// A pulled task lives in the owner's store, so the leaf carries the owner's
/// snapshot of it: crew resolution on the follower reads the task's crew from
/// here instead of falling through to its own `default_crew`.
#[test]
fn created_leaf_carries_the_owner_task_snapshot_crew() {
    if isolated_pull_test(
        "driver::sqlite::job_run_store::tests::pull::created_leaf_carries_the_owner_task_snapshot_crew",
    ) {
        return;
    }
    let (_temp, store, destination, request) = pull_fixture();
    for (id, crew) in [("crewed", Some("sol")), ("crewless", None)] {
        let named = request_named(&request, id);
        store
            .allocate_pull_request(&destination, &named, 10)
            .expect("allocate")
            .expect("slot");
        let mut receipt = claim_receipt(&named);
        receipt.task.as_mut().expect("task snapshot").crew = crew.map(ToOwned::to_owned);
        advance(
            &store,
            &destination,
            id,
            [M::Receive(Box::new(receipt)), M::CreateLeaf],
        );
        let leaf = store
            .local_pull_admissions()
            .expect("records")
            .into_iter()
            .find(|record| record.request.request_id == id)
            .and_then(|record| record.leaf_run_id)
            .expect("leaf");
        let input = store
            .get_job_run(&leaf)
            .expect("read")
            .expect("leaf run")
            .input
            .expect("leaf input");
        assert_eq!(
            input["claimed_task"],
            serde_json::json!({ "id": "task", "crew": crew }),
            "{id}"
        );
    }
}
