// v49 `absorb_feature_ledgers`: the feature registries run to completion in
// the store ledger and freeze. These fixtures need ledger prefixes before v49
// and the frozen registries' prefixes, which no public boundary exposes.
use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::members::{
    MemberAttempt, MemberState, StateMember, StateTriggerKind,
};
use orbit_types::workflow::automation::{AutomationState, SourceRevision};
use orbit_types::workflow::{JobRunState, PipelineState};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::json;

use super::super::feature::{FeatureMigration, apply_feature_registry};
use super::super::feature_ledgers::FEATURE_REGISTRIES;
use super::super::ledger::{self, Migration};
use crate::Store;
use crate::compose;

/// The store registry up to, not including, v49.
fn before_absorption() -> &'static [Migration] {
    let before = ledger::MIGRATIONS
        .iter()
        .position(|m| m.name == "absorb_feature_ledgers")
        .expect("v49 registered");
    &ledger::MIGRATIONS[..before]
}

fn registry(feature: &str) -> &'static [FeatureMigration] {
    FEATURE_REGISTRIES
        .iter()
        .find(|(name, _)| *name == feature)
        .map(|(_, migrations)| *migrations)
        .expect("frozen feature registry")
}

/// A feature store opening as a binary from before v49 did: its registry
/// applied in one write transaction.
fn open_feature(
    conn: &Connection,
    feature: &str,
    migrations: &[FeatureMigration],
) -> Result<(), orbit_common::OrbitError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .expect("begin feature open");
    apply_feature_registry(&tx, feature, migrations)?;
    tx.commit().expect("commit feature open");
    Ok(())
}

/// `(feature, version, name, applied_at)` of every feature ledger row.
fn feature_rows(conn: &Connection) -> Vec<(String, u32, String, String)> {
    let mut statement = conn
        .prepare(
            "SELECT feature, version, name, applied_at FROM feature_schema_meta \
             ORDER BY feature, version",
        )
        .expect("prepare feature rows");
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("read feature rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect feature rows")
}

/// `(feature, version, name)` ledger rows and `(key, value)` compatibility
/// records: what a feature open records, minus timestamps.
type FeatureFootprint = (Vec<(String, u32, String)>, Vec<(String, String)>);

fn feature_footprint(conn: &Connection) -> FeatureFootprint {
    let rows = feature_rows(conn)
        .into_iter()
        .map(|(feature, version, name, _)| (feature, version, name))
        .collect();
    let mut statement = conn
        .prepare(
            "SELECT key, value FROM schema_meta WHERE key LIKE 'feature.compat.%' ORDER BY key",
        )
        .expect("prepare compat records");
    let records = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("read compat records")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect compat records");
    (rows, records)
}

fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
    let mut statement = conn
        .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
        .expect("prepare schema");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("read schema")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect schema")
}

/// The store ledger writes the same feature ledger rows, compatibility
/// records and tables as opening each feature store did before v49.
#[test]
fn absorption_writes_what_each_feature_open_wrote() {
    let opened = Connection::open_in_memory().expect("feature-open fixture");
    ledger::run_migrations(&opened, before_absorption()).expect("migrate to v48");
    for (feature, migrations) in FEATURE_REGISTRIES {
        open_feature(&opened, feature, migrations).expect("open feature");
    }

    let absorbed = Connection::open_in_memory().expect("absorbed fixture");
    ledger::run_migrations(&absorbed, before_absorption()).expect("migrate to v48");
    assert!(
        ledger::run_migrations(
            &absorbed,
            &ledger::MIGRATIONS[..before_absorption().len() + 1]
        )
        .expect("apply v49")
        .is_none()
    );

    let (rows, records) = feature_footprint(&absorbed);
    assert_eq!((rows.clone(), records), feature_footprint(&opened));
    assert_eq!(schema(&absorbed), schema(&opened));
    let newest = |feature: &str| {
        rows.iter()
            .filter(|(name, _, _)| name == feature)
            .map(|(_, version, _)| *version)
            .max()
    };
    assert_eq!(newest("automation"), Some(5));
    assert_eq!(newest("review"), Some(4));
    assert_eq!(newest("local_pull"), Some(1));
}

