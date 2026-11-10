//! Handoff attribution must not introduce an enum value into older clients' lifecycle rows.

use super::*;
use orbit_store::contracts::{ClaimAttribution, ClaimInspection, ClaimSettlementKind};

// The closed settlement vocabulary shipped in v0.30.0 (34b2240d16c3a8eb5c1a00eeb0a7211b6702027d).
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReleasedSettlementKind {
    Fail,
    Release,
}

// The settlement record as owners before the reported effort read it: no
// `reasoning_effort` field, and no refusal of unknown ones.
#[derive(serde::Deserialize)]
struct ReleasedSettlement {
    kind: ReleasedSettlementKind,
    crew_model: Option<String>,
}

#[derive(serde::Deserialize)]
struct ReleasedLifecycle {
    settlement: Option<ReleasedSettlement>,
}

#[test]
fn handoff_attribution_keeps_older_lifecycle_readers_and_replays_working() {
    if !allocation_admission_isolated(
        "handoff_compatibility::handoff_attribution_keeps_older_lifecycle_readers_and_replays_working",
    ) {
        return;
    }
    let delivery = Delivery::admit(done_ship());
    // Keep the same client open across the new writer's settlement.
    let client = Store::open(&delivery.owner.root().join("state.sqlite")).unwrap();
    let auth = delivery
        .worker()
        .with_handoff_observation(delivery.observation(Some(OWNER_POLICY)))
        .with_attribution(ClaimAttribution {
            crew: Some("reported-crew".into()),
            crew_model: Some("reported-model".into()),
            ..ClaimAttribution::default()
        });
    let mutation = ClaimMutation::AcceptHandoff(delivery.handoff.clone());
    let boundary = &delivery.owner.backends.commit_boundary;
    let first = boundary
        .mutate_execution_claim(Some(&auth), "handoff", &mutation)
        .unwrap();
    let payload: String = client
        .with_read_connection(|conn| {
            conn.query_row(
                "SELECT payload_json FROM task_coordination_rows WHERE workspace_id=?1 AND kind='distributed-claim-lifecycle-v1' AND row_id=?2",
                rusqlite::params![PARTITION_ID, delivery.claim.claim_id],
                |row| row.get(0),
            ).map_err(|error| OrbitError::Store(error.to_string()))
        })
        .unwrap();
    let old: ReleasedLifecycle = serde_json::from_str(&payload)
        .expect("released clients must still decode lifecycle state after a handoff");
    assert!(
        old.settlement.is_none(),
        "a handoff must not impersonate a failure or release"
    );
    let state = boundary
        .execution_claim_state(&delivery.claim.claim_id)
        .unwrap()
        .unwrap();
    let settlement = state.settlement.as_ref().unwrap();
    assert_eq!(settlement.kind, ClaimSettlementKind::Handoff);
    assert_eq!(settlement.crew.as_deref(), Some("reported-crew"));
    assert_eq!(settlement.crew_model.as_deref(), Some("reported-model"));
    assert_eq!(
        boundary
            .mutate_execution_claim(Some(&auth), "handoff", &mutation)
            .unwrap(),
        first
    );
    // Attribution is deliberately outside receipt identity: changed labels cannot rewrite it.
    let retry = auth.with_attribution(ClaimAttribution::default());
    assert_eq!(
        boundary
            .mutate_execution_claim(Some(&retry), "handoff", &mutation)
            .unwrap(),
        first
    );
    delivery.merge_intent(Some(OWNER_POLICY)).unwrap();
    let reopened = Coordinated::open(delivery.owner.root());
    let states = reopened
        .backends
        .commit_boundary
        .inspect_execution_claims()
        .unwrap();
    assert_eq!(states[0].settlement, state.settlement);
    let projected: ClaimInspection =
        serde_json::from_value(serde_json::to_value(&states[0]).unwrap()).unwrap();
    assert_eq!(projected.settlement, state.settlement);
    // Keep the released vocabulary operational as well: both existing kinds
    // still settle through the public boundary and remain readable by old
    // clients, including with the follower's reported effort [ORB-23825]: a
    // wire name, the provider default (`null`), or none, as an older follower
    // reports.
    for (key, kind, effort) in [
        (
            "legacy-fail",
            ReleasedSettlementKind::Fail,
            Some(Some("high".to_string())),
        ),
        (
            "legacy-release",
            ReleasedSettlementKind::Release,
            Some(None),
        ),
        ("legacy-unreported", ReleasedSettlementKind::Fail, None),
    ] {
        delivery.owner.create_task_in(key, &[key]);
        let claim = delivery.owner.pull(&owner_request(key)).claim.unwrap();
        let auth = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id,
            None,
        )
        .with_attribution(ClaimAttribution {
            crew_model: Some("reported-model".into()),
            reasoning_effort: effort.clone(),
            ..ClaimAttribution::default()
        });
        let evidence = ClaimEvidence {
            summary: Some("existing settlement remains readable".into()),
            ..Default::default()
        };
        let mutation = match kind {
            ReleasedSettlementKind::Fail => ClaimMutation::Fail(evidence),
            ReleasedSettlementKind::Release => ClaimMutation::Release(evidence),
        };
        boundary
            .mutate_execution_claim(Some(&auth), key, &mutation)
            .unwrap();
        let record = boundary
            .execution_claim_state(&claim.claim_id)
            .unwrap()
            .unwrap();
        let settlement = record.settlement.as_ref().unwrap();
        assert_eq!(settlement.reasoning_effort, effort, "{key}");
        let encoded = serde_json::to_value(&record).unwrap();
        match &effort {
            None => assert!(
                encoded["settlement"].get("reasoning_effort").is_none(),
                "an unreported effort stays absent: {encoded}"
            ),
            Some(effort) => assert_eq!(
                encoded["settlement"]["reasoning_effort"],
                serde_json::json!(effort),
                "{encoded}"
            ),
        }
        let legacy: ReleasedLifecycle = serde_json::from_value(encoded).unwrap();
        let legacy = legacy.settlement.unwrap();
        assert_eq!(legacy.kind, kind);
        assert_eq!(legacy.crew_model.as_deref(), Some("reported-model"));
    }
}

