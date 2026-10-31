use super::super::super::ledger::{self, Migration};
use super::super::super::*;
use rusqlite::Connection;

/// The two resource-minute tables, and the admission reservations and
/// forecasts after them, are additive: a binary without them keeps opening the migrated database
/// read-write, and upgrade admission sees no new writer or reader floor, so
/// they force no generation switch while old drains still run.
#[test]
fn resource_minute_tables_keep_an_older_binary_writing() {
    let position = |name: &str| {
        ledger::MIGRATIONS
            .iter()
            .position(|migration| migration.name == name)
            .expect("registered migration")
    };
    let first = position("run_resource_minutes");
    let last = position("worker_cpu_accounting");
    let newer = &ledger::MIGRATIONS[..=last];
    let older = &ledger::MIGRATIONS[..first];

    let conn = Connection::open_in_memory().expect("open in-memory connection");
    ledger::run_migrations(&conn, &newer[..last]).expect("pre-provenance schema");
    let legacy_insert = "INSERT INTO run_resource_minutes (
        workspace_id, run_id, minute_index, minute_started_at, task_id,
        work_class, cpu_cores, memory_bytes, samples
    ) VALUES ('ws_a', ?1, 0, '2026-10-13T00:00:00Z', 'task', 'change', 0.03, 4096, 12)";
    conn.execute(legacy_insert, ["before"]).expect("old sample");
    assert!(
        ledger::run_migrations(&conn, newer)
            .expect("migrate")
            .is_none()
    );
    for table in [
        "run_resource_minutes",
        "host_resource_minutes",
        "admission_reservations",
        "admission_forecasts",
    ] {
        assert!(table_exists(&conn, table).expect("introspect"), "{table}");
    }
    let open = ledger::run_migrations(&conn, older)
        .expect("an older binary opens the database")
        .expect("the database is newer than that binary");
    assert!(open.writable, "{open}");

    let floors = |registry: &[Migration]| {
        let compatibility = orbit_common::fs::generation::LedgerCompatibility::from_registry(
            registry
                .iter()
                .map(|m| (m.version, m.compat.is_write_safe(), !m.compat.is_breaking())),
        );
        (compatibility.writer_floor, compatibility.reader_floor)
    };
    assert_eq!(floors(newer), floors(older));
    conn.execute(legacy_insert, ["after"])
        .expect("an old writer can still insert");
    let mut statement = conn
        .prepare("SELECT cpu_cores, memory_bytes, cpu_accounting FROM run_resource_minutes")
        .expect("raw historical evidence");
    let rows: Vec<(f64, u64, Option<String>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("read minutes")
        .collect::<Result<_, _>>()
        .expect("raw rows");
    assert_eq!(rows, vec![(0.03, 4096, None); 2]);
}

/// The executor census table is additive [ORB-17304]: a binary without it
/// keeps opening the migrated database read-write under the same writer and
/// reader floors, so it forces no generation switch while old drains run,
/// and that binary's coordination writes still land beside the census.
#[test]
fn executor_census_keeps_an_older_binary_writing() {
    let census = ledger::MIGRATIONS
        .iter()
        .position(|migration| migration.name == "executor_census")
        .expect("registered migration");
    let newer = &ledger::MIGRATIONS[..=census];
    let older = &ledger::MIGRATIONS[..census];

    let conn = Connection::open_in_memory().expect("open in-memory connection");
    assert!(
        ledger::run_migrations(&conn, newer)
            .expect("migrate")
            .is_none()
    );
    assert!(table_exists(&conn, "executor_census").expect("introspect"));
    conn.execute(
        "INSERT INTO executor_census(workspace_id, machine_id, capability_json, run_id, seen_at)
         VALUES ('ws_a', 'mac', '{}', 'drain', '2026-10-21T00:00:00Z')",
        [],
    )
    .expect("a census row");
    let open = ledger::run_migrations(&conn, older)
        .expect("an older binary opens the database")
        .expect("the database is newer than that binary");
    assert!(open.writable, "{open}");

    let floors = |registry: &[Migration]| {
        let compatibility = orbit_common::fs::generation::LedgerCompatibility::from_registry(
            registry
                .iter()
                .map(|m| (m.version, m.compat.is_write_safe(), !m.compat.is_breaking())),
        );
        (compatibility.writer_floor, compatibility.reader_floor)
    };
    assert_eq!(floors(newer), floors(older));
    conn.execute(
        "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
         VALUES ('ws_a', 'distributed-admission-receipt-v1', 'r', '{}', 'sql-only', '2026-10-21T00:00:00Z')",
        [],
    )
    .expect("an old writer can still record a receipt");
}
