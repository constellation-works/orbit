use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v79: an experiment's events stay append-only while the experiment exists.
/// `orbit experiment delete` removes the experiment row first and then its
/// events in the same transaction; any other delete of an event is still
/// refused, and updates always are.
pub(super) fn apply_experiment_delete(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "
            DROP TRIGGER IF EXISTS experiment_events_no_delete;
            CREATE TRIGGER experiment_events_no_delete
                BEFORE DELETE ON experiment_events
                WHEN EXISTS (
                    SELECT 1 FROM experiments
                    WHERE workspace_id = OLD.workspace_id AND id = OLD.experiment_id
                )
                BEGIN SELECT RAISE(ABORT, 'experiment_events is append-only'); END;
            ",
    )
    .map_err(sqlite_store_error)
}
