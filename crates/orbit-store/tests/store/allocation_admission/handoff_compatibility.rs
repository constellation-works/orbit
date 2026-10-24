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

#[derive(serde::Deserialize)]
struct ReleasedSettlement {
    kind: ReleasedSettlementKind,
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
    // still settle through the public boundary and remain readable by old clients.
    for (key, kind) in [
        ("legacy-fail", ReleasedSettlementKind::Fail),
        ("legacy-release", ReleasedSettlementKind::Release),
    ] {
        delivery.owner.create_task_in(key, &[key]);
        let claim = delivery.owner.pull(&owner_request(key)).claim.unwrap();
        let auth = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id,
            None,
        );
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
        let legacy: ReleasedLifecycle =
            serde_json::from_value(serde_json::to_value(record).unwrap()).unwrap();
        assert_eq!(legacy.settlement.unwrap().kind, kind);
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
    conn.execute("DELETE FROM schema_meta WHERE key='migration.v0057'", [])
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
    conn.execute_batch("DROP TRIGGER handoff_settlement_insert_guard; DROP TRIGGER handoff_settlement_update_guard; DELETE FROM schema_meta WHERE key='migration.v0057';").unwrap();
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
    assert_eq!(migrated.schema_version().unwrap(), 57);
    delivery.accept(Some(OWNER_POLICY)).unwrap();
}
