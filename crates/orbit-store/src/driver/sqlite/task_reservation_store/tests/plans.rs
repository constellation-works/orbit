//! Active `task_reservations` statements must seek an index [ORB-24410].
//!
//! The scope predicate used to test `?1 IS NOT NULL` in SQL, which SQLite
//! cannot fold, so every active read scanned the unpruned table.

use super::super::rows::{
    HOLDS_PATHS, LEASE_ROWS, TASK_RESERVATION_ROWS, reservation_scope_clause,
};
use crate::{Store, TaskReservationScope};
use rusqlite::Connection;

#[test]
fn active_reservation_statements_seek_an_index() {
    let store = Store::open_in_memory().expect("open store");
    let connection = store.connection();
    let conn = connection.lock().expect("store connection");
    let mut scanned = Vec::new();
    for workspace_id in [Some("ws"), None] {
        let arm = if workspace_id.is_some() {
            "workspace id"
        } else {
            "orbit dir only"
        };
        for (label, scope, body) in statement_shapes() {
            let sql = body.replace("{clause}", &reservation_scope_clause(scope, workspace_id));
            let plan = query_plan(&conn, &sql);
            if plan
                .iter()
                .any(|line| line.contains("SCAN task_reservations"))
            {
                scanned.push(format!("{label} ({arm}): {plan:?}"));
            }
        }
    }
    assert!(
        scanned.is_empty(),
        "active task_reservations statements must seek an index, not scan the table (ORB-24410): {scanned:?}"
    );
}