#[test]
fn persisted_handoffs_upgrade_atomically_without_changing_receipts_or_authority() {
    if !allocation_admission_isolated(
        "handoff_compatibility::persisted_handoffs_upgrade_atomically_without_changing_receipts_or_authority",
    ) {
        return;
    }
    let delivery = Delivery::admit(done_ship());
    delivery.accept(Some(OWNER_POLICY)).unwrap();
    let boundary = &delivery.owner.backends.commit_boundary;
    let expected = boundary
        .execution_claim_state(&delivery.claim.claim_id)
        .unwrap()
        .unwrap();
    let database = delivery.owner.root().join("state.sqlite");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("DROP TRIGGER handoff_settlement_insert_guard; DROP TRIGGER handoff_settlement_update_guard;").unwrap();
    // Restore the precise pre-repair durable shape, including an unknown
    // extension so repair cannot silently discard future attribution fields.
    let mut settlement = serde_json::to_value(expected.settlement.as_ref().unwrap()).unwrap();
    settlement["future_attribution"] = serde_json::json!({"source": "retained"});
    conn.execute(
        "UPDATE task_coordination_rows SET payload_json=json_set(payload_json,'$.settlement',json(?1))
         WHERE kind='distributed-claim-lifecycle-v1' AND row_id=?2",
        rusqlite::params![settlement.to_string(), delivery.claim.claim_id],
    ).unwrap();
    conn.execute(
        "DELETE FROM task_coordination_rows WHERE kind='distributed-handoff-settlement-v1'",
        [],
    )
    .unwrap();
    crate::support::drop_replayed_migration_tables(&conn).unwrap();
    conn.execute_batch(
        "DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key >= 'migration.v0057';",
    )
    .unwrap();
    conn.execute(
        "UPDATE schema_meta SET value=json_set(value,'$.version',56) WHERE key='migration.compat'",
        [],
    )
    .unwrap();

    // A public reopen must roll back the sidecar INSERT as well as the
    // lifecycle rewrite and ledger stamp if interrupted after the insert.
    conn.execute_batch(
        "CREATE TRIGGER interrupt_handoff_upgrade BEFORE UPDATE ON task_coordination_rows
         WHEN OLD.kind='distributed-claim-lifecycle-v1'
         BEGIN SELECT RAISE(ABORT,'injected upgrade interruption'); END;",
    )
    .unwrap();
    let snapshot = || -> Vec<(String, String, String, String, String, String)> {
        let mut stmt = conn.prepare("SELECT workspace_id,kind,row_id,payload_json,journal_id,created_at FROM task_coordination_rows ORDER BY workspace_id,kind,row_id").unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    let before = snapshot();
    let journal_before: Vec<(String, String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT journal_id,state,intent_json FROM task_commit_journal ORDER BY journal_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(
        Store::open(&database).is_err(),
        "an interrupted upgrade must refuse the open"
    );
    assert_eq!(
        snapshot(),
        before,
        "interruption must roll back every attribution change"
    );
    assert_eq!(
        orbit_store::maintenance::migration::read_schema_ledger_status(&database)
            .unwrap()
            .current_version,
        56
    );
    conn.execute_batch("DROP TRIGGER interrupt_handoff_upgrade")
        .unwrap();
    // Conflicting evidence must fail closed too, without choosing a winner.
    conn.execute(
        "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at)
         SELECT workspace_id,'distributed-handoff-settlement-v1',row_id,'{}',journal_id,created_at
         FROM task_coordination_rows WHERE kind='distributed-claim-lifecycle-v1' AND row_id=?1",
        [&delivery.claim.claim_id],
    ).unwrap();
    let conflict = snapshot();
    assert!(Store::open(&database).is_err());
    assert_eq!(
        snapshot(),
        conflict,
        "conflicting attribution must remain recoverable"
    );
    conn.execute("DELETE FROM task_coordination_rows WHERE kind='distributed-handoff-settlement-v1' AND row_id=?1", [&delivery.claim.claim_id]).unwrap();
    let migrated = Store::open(&database).unwrap();
    assert!(!migrated.is_read_only());
    let after = snapshot();
    for row in &before {
        if row.1 != "distributed-claim-lifecycle-v1" {
            assert!(
                after.contains(row),
                "migration must preserve claim authority, handoffs and receipt bytes"
            );
        }
    }
    let sidecar = after
        .iter()
        .find(|r| r.1 == "distributed-handoff-settlement-v1")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sidecar.3).unwrap(),
        settlement
    );
    let lifecycle = after
        .iter()
        .find(|r| r.1 == "distributed-claim-lifecycle-v1")
        .unwrap();
    assert!(
        serde_json::from_str::<ReleasedLifecycle>(&lifecycle.3)
            .unwrap()
            .settlement
            .is_none()
    );
    let journal_after: Vec<(String, String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT journal_id,state,intent_json FROM task_commit_journal ORDER BY journal_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        journal_after, journal_before,
        "bundle recovery intents are not lifecycle copies"
    );
    let reopened = Coordinated::open(delivery.owner.root());
    assert_eq!(
        reopened
            .backends
            .commit_boundary
            .execution_claim_state(&delivery.claim.claim_id)
            .unwrap()
            .unwrap()
            .settlement,
        expected.settlement
    );
    assert_eq!(
        snapshot(),
        after,
        "repeated opens must not rewrite settlement evidence"
    );
    delivery.accept(Some(OWNER_POLICY)).unwrap();
    delivery.merge_intent(Some(OWNER_POLICY)).unwrap();
    assert_eq!(
        boundary
            .execution_claim_state(&delivery.claim.claim_id)
            .unwrap()
            .unwrap()
            .settlement,
        expected.settlement
    );
}

