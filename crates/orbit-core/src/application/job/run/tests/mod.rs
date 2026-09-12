//! Shared test helpers and child module declarations for run command tests.

use crate::OrbitRuntime;

mod actions;
mod admissions_stop;
mod cancellation_race;
mod conflict;
mod owner;
mod projection;
mod reconcile;
mod reconcile_provider;
mod worker_limit;

use chrono::{DateTime, Utc};
use orbit_store::{TaskReservationReserveParams, V2AuditEventInsertParams};
use orbit_types::workflow::{JobRun, JobRunState};
use rusqlite::{Connection, params};
use tempfile::tempdir;

pub(crate) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

pub(crate) fn insert_pending_run(runtime: &OrbitRuntime, job_id: &str) -> JobRun {
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            job_id,
            1,
            Utc::now() - chrono::Duration::seconds(5),
            None,
            None,
        )
        .expect("insert run")
}

/// Rewrites a run's `created_at` so tests can age a run past reconcile grace
/// windows without sleeping [ORB-10070].
pub(crate) fn backdate_run_created_at(
    runtime: &OrbitRuntime,
    run: &JobRun,
    created_at: DateTime<Utc>,
) {
    let conn = Connection::open(runtime.global_root().join("orbit.db")).expect("open orbit db");
    conn.execute(
        "UPDATE job_runs SET created_at = ?3 \
         WHERE workspace_id = ?1 AND run_id = ?2",
        params![
            runtime.workspace_id().expect("workspace id"),
            run.run_id,
            created_at.to_rfc3339(),
        ],
    )
    .expect("backdate run created_at");
}

pub(crate) fn strip_run_timing(runtime: &OrbitRuntime, run: &JobRun) {
    let conn = Connection::open(runtime.global_root().join("orbit.db")).expect("open orbit db");
    conn.execute(
        "UPDATE job_runs SET finished_at = NULL, duration_ms = NULL \
         WHERE workspace_id = ?1 AND run_id = ?2",
        params![runtime.workspace_id().expect("workspace id"), run.run_id],
    )
    .expect("strip run timing");
}

pub(crate) fn set_run_pid_start_time(runtime: &OrbitRuntime, run: &JobRun, token: &str) {
    let conn = Connection::open(runtime.global_root().join("orbit.db")).expect("open orbit db");
    conn.execute(
        "UPDATE job_runs SET pid_start_time = ?3 \
         WHERE workspace_id = ?1 AND run_id = ?2",
        params![
            runtime.workspace_id().expect("workspace id"),
            run.run_id,
            token,
        ],
    )
    .expect("set pid_start_time");
}

/// Overwrite the recorded owner identity without going through start/claim
/// guards, so race fixtures can change ownership after a stale snapshot.
pub(crate) fn set_run_owner(
    runtime: &OrbitRuntime,
    run: &JobRun,
    pid: u32,
    pid_start_time: Option<&str>,
) {
    let conn = Connection::open(runtime.global_root().join("orbit.db")).expect("open orbit db");
    conn.execute(
        "UPDATE job_runs SET pid = ?3, pid_start_time = ?4 \
         WHERE workspace_id = ?1 AND run_id = ?2",
        params![
            runtime.workspace_id().expect("workspace id"),
            run.run_id,
            pid,
            pid_start_time,
        ],
    )
    .expect("set run owner");
}

pub(crate) fn reserve_for_run(runtime: &OrbitRuntime, owner_run_id: &str, file: &str) {
    runtime
        .stores()
        .task_reservations()
        .reserve_task_reservation(TaskReservationReserveParams {
            workspace_orbit_dir: runtime.paths().orbit_dir.to_string_lossy().into_owned(),
            workspace_id: Some(runtime.workspace_id().expect("workspace id")),
            task_ids: Vec::new(),
            requested_files: vec![file.to_string()],
            actor: "test".to_string(),
            ttl_seconds: 3_600,
            owner_run_id: Some(owner_run_id.to_string()),
            owner_metadata_json: None,
        })
        .expect("reserve for run");
}

pub(crate) fn active_reservation_owners(runtime: &OrbitRuntime) -> Vec<String> {
    let workspace_orbit_dir = runtime.paths().orbit_dir.to_string_lossy().into_owned();
    let workspace_id = runtime.workspace_id().expect("workspace id");
    runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(&workspace_orbit_dir, Some(&workspace_id))
        .expect("list active reservations")
        .reservations
        .into_iter()
        .filter_map(|reservation| reservation.owner_run_id)
        .collect()
}

pub(crate) fn write_run_finished_audit(
    runtime: &OrbitRuntime,
    run_id: &str,
    finished_at: DateTime<Utc>,
) {
    let event = serde_json::json!({
        "schemaVersion": 1,
        "event_type": "run.finished",
        "event_id": format!("evt-{run_id}-finished"),
        "ts": finished_at.to_rfc3339(),
        "run_id": run_id,
        "agent_identity": "system",
        "body_kind": "run_finished",
        "outcome": "success",
        "error_message": null,
    });
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().expect("workspace id"),
            event_id: event["event_id"].as_str().expect("event id").to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "run.finished".to_string(),
            ts: finished_at,
            run_id: run_id.to_string(),
            agent_identity: "system".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: event.to_string(),
        })
        .expect("insert run finished audit");
}
