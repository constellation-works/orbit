//! ORB-24411: a task write and a footprint guard decode the live claims, not
//! every claim the workspace has ever admitted.

use std::time::Duration;

use orbit_types::task::TaskStatus;
use rusqlite::Connection;

use super::super::admission::{claim_decodes, reset_claim_decodes};
use super::super::lifecycle::CLAIM;
use super::claim_section::Owner;
use crate::contracts::*;
use crate::driver::sqlite::task_commit_journal::CLAIM_ROWS_FOR_TASK_SQL;

const SETTLED_CLAIMS: usize = 1_000;
const CLAIM_BY_TASK_INDEX: &str = "idx_task_coordination_claims_by_task";

/// One `with_task_lock` and one `guard_ordinary_footprint` each decode at most
/// the two live claims when a thousand settled claims share the workspace, and
/// the by-task statement's plan names the v90 index.
#[test]
fn a_task_write_and_a_footprint_guard_decode_only_live_claims() {
    let owner = Owner::open(1);
    let admitted = owner.admit("live");
    seed_other_claims(
        &owner.state_database(),
        owner.boundary.workspace_id(),
        &admitted.claim.footprint,
    );

    reset_claim_decodes();
    let locked = owner
        .tasks
        .with_task_lock(&admitted.claim.task_id, || Ok(()));
    let lock_decodes = claim_decodes();
    let lock_message = locked
        .expect_err("the live claim refuses the write")
        .to_string();
    assert!(
        lock_message.contains("active execution claim"),
        "{lock_message}"
    );
    assert_eq!(
        lock_decodes, 1,
        "with_task_lock decoded {lock_decodes} claims; only this task's live claim matches"
    );

    reset_claim_decodes();
    let guarded = owner
        .boundary
        .guard_ordinary_footprint(TaskStatus::InProgress, &admitted.claim.footprint);
    let guard_decodes = claim_decodes();
    let guard_message = guarded
        .expect_err("the live claim refuses work on its footprint")
        .to_string();
    assert!(
        guard_message.contains("overlaps an execution claim"),
        "{guard_message}"
    );
    assert_eq!(
        guard_decodes, 2,
        "guard_ordinary_footprint decoded {guard_decodes} claims; two protect a footprint and \
         {SETTLED_CLAIMS} are settled"
    );

    let plan = explain_claim_task_plan(
        &owner.state_database(),
        owner.boundary.workspace_id(),
        &admitted.claim.task_id,
    );
    assert!(
        plan.contains(CLAIM_BY_TASK_INDEX),
        "by-task EXPLAIN QUERY PLAN does not name {CLAIM_BY_TASK_INDEX}: {plan}"
    );
}

/// A thousand landed claims plus one other live claim. The admitted claim is
/// the second live row and the only one for its task.
fn seed_other_claims(database: &std::path::Path, workspace: &str, footprint: &[String]) {
    let mut conn = Connection::open(database).expect("open store");
    conn.busy_timeout(Duration::from_secs(5))
        .expect("busy timeout");
    let tx = conn.transaction().expect("seed transaction");
    for index in 0..SETTLED_CLAIMS {
        insert_claim(
            &tx,
            workspace,
            &claim_payload(
                &format!("settled-{index:04}"),
                &format!("ORB-SETTLED-{index:04}"),
                ExecutionClaimPhase::Landed,
                footprint.to_vec(),
            ),
        );
    }
    insert_claim(
        &tx,
        workspace,
        &claim_payload(
            "live-other",
            "ORB-LIVE-OTHER",
            ExecutionClaimPhase::Running,
            vec!["file:src/other.rs".into()],
        ),
    );
    tx.commit().expect("commit seeded claims");
}

fn insert_claim(tx: &rusqlite::Transaction<'_>, workspace: &str, claim: &ExecutionClaim) {
    tx.execute(
        "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
         VALUES (?1, ?2, ?3, ?4, 'sql-only', '2026-01-01T00:00:00Z')",
        rusqlite::params![
            workspace,
            CLAIM,
            claim.claim_id,
            serde_json::to_string(claim).expect("claim json"),
        ],
    )
    .expect("insert claim");
}

fn claim_payload(
    id: &str,
    task_id: &str,
    phase: ExecutionClaimPhase,
    footprint: Vec<String>,
) -> ExecutionClaim {
    ExecutionClaim {
        claim_id: id.into(),
        task_id: task_id.into(),
        request_id: format!("req-{id}"),
        executed_on: ExecutionLocation {
            machine_id: "machine-a".into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: format!("run-{id}"),
            job_name: "pull".into(),
            machine_name: None,
        },
        footprint,
        reservation_id: format!("res-{id}"),
        reservation_expires_at: "2099-01-01T00:00:00Z".into(),
        phase,
        repair: None,
    }
}

fn explain_claim_task_plan(database: &std::path::Path, workspace: &str, task_id: &str) -> String {
    let conn = Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open store to explain");
    let mut statement = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {CLAIM_ROWS_FOR_TASK_SQL}"))
        .expect("prepare plan");
    let rows = statement
        .query_map(rusqlite::params![workspace, task_id], |row| {
            row.get::<_, String>(3)
        })
        .expect("query plan");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("plan rows")
        .join("\n")
}