#[test]
fn already_open_inline_writers_are_refused_atomically_at_the_store_boundary() {
    if !allocation_admission_isolated(
        "handoff_compatibility::already_open_inline_writers_are_refused_atomically_at_the_store_boundary",
    ) {
        return;
    }
    let delivery = Delivery::admit(done_ship());
    let database = delivery.owner.root().join("state.sqlite");
    let stale_writer = Store::open(&database).unwrap();
    let conn = rusqlite::Connection::open(&database).unwrap();
    crate::support::drop_replayed_migration_tables(&conn).unwrap();
    conn.execute_batch(
        "DROP TRIGGER handoff_settlement_insert_guard;
         DROP TRIGGER handoff_settlement_update_guard;
         DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key >= 'migration.v0057';",
    )
    .unwrap();
    let migrated = Store::open(&database).unwrap();
    let before = delivery
        .owner
        .backends
        .commit_boundary
        .resolve_execution_claims()
        .unwrap();
    for insert in [false, true] {
        let result = stale_writer.with_transaction(|transaction| {
        transaction.connection().execute("UPDATE task_coordination_rows SET payload_json='{}' WHERE kind='distributed-execution-claim-v1'", []).unwrap();
        let result = if insert {
            transaction.connection().execute(
                "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at)
                 SELECT workspace_id,kind,row_id || ':new',json_set(payload_json,'$.settlement',json(?1)),journal_id,created_at
                 FROM task_coordination_rows WHERE kind='distributed-claim-lifecycle-v1'",
                [r#"{"kind":"handoff"}"#],
            )
        } else {
            transaction.connection().execute("UPDATE task_coordination_rows SET payload_json=json_set(payload_json,'$.settlement',json(?1)) WHERE kind='distributed-claim-lifecycle-v1'", [r#"{"kind":"handoff"}"#])
        };
        result.map_err(|error| OrbitError::Store(error.to_string()))
        });
        assert!(
            result.is_err(),
            "both legacy insertion and replacement must be refused"
        );
        assert_eq!(
            delivery
                .owner
                .backends
                .commit_boundary
                .resolve_execution_claims()
                .unwrap(),
            before
        );
    }
    assert_eq!(migrated.schema_version().unwrap(), SUPPORTED_SCHEMA_VERSION);
    delivery.accept(Some(OWNER_POLICY)).unwrap();
}

/// The selective policy an earlier owner stored on a claim that captured no
/// before-PR or before-landing timing.
fn persisted_selective_policy() -> orbit_types::workflow::SelectiveBeforeLandingPolicy {
    orbit_types::workflow::SelectiveBeforeLandingPolicy {
        mode: orbit_types::workflow::SelectiveBeforeLandingMode::Observe,
        risk_threshold: None,
        risk_model_identity: None,
        paths: vec!["crates/**".into()],
        unknown_risk: orbit_types::workflow::SelectiveUnknownRisk::Review,
        sources: std::collections::BTreeMap::new(),
        digest: "sha256:legacy-selective".into(),
    }
}

fn selective_only_ship() -> AdmissionShipContract {
    let mut ship = before_pr_ship();
    ship.before_pr = false;
    ship.before_landing = false;
    let review = ship
        .review
        .as_mut()
        .expect("the before-PR fixture carries a review contract");
    review.selective_before_landing = Some(persisted_selective_policy());
    ship
}

fn refused_as(ship: AdmissionShipContract, request_id: &str) -> String {
    let owner = Coordinated::fixture();
    owner.create_task(request_id);
    let mut request = owner_request(request_id);
    request.ship = ship;
    owner
        .try_pull(&request)
        .expect_err("malformed review contract")
        .to_string()
}

#[test]
fn persisted_selective_only_claim_accepts_a_non_light_pr_handoff() {
    if !allocation_admission_isolated(
        "handoff_compatibility::persisted_selective_only_claim_accepts_a_non_light_pr_handoff",
    ) {
        return;
    }
    let ship = selective_only_ship();
    assert!(
        ship.review_contract_consistent(),
        "a retained selective-only contract stays a consistent stored shape"
    );
    let delivery = Delivery::admit(ship);
    assert!(
        matches!(
            delivery.handoff.candidate.delivery,
            orbit_types::workflow::handoff::HandoffDelivery::PullRequest { number: 1.. }
        ),
        "the fixture handoff is a non-light pull request"
    );
    assert_eq!(delivery.handoff.review, HandoffReview::not_required());
    delivery
        .accept(None)
        .expect("timing none needs no review evidence");
    delivery.assert_status(TaskStatus::Review);
    let found = delivery
        .owner
        .backends
        .commit_boundary
        .lookup_admission(
            &AdmissionIdentity::trusted_remote(delivery.claim.executed_on.clone()),
            "first",
        )
        .unwrap();
    let AdmissionLookup::Found { receipt, .. } = found else {
        panic!("stored receipt missing: {found:?}");
    };
    let stored = receipt
        .request
        .ship
        .review
        .as_ref()
        .expect("the selective contract stays on the receipt");
    assert_eq!(
        stored.selective_before_landing.as_ref(),
        Some(&persisted_selective_policy()),
        "selective bytes stay on the stored claim"
    );
    assert!(!receipt.request.ship.before_pr && !receipt.request.ship.before_landing);

    let mut drifted = selective_only_ship();
    drifted
        .review
        .as_mut()
        .unwrap()
        .required_validation_commands = Some(vec!["other".into()]);
    Delivery::admit(drifted)
        .accept(None)
        .expect("a selective command list is not a review requirement");

    let mut evidenced = Delivery::admit(selective_only_ship());
    let issued = certificate(&evidenced.handoff, ReviewVerdict::Accept);
    evidenced.reviewed(&issued);
    let error = evidenced.accept(None).unwrap_err().to_string();
    assert!(
        error.contains(HandoffReviewRefusal::ReviewEvidenceUnexpected.as_str()),
        "{error}"
    );

    for (label, ship) in [
        ("before-pr", before_pr_ship()),
        ("before-landing", before_landing_ship()),
    ] {
        let delivery = Delivery::admit(ship);
        let error = delivery.accept(None).expect_err(label).to_string();
        assert!(
            error.contains(HandoffReviewRefusal::ReviewEvidenceMissing.as_str()),
            "{label}: {error}"
        );
        assert_eq!(
            delivery.owner.task_status(&delivery.claim.task_id),
            TaskStatus::InProgress,
            "{label}"
        );
    }
    let mut landed = before_landing_ship();
    landed.review.as_mut().unwrap().selective_before_landing = Some(persisted_selective_policy());
    let error = Delivery::admit(landed)
        .accept(None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(HandoffReviewRefusal::ReviewEvidenceMissing.as_str()),
        "a selective field does not waive before-landing review: {error}"
    );

    let mut bare = before_pr_ship();
    bare.before_pr = false;
    let refused = refused_as(bare, "bare-review");
    assert!(
        refused.contains("invalid_input"),
        "a review contract with no timing and no selective field is still malformed: {refused}"
    );
    let mut commands = selective_only_ship();
    commands
        .review
        .as_mut()
        .unwrap()
        .required_validation_commands = None;
    let refused = refused_as(commands, "unlisted");
    assert!(
        refused.contains("invalid_input"),
        "a selective contract without its captured command list is still malformed: {refused}"
    );
}
