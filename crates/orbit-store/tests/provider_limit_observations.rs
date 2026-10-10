//! [ORB-14695] The host's provider usage limits: the latest observation per
//! provider, model and window wins, and the table arrives in an existing
//! store by an additive migration.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]
use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::{process, test_env};
use orbit_store::Store;
use orbit_store::compose::provider_limit_store_from_store;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use rusqlite::Connection;
use std::process::Command;

/// Run `test` in a child process with its own home, so opening a store
/// cannot touch the caller's Orbit state. Returns whether this is the child.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_PROVIDER_LIMIT_STORE_TEST_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let result =
        process::run_bounded_capped(&mut command, std::time::Duration::from_secs(60), 256 * 1024)
            .unwrap();
    test_env::assert_child_test_passed(test, result.status, result.stdout, result.stderr);
    false
}

fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 8, hour, minute, 0).unwrap()
}

fn observation(
    provider: &str,
    observed_at: DateTime<Utc>,
    resets_at: Option<DateTime<Utc>>,
) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: provider.to_string(),
        model: None,
        window: None,
        exhausted: true,
        source: ProviderLimitSource::Error,
        resets_at,
        observed_at,
        run_id: Some(format!("run-{}", observed_at.timestamp())),
        crew: Some("gemini-flash".to_string()),
        detail: "Individual quota reached.".to_string(),
        used_percent: None,
        window_minutes: None,
        gating: true,
        partial: false,
    }
}

/// One row per provider, model and window: a newer observation replaces it,
/// an older one does not, and another scope gets its own row.
#[test]
fn the_latest_observation_per_provider_scope_wins() {
    if !isolated("the_latest_observation_per_provider_scope_wins") {
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let store =
        provider_limit_store_from_store(Store::open(&root.path().join("orbit.db")).unwrap());

    let first = observation("antigravity", at(14, 0), Some(at(15, 37)));
    assert!(store.record_provider_limit(&first).unwrap());
    let newer = observation("antigravity", at(14, 30), Some(at(16, 7)));
    assert!(
        store.record_provider_limit(&newer).unwrap(),
        "a newer one replaces it"
    );
    let older = observation("antigravity", at(13, 0), Some(at(13, 30)));
    assert!(
        !store.record_provider_limit(&older).unwrap(),
        "an older one is not stored"
    );
    assert_eq!(
        store.provider_limits().unwrap(),
        std::slice::from_ref(&newer)
    );

    // Microseconds survive, so two failures in one second still order.
    let close = observation("antigravity", at(14, 30) + Duration::microseconds(7), None);
    assert!(store.record_provider_limit(&close).unwrap());
    assert_eq!(
        store.provider_limits().unwrap(),
        std::slice::from_ref(&close)
    );

    let opus = ProviderLimitObservation {
        model: Some("opus".to_string()),
        window: Some("seven_day_opus".to_string()),
        ..observation("claude", at(12, 0), Some(at(23, 0)))
    };
    assert!(store.record_provider_limit(&opus).unwrap());
    assert_eq!(
        store.provider_limits().unwrap(),
        [close.clone(), opus.clone()],
        "newest first, one row per scope"
    );

    // [ORB-14696] A provider's own reading of a window keeps its percent,
    // length and gating, and replaces the older failure on the same key.
    let reading = ProviderLimitObservation {
        exhausted: false,
        source: ProviderLimitSource::Event,
        used_percent: Some(91.5),
        window_minutes: Some(10080),
        gating: false,
        partial: false,
        detail: "status=allowed".to_string(),
        ..observation("claude", at(12, 30), Some(at(23, 0)))
    };
    let reading = ProviderLimitObservation {
        model: opus.model.clone(),
        window: opus.window.clone(),
        ..reading
    };
    assert!(store.record_provider_limit(&reading).unwrap());
    assert_eq!(store.provider_limits().unwrap(), [close, reading]);
}

/// A store from before the table gains it, with its v41 reading columns, on
/// open without touching what it held, and records limits afterwards.
#[test]
fn an_existing_store_gains_the_provider_limit_table_additively() {
    if !isolated("an_existing_store_gains_the_provider_limit_table_additively") {
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = root.path().join("orbit.db");
    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    // Recreate schema v39, with a row the older binary wrote.
    conn.execute_batch(
        "DROP TABLE provider_limit_observations;
        DELETE FROM schema_meta WHERE key IN ('migration.v0040', 'migration.v0041', 'migration.v0042');
        INSERT INTO audit_events (execution_id, timestamp, command, role, status,
            exit_code, duration_ms, working_directory, pid)
        VALUES ('preserved', '2026-10-01T00:00:00Z', 'tool', 'codex', 'failure', 1, 1, '.', 1);",
    )
    .unwrap();
    drop(conn);

    for _ in 0..2 {
        let store = provider_limit_store_from_store(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        let migration: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'migration.v0040'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migration, "provider_limit_observations");
        let row: (String, String) = conn
            .query_row("SELECT execution_id, status FROM audit_events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("preserved".into(), "failure".into()));

        let limit = observation("codex", Utc::now(), None);
        assert!(store.record_provider_limit(&limit).unwrap());
        assert_eq!(store.provider_limits().unwrap().len(), 1);
    }
}

/// [ORB-14696] A v40 table gains the reading columns on open. A row an older
/// binary wrote reads back as a gating failure with no percent.
#[test]
fn a_v40_table_gains_the_reading_columns_additively() {
    if !isolated("a_v40_table_gains_the_reading_columns_additively") {
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = root.path().join("orbit.db");
    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    // Recreate schema v40, with a row the older binary wrote.
    conn.execute_batch(
        "ALTER TABLE provider_limit_observations DROP COLUMN used_percent;
        ALTER TABLE provider_limit_observations DROP COLUMN window_minutes;
        ALTER TABLE provider_limit_observations DROP COLUMN gating;
        DELETE FROM schema_meta WHERE key IN ('migration.v0041', 'migration.v0042');
        INSERT INTO provider_limit_observations (provider, model_scope, window_label,
            exhausted, source, resets_at, observed_at, run_id, crew, detail)
        VALUES ('codex', '', '', 1, 'error', NULL, '2026-10-08T14:00:00.000000Z',
            'run-1', 'sol', 'You''ve hit your usage limit.');",
    )
    .unwrap();
    drop(conn);

    let store = provider_limit_store_from_store(Store::open(&path).unwrap());
    let [row] = store.provider_limits().unwrap().try_into().unwrap();
    assert_eq!(row.source, ProviderLimitSource::Error);
    assert_eq!((row.used_percent, row.window_minutes), (None, None));
    assert!(row.gating, "a failure an older binary recorded still gates");
}
