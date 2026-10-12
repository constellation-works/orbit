use orbit_common::OrbitError;
use rusqlite::Connection;

/// v47 `admission_reservations`: one row per task an admitter decided to
/// start, written under the host admission lock, so a drain in another
/// process counts it before its run exists in `job_runs`.
///
/// The work class, complexity and base class are the profile lookup key the
/// admitter chose. `reserved_at` is fixed-width UTC RFC3339 text, so expiry
/// compares it as text. A new table only: an older binary never reads or
/// writes it.
pub(super) fn apply_admission_reservations(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS admission_reservations (
                workspace_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                work_class TEXT NOT NULL,
                complexity TEXT,
                base_class TEXT NOT NULL,
                reserved_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_admission_reservations_reserved
            ON admission_reservations(reserved_at);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
