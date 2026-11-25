use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v52 `provider_crew_outages` [ORB-16379]: the crews this host could not
/// run because their provider was unavailable here (an expired login, say),
/// beside the usage limits in the host-global database, since an outage
/// belongs to the provider login on this host too.
///
/// One row per provider and crew; the empty string stands for a provider the
/// failure did not name. Purely additive: an older binary never reads the
/// table.
pub(super) fn apply_provider_crew_outages(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS provider_crew_outages (
                provider TEXT NOT NULL DEFAULT '',
                crew TEXT NOT NULL,
                until_at TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                run_id TEXT NOT NULL,
                PRIMARY KEY (provider, crew)
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
