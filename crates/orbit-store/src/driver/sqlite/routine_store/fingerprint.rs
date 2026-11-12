//! Cheap change tokens over the tables a routine status read draws on.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::Store;

/// Newest fire and job-run rows whose in-place changes the token samples. A
/// fire outcome or run transition rewrites the row of an in-flight or recent
/// one, so the newest rows carry every change a status read can show; an
/// older row rewritten in place leaves the token unchanged, so it does not
/// immediately invalidate a cached value. The change becomes visible on the
/// next successful recomputation, including refresh-ahead when the caller uses
/// it; the caller's freshness bound remains the fallback.
const FINGERPRINT_ROW_WINDOW: i64 = 256;

impl Store {
    /// Change tokens over the tables a routine status read draws on, as
    /// `(routines, job_runs)`.
    ///
    /// `routines` changes when a routine fire, cursor or pause row or an
    /// automation consumer is added or removed, or when a sampled fire row
    /// advances. Those writes are rare and are what an operator acts on, so a
    /// caller invalidates on them at once. Fire in-place updates are sampled
    /// only over the newest `FINGERPRINT_ROW_WINDOW` (256) rows, so an older row
    /// rewritten in place leaves the token unchanged and does not immediately
    /// invalidate cached values; the caller's freshness bound remains the
    /// fallback.
    ///
    /// `job_runs` changes when a job run (the fire record of state-, delivery-
    /// and condition-triggered routines) is added or removed, or when a sampled
    /// run advances, with the same newest-256 sampling. Every run starting or
    /// finishing anywhere on the host moves it, so a caller should tolerate
    /// serving the previous status for its freshness bound rather than wait
    /// out a recompute on each change.
    ///
    /// Unlike the database file's size or modification time both ignore the
    /// rest of the audit store, so unrelated audit traffic leaves them
    /// unchanged.
    pub fn routine_state_fingerprint(&self) -> Result<(String, String), OrbitError> {
        let conn = self.read()?;
        conn.query_row(
            "SELECT
                (SELECT COUNT(*) || '/' || COALESCE(MAX(id), 0) FROM routine_fires),
                (SELECT COALESCE(group_concat(updated_at), '') FROM
                    (SELECT updated_at FROM routine_fires ORDER BY id DESC LIMIT ?1)),
                (SELECT COUNT(*) || '/' || COALESCE(MAX(updated_at), '') FROM routine_cursors),
                (SELECT COUNT(*) || '/' || COALESCE(MAX(paused_at), '') FROM routine_pauses),
                (SELECT COUNT(*) || '/' || COALESCE(SUM(generation), 0) FROM automation_consumers),
                (SELECT COUNT(*) || '/' || COALESCE(MAX(rowid), 0) FROM job_runs),
                (SELECT COALESCE(group_concat(state || COALESCE(finished_at, '')), '') FROM
                    (SELECT state, finished_at FROM job_runs ORDER BY rowid DESC LIMIT ?1))",
            [FINGERPRINT_ROW_WINDOW],
            |row| {
                let column = |index: usize| row.get::<_, String>(index);
                let routines = (0..5).map(column).collect::<Result<Vec<_>, _>>()?.join("|");
                let job_runs = (5..7).map(column).collect::<Result<Vec<_>, _>>()?.join("|");
                Ok((routines, job_runs))
            },
        )
        .map_err(sqlite_store_error)
    }
}