fn statement_shapes() -> Vec<(&'static str, TaskReservationScope, String)> {
    let files = TaskReservationScope::Files;
    let claim = TaskReservationScope::WorkspaceClaim;
    let holds = HOLDS_PATHS;
    let tasks = TASK_RESERVATION_ROWS;
    let leases = LEASE_ROWS;
    vec![
        (
            "expire task reservations",
            files,
            format!(
                "SELECT reservation_id, expires_at FROM task_reservations
                 WHERE {{clause}} AND {tasks} AND released_at IS NULL AND expires_at <= ?3
                 ORDER BY expires_at ASC, reservation_id ASC"
            ),
        ),
        (
            "expire task reservations write",
            files,
            format!(
                "UPDATE task_reservations SET released_at = ?3
                 WHERE {{clause}} AND {tasks} AND released_at IS NULL AND expires_at <= ?3"
            ),
        ),
        (
            "expire leases",
            files,
            format!(
                "SELECT reservation_id, expires_at FROM task_reservations
                 WHERE {{clause}} AND {leases} AND released_at IS NULL AND expires_at <= ?3
                 ORDER BY expires_at ASC, reservation_id ASC"
            ),
        ),
        (
            "expire leases write",
            files,
            format!(
                "UPDATE task_reservations SET released_at = ?3
                 WHERE {{clause}} AND {leases} AND released_at IS NULL AND expires_at <= ?3"
            ),
        ),
        (
            "expire workspace claims",
            claim,
            "SELECT reservation_id, expires_at FROM task_reservations
             WHERE {clause} AND released_at IS NULL AND expires_at <= ?3
             ORDER BY expires_at ASC, reservation_id ASC"
                .to_string(),
        ),
        (
            "expire workspace claims write",
            claim,
            "UPDATE task_reservations SET released_at = ?3
             WHERE {clause} AND released_at IS NULL AND expires_at <= ?3"
                .to_string(),
        ),
        (
            "visit reservation conflicts",
            files,
            format!(
                "SELECT reservation_id, files_json, kind FROM task_reservations
                 WHERE {{clause}} AND {holds} AND released_at IS NULL AND expires_at > ?3
                 ORDER BY created_at ASC, reservation_id ASC"
            ),
        ),
        (
            "active reservations by owner",
            files,
            "SELECT reservation_id FROM task_reservations
             WHERE {clause} AND owner_run_id = ?3 AND released_at IS NULL
             ORDER BY created_at ASC, reservation_id ASC"
                .to_string(),
        ),
        (
            "owned reservation conflicts",
            files,
            "SELECT reservation_id FROM task_reservations
             WHERE {clause} AND released_at IS NULL AND expires_at > ?3
               AND owner_run_id IS NOT NULL
             ORDER BY created_at ASC, reservation_id ASC"
                .to_string(),
        ),
        (
            "inspect active reservations",
            files,
            format!(
                "SELECT reservation_id FROM task_reservations
                 WHERE {{clause}} AND {holds} AND released_at IS NULL AND expires_at > ?3
                 ORDER BY created_at ASC, reservation_id ASC"
            ),
        ),
        (
            "inspect one active reservation",
            files,
            format!(
                "SELECT reservation_id FROM task_reservations
                 WHERE {{clause}} AND {holds} AND reservation_id = ?4
                   AND released_at IS NULL AND expires_at > ?3"
            ),
        ),
        (
            "release one reservation",
            files,
            format!(
                "UPDATE task_reservations SET released_at = ?4
                 WHERE {{clause}} AND {tasks} AND reservation_id = ?3 AND released_at IS NULL"
            ),
        ),
        (
            "release reservations by owner",
            files,
            "UPDATE task_reservations SET released_at = ?4
             WHERE {clause} AND owner_run_id = ?3 AND released_at IS NULL"
                .to_string(),
        ),
        (
            "renew lease",
            files,
            format!(
                "UPDATE task_reservations SET expires_at = ?4
                 WHERE {{clause}} AND {leases} AND reservation_id = ?3
                   AND released_at IS NULL AND expires_at > ?5"
            ),
        ),
        (
            "release lease",
            files,
            format!(
                "UPDATE task_reservations SET released_at = ?4
                 WHERE {{clause}} AND {leases} AND reservation_id = ?3 AND released_at IS NULL"
            ),
        ),
        (
            "active leases",
            files,
            format!(
                "SELECT reservation_id FROM task_reservations
                 WHERE {{clause}} AND {leases} AND released_at IS NULL AND expires_at > ?3
                 ORDER BY created_at ASC, reservation_id ASC"
            ),
        ),
        (
            "active principal leases",
            files,
            format!(
                "SELECT COUNT(*) FROM task_reservations
                 WHERE {{clause}} AND {leases} AND principal = ?3
                   AND released_at IS NULL AND expires_at > ?4"
            ),
        ),
        (
            "lease queue",
            files,
            format!(
                "SELECT reservation_id FROM task_reservations
                 WHERE {{clause}} AND {leases} AND released_at IS NULL AND expires_at > ?3
                   AND queue_kind IS NOT NULL AND queued_files_json IS NOT NULL"
            ),
        ),
        (
            "lease overlap notices",
            files,
            format!(
                "SELECT reservation_id FROM task_reservations
                 WHERE {{clause}} AND {leases} AND released_at IS NULL AND expires_at > ?3
                   AND reservation_id != ?4 ORDER BY reservation_id"
            ),
        ),
        (
            "release workspace claim",
            claim,
            "UPDATE task_reservations SET released_at = ?4
             WHERE {clause} AND reservation_id = ?3 AND released_at IS NULL"
                .to_string(),
        ),
        (
            "load active workspace claim",
            claim,
            "SELECT reservation_id FROM task_reservations
             WHERE {clause} AND released_at IS NULL AND expires_at > ?3
             ORDER BY created_at ASC, reservation_id ASC LIMIT 1"
                .to_string(),
        ),
    ]
}

fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let count = (1..=9)
        .filter(|index| sql.contains(&format!("?{index}")))
        .max()
        .unwrap_or(0);
    let values = vec!["bound"; count];
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare plan");
    stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| row.get(3))
        .expect("query plan")
        .collect::<Result<Vec<String>, _>>()
        .expect("plan rows")
}
