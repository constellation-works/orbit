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
        (
            6,
            "backfill_projection",
            MigrationCompatibility::ReadCompatible,
        ),
        (7, "add_table", MigrationCompatibility::Additive),
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
fn additive_only_state_keeps_older_writers() {
    // A binary supporting v4 meets a v5 store: only v5 is missing and v5 is
    // additive, so the older binary keeps reading and writing.
    let forward = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, Some(record_at(5)), true)
        .expect("additive-newer state must open");

    assert_eq!(forward.component, StateComponent::StoreSchema);
    assert_eq!(forward.state_version, 5);
    assert_eq!(forward.supported_version, 4);
    assert_eq!(forward.min_reader_version, 4);
    assert!(forward.writable);
}

#[test]
fn read_compatible_state_opens_read_only_where_writes_can_be_gated() {
    // v6 keeps older readers but not older writers.
    let record = record_at(7);
    assert_eq!(
        record.read_only,
        Some(vec![BreakingMigration {
            version: 6,
            name: "backfill_projection".to_string(),
        }])
    );
    let gated = evaluate_newer_state(
        StateComponent::StoreSchema,
        7,
        5,
        Some(record.clone()),
        true,
    )
    .expect("read-compatible state opens read-only");
    assert!(!gated.writable);
    // Past it, only additive work separates the two binaries.
    let past = evaluate_newer_state(
        StateComponent::StoreSchema,
        7,
        6,
        Some(record.clone()),
        true,
    )
    .expect("additive-newer state opens");
    assert!(past.writable);
    // State that cannot be held read-only refuses instead.
    let ungated = evaluate_newer_state(StateComponent::WorkspaceLayout, 7, 5, Some(record), false)
        .expect_err("an ungated reader cannot be kept from writing");
    assert!(
        ungated.to_string().contains("v6 (backfill_projection)"),
        "{ungated}"
    );
}

#[test]
fn record_without_writer_classification_never_grants_gated_writes() {
    // Written by a binary from before older writers were covered.
    let legacy = CompatibilityRecord::decode(r#"{"format":1,"version":5,"breaking":[]}"#)
        .expect("decode legacy record");
    assert_eq!(legacy.read_only, None);
    let gated = evaluate_newer_state(
        StateComponent::StoreSchema,
        5,
        4,
        Some(legacy.clone()),
        true,
    )
    .expect("legacy additive-newer state opens");
    assert!(!gated.writable);
    // The layout's additive declarations always had to keep writers safe.
    let ungated = evaluate_newer_state(StateComponent::WorkspaceLayout, 5, 4, Some(legacy), false)
        .expect("legacy additive-newer layout opens");
    assert!(ungated.writable);
}

#[test]
fn breaking_state_refuses_and_names_the_first_missing_breaking_migration() {
    // A binary supporting v1 lacks both breaking migrations; the diagnostic
    // names the first one it lacks, not the newest.
    let refusal = evaluate_newer_state(
        StateComponent::WorkspaceLayout,
        5,
        1,
        Some(record_at(5)),
        false,
    )
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
    let missing = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, None, true)
        .expect_err("no record must refuse");
    assert_eq!(missing, CompatibilityRefusal::NoRecord);

    // The record predates the recorded version, so the migrations in between
    // are unclassified.
    let stale = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, Some(record_at(4)), true)
        .expect_err("stale record must refuse");
    assert_eq!(stale, CompatibilityRefusal::StaleRecord { recorded: 4 });

    let mut future = record_at(5);
    future.format = COMPATIBILITY_RECORD_FORMAT + 1;
    let unknown = evaluate_newer_state(StateComponent::StoreSchema, 5, 4, Some(future), true)
        .expect_err("unknown record format must refuse");
    assert!(matches!(
        unknown,
        CompatibilityRefusal::UnknownRecordFormat { .. }
    ));

    let corrupt = CompatibilityRecord::decode("{not json").expect_err("corrupt record must refuse");
    assert!(matches!(corrupt, CompatibilityRefusal::CorruptRecord(_)));
}
