use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// Additive, independent assistance tables. No triggers, projections or
/// changes to existing task, review, job or human-decision records.
pub(super) fn apply_decision_assistance(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE assistance_work (
            id TEXT PRIMARY KEY,
            slot TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            use_case TEXT NOT NULL,
            policy_digest TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            retain_until INTEGER NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('queued', 'claimed', 'done')),
            attempt_count INTEGER NOT NULL DEFAULT 0,
            ready_at INTEGER NOT NULL,
            claim_until INTEGER,
            claim_id TEXT,
            result_json TEXT
        );
        CREATE INDEX assistance_queue ON assistance_work(workspace_id, use_case, policy_digest, state, ready_at);
        CREATE INDEX assistance_expiry ON assistance_work(state, claim_until, retain_until);
        CREATE TABLE assistance_slots (
            slot TEXT PRIMARY KEY,
            request_id TEXT NOT NULL REFERENCES assistance_work(id) ON DELETE CASCADE
        );
        CREATE TABLE assistance_attempts (
            attempt_id TEXT PRIMARY KEY,
            request_id TEXT NOT NULL REFERENCES assistance_work(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            started_at INTEGER NOT NULL,
            deadline INTEGER NOT NULL,
            outcome_unknown INTEGER NOT NULL DEFAULT 0,
            reserved_tokens INTEGER NOT NULL,
            reserved_charge INTEGER NOT NULL,
            result_json TEXT,
            late_result_json TEXT,
            UNIQUE(request_id, ordinal)
        );
        CREATE TABLE assistance_budgets (
            epoch TEXT PRIMARY KEY,
            reserved_tokens INTEGER NOT NULL DEFAULT 0,
            reserved_charge INTEGER NOT NULL DEFAULT 0,
            attempts INTEGER NOT NULL DEFAULT 0,
            unknown_attempts INTEGER NOT NULL DEFAULT 0,
            queue_rejections INTEGER NOT NULL DEFAULT 0
        );",
    ).map_err(sqlite_store_error)
}