/// v49 is additive: a binary whose store registry stops at v48 keeps opening
/// the store writable, and its own feature opens find every feature at the
/// version it supports and apply nothing. A binary lacking a data-only
/// feature migration still opens that feature [ORB-15260]; one lacking a
/// breaking feature migration refuses that feature from its record, naming it.
#[test]
fn a_binary_without_absorption_keeps_writing_and_decides_features_from_their_records() {
    let conn = Connection::open_in_memory().expect("fixture");
    // This guarantee belongs to additive v49; a later read-compatible or
    // breaking store migration must still fence a v48 writer.
    ledger::run_migrations(&conn, &ledger::MIGRATIONS[..before_absorption().len() + 1])
        .expect("migrate to v49");
    let rows = feature_rows(&conn);

    let open = ledger::run_migrations(&conn, before_absorption())
        .expect("a v48 binary opens the v49 store")
        .expect("the store is newer than that binary");
    assert!(open.writable, "{open}");
    for (feature, migrations) in FEATURE_REGISTRIES {
        open_feature(&conn, feature, migrations).expect("feature open");
    }
    assert_eq!(feature_rows(&conn), rows, "feature opens applied nothing");

    open_feature(&conn, "automation", &registry("automation")[..4])
        .expect("an automation v4 binary survives the data-only v5");

    for (feature, known, missing) in [
        ("review", 3, "v4 (reconciliation_landed_delivery_binding)"),
        ("automation", 3, "v4 (release_stale_source_failures)"),
    ] {
        let refused = open_feature(&conn, feature, &registry(feature)[..known])
            .expect_err("a binary lacking a breaking feature migration refuses")
            .to_string();
        assert!(refused.contains(missing), "{refused}");
    }
    assert_eq!(feature_rows(&conn), rows, "a refusal writes nothing");
}

fn now() -> DateTime<Utc> {
    "2026-01-01T00:00:00Z".parse().expect("fixture time")
}

fn revision(commit: &str) -> SourceRevision {
    SourceRevision {
        commit: commit.into(),
        tree: format!("{commit}-tree"),
    }
}

fn member(key: &str, source: &str) -> StateMember {
    StateMember {
        key: key.into(),
        task_ids: vec![key.into()],
        fingerprint: format!("{key}-fingerprint"),
        source: revision(source),
        evidence: json!({"task_id": key}),
        first_seen: now(),
        changed_at: now(),
        crew: None,
    }
}

fn failed(member: StateMember, attempt: u32, run_id: &str) -> MemberAttempt {
    MemberAttempt {
        consumer: "fixture/pilot".into(),
        kind: StateTriggerKind::PreparationEligible,
        id: format!("attempt-{}", member.key),
        action_key: format!("automation:attempt-{}:{attempt}", member.key),
        member,
        members: vec![],
        attempt,
        max_attempts: 2,
        deadline: now(),
        retry_after: now(),
        action_id: Some(run_id.into()),
        exhausted: true,
    }
}

fn pilot_state(generation: u64, members: MemberState) -> AutomationState {
    AutomationState {
        members: Some(members),
        consumer: "fixture/pilot".into(),
        epoch: "epoch".into(),
        trigger: None,
        repository: "fixture-repo".into(),
        branch: "fixture-branch".into(),
        generation,
        baseline: revision("base"),
        observed: revision("base"),
        covered: revision("base"),
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        active: None,
        stall: None,
    }
}

/// A store as a binary before the member repairs left it: the store ledger
/// at v48 and automation at v3, holding `state`. `seed` adds other rows.
fn automation_v3_store(path: &Path, state: &AutomationState, seed: impl FnOnce(&Connection)) {
    let conn = Connection::open(path).expect("open fixture database");
    ledger::run_migrations(&conn, before_absorption()).expect("migrate to v48");
    open_feature(&conn, "automation", &registry("automation")[..3]).expect("automation v3");
    conn.execute(
        "INSERT INTO automation_consumers VALUES (?1,?2,?3)",
        rusqlite::params![
            state.consumer,
            state.generation,
            serde_json::to_string(state).expect("encode state")
        ],
    )
    .expect("seed consumer");
    seed(&conn);
}

/// The automation state the next store open leaves, after it applies v49.
fn after_absorption(path: &Path, consumer: &str) -> AutomationState {
    let store = Store::open(path).expect("open and apply v49");
    compose::automation_store(store)
        .expect("automation store")
        .automation_state(consumer)
        .expect("read state")
        .expect("consumer kept")
}

