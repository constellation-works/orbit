//! Sibling tests for `contracts/compat.rs` — the forward-compatibility
//! decision shared by both state ledgers (ORB-12434).

use crate::contracts::compat::{
    BreakingMigration, COMPATIBILITY_RECORD_FORMAT, CompatibilityRecord, CompatibilityRefusal,
    MigrationCompatibility, StateComponent, evaluate_newer_state,
};

fn registry() -> Vec<(u32, &'static str, MigrationCompatibility)> {
    vec![
        (1, "baseline", MigrationCompatibility::Additive),
        (2, "drop_legacy_table", MigrationCompatibility::Breaking),
        (3, "add_index", MigrationCompatibility::Additive),
        (4, "rename_column", MigrationCompatibility::Breaking),
        (5, "add_column", MigrationCompatibility::Additive),
    ]
}

fn record_at(version: u32) -> CompatibilityRecord {
    CompatibilityRecord::for_registry(version, registry())
}

#[test]
fn record_lists_only_breaking_migrations_up_to_its_version() {
    let record = record_at(3);
    assert_eq!(record.format, COMPATIBILITY_RECORD_FORMAT);
    assert_eq!(record.version, 3);
    assert_eq!(
        record.breaking,
        vec![BreakingMigration {
            version: 2,
            name: "drop_legacy_table".to_string(),
        }]
    );
    assert_eq!(record.min_reader_version(), 2);

    let none_breaking = CompatibilityRecord::for_registry(
        2,
        vec![(1u32, "baseline", MigrationCompatibility::Additive)],
    );
    assert!(none_breaking.breaking.is_empty());
    assert_eq!(none_breaking.min_reader_version(), 0);
}

#[test]
fn record_round_trips_and_tolerates_unknown_fields() {
    let record = record_at(5);
    let encoded = record.encode().expect("encode");
    assert_eq!(
        CompatibilityRecord::decode(&encoded).expect("decode"),
        record
    );

    // Format 1 is extended by adding fields; a reader ignores what it does
    // not know rather than refusing a record it can still evaluate.
    let extended = r#"{"format":1,"version":6,"breaking":[],"applied_by":"orbit 9.9.9"}"#;
    let decoded = CompatibilityRecord::decode(extended).expect("decode extended record");
    assert_eq!(decoded.version, 6);
    assert!(decoded.breaking.is_empty());
}

#[test]
fn additive_only_state_opens_read_only() {
    // A binary supporting v4 meets a v5 store: only v5 is missing and v5 is
    // additive.
    let forward = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, Some(record_at(5)))
        .expect("additive-newer state must open read-only");

    assert_eq!(forward.component, StateComponent::StoreSchema);
    assert_eq!(forward.state_version, 5);
    assert_eq!(forward.supported_version, 4);
    assert_eq!(forward.min_reader_version, 4);
}

#[test]
fn breaking_state_refuses_and_names_the_first_missing_breaking_migration() {
    // A binary supporting v1 lacks both breaking migrations; the diagnostic
    // names the first one it lacks, not the newest.
    let refusal = evaluate_newer_state(StateComponent::WorkspaceLayout, 5, 1, Some(record_at(5)))
        .expect_err("breaking-newer state must refuse");

    assert_eq!(
        refusal,
        CompatibilityRefusal::Breaking(BreakingMigration {
            version: 2,
            name: "drop_legacy_table".to_string(),
        })
    );
    let message = refusal.to_string();
    assert!(message.contains("v2 (drop_legacy_table)"), "{message}");
}

#[test]
fn missing_stale_corrupt_and_future_records_all_refuse() {
    let missing = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, None)
        .expect_err("no record must refuse");
    assert_eq!(missing, CompatibilityRefusal::NoRecord);

    // The record predates the recorded version, so the migrations in between
    // are unclassified.
    let stale = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, Some(record_at(4)))
        .expect_err("stale record must refuse");
    assert_eq!(stale, CompatibilityRefusal::StaleRecord { recorded: 4 });

    let mut future = record_at(5);
    future.format = COMPATIBILITY_RECORD_FORMAT + 1;
    let unknown = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, Some(future))
        .expect_err("unknown record format must refuse");
    assert!(matches!(
        unknown,
        CompatibilityRefusal::UnknownRecordFormat { .. }
    ));

    let corrupt = CompatibilityRecord::decode("{not json").expect_err("corrupt record must refuse");
    assert!(matches!(corrupt, CompatibilityRefusal::CorruptRecord(_)));
}