/// Automation v4 [ORB-14476], as v49 runs it: a task-pilot member a branch
/// move shelved before supersession existed, its exhausted failure record at
/// the fingerprint it is still pending at, while pending at a newer source,
/// is released; a record at the pending source stays.
#[test]
fn absorbed_repair_releases_members_shelved_at_a_source_the_branch_left() {
    let shelved = |key: &str| failed(member(key, "frozen"), 2, "run");
    let state = pilot_state(
        7,
        MemberState {
            pending: [
                ("moved".into(), member("moved", "head")),
                ("unmoved".into(), member("unmoved", "frozen")),
            ]
            .into(),
            failed: [
                ("moved".into(), shelved("moved")),
                ("unmoved".into(), shelved("unmoved")),
            ]
            .into(),
            withheld: [
                ("moved".into(), "stopped_without_member_evidence".into()),
                ("unmoved".into(), "stopped_without_member_evidence".into()),
            ]
            .into(),
            ..Default::default()
        },
    );
    let dir = tempfile::tempdir().expect("fixture dir");
    let path = dir.path().join("orbit.db");
    automation_v3_store(&path, &state, |_| {});

    let repaired = after_absorption(&path, &state.consumer);
    assert_eq!(repaired.generation, 8, "the repair fences older snapshots");
    let members = repaired.members.expect("member state");
    assert!(!members.failed.contains_key("moved"));
    assert!(!members.withheld.contains_key("moved"));
    assert_eq!(members.pending["moved"].source, revision("head"));
    assert_eq!(
        members.failed.get("unmoved"),
        state.members.as_ref().and_then(|m| m.failed.get("unmoved")),
        "a member shelved at the source it is pending at stays retired"
    );
    assert!(members.withheld.contains_key("unmoved"));

    // Applied once: the next open leaves the repaired state alone.
    assert_eq!(after_absorption(&path, &state.consumer).generation, 8);
}

/// `(step index, pipeline output id, output)` a run checkpointed.
type Checkpoint = (u32, &'static str, serde_json::Value);

/// Automation v5 [ORB-15197], as v49 runs it: a task-pilot member settled
/// from the checkpoint at apply's former position after a step was inserted
/// before apply is released; a record whose run checkpointed apply there, or
/// whose run is unknown, stays.
#[test]
fn absorbed_repair_releases_pilot_failures_settled_from_another_steps_checkpoint() {
    let pilots = json!([{"partition_index": 0, "tasks": []}]);
    let apply = json!({"member_evidence": [], "repair_count": 1});
    let runs: [(&str, &str, Vec<Checkpoint>); 4] = [
        // Settled from the fan-in while apply had yet to run.
        (
            "running",
            "jrun-running",
            vec![(2, "pilots", pilots.clone())],
        ),
        // Settled from the fan-in; apply and its repair ran afterwards.
        (
            "applied",
            "jrun-applied",
            vec![(2, "pilots", pilots.clone()), (3, "apply", apply.clone())],
        ),
        // Before the inserted step, apply itself sat at that position.
        ("read", "jrun-read", vec![(2, "apply", apply)]),
        ("unknown", "missing-run", vec![]),
    ];
    let state = pilot_state(
        3,
        MemberState {
            pending: runs
                .iter()
                .map(|(key, _, _)| (key.to_string(), member(key, "head")))
                .collect(),
            failed: runs
                .iter()
                .map(|(key, run_id, _)| (key.to_string(), failed(member(key, "head"), 1, run_id)))
                .collect(),
            withheld: runs
                .iter()
                .map(|(key, _, _)| (key.to_string(), "no_member_evidence".into()))
                .collect(),
            ..Default::default()
        },
    );
    let dir = tempfile::tempdir().expect("fixture dir");
    let path = dir.path().join("orbit.db");
    automation_v3_store(&path, &state, |conn| {
        for (_, run_id, steps) in runs.iter().filter(|(key, _, _)| *key != "unknown") {
            let mut run = PipelineState::new(
                (*run_id).to_string(),
                "task_pilot_pipeline".into(),
                json!({}),
            );
            for (index, id, output) in steps {
                run.record_step(*index, JobRunState::Success, Some(output.clone()), None);
                run.record_pipeline_output(id, output.clone());
            }
            conn.execute(
                "INSERT INTO job_runs(run_id, workspace_id, job_id, attempt, state, \
                 scheduled_at, created_at) VALUES (?1, 'workspace', 'task_pilot_pipeline', 1, \
                 'running', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
                [run_id],
            )
            .expect("seed run");
            conn.execute(
                "INSERT INTO job_run_states(workspace_id, run_id, pipeline_state_json) \
                 VALUES ('workspace', ?1, ?2)",
                rusqlite::params![run_id, serde_json::to_string(&run).expect("encode run")],
            )
            .expect("seed run state");
        }
    });

    let repaired = after_absorption(&path, &state.consumer);
    assert_eq!(repaired.generation, 4, "the repair fences older snapshots");
    let members = repaired.members.expect("member state");
    for key in ["running", "applied"] {
        assert!(!members.failed.contains_key(key), "{key} is released");
        assert!(!members.withheld.contains_key(key), "{key} is released");
        assert!(members.pending.contains_key(key), "{key} stays observed");
    }
    for key in ["read", "unknown"] {
        assert_eq!(
            members.failed.get(key),
            state.members.as_ref().and_then(|m| m.failed.get(key)),
            "{key} keeps its failure record"
        );
        assert!(members.withheld.contains_key(key));
    }

    // Applied once: the next open leaves the repaired state alone.
    assert_eq!(after_absorption(&path, &state.consumer).generation, 4);
}
