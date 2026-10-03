use chrono::Utc;
use tempfile::TempDir;

use super::super::SqliteJobRunStore;
use super::super::backend::KEYED_RUN_WINDOW_SQL;
use super::super::queries::{job_run_list_sql, latest_job_runs_sql};
use crate::Store;
use crate::contracts::{JobRunQuery, JobRunStoreBackend};

#[test]
fn legacy_role_model_row_loads_as_flat_crew_model() {
    let temp = TempDir::new().expect("tempdir");
    let db_path = temp.path().join("orbit.db");
    drop(Store::open(&db_path).expect("create current schema"));

    let conn = rusqlite::Connection::open(&db_path).expect("open raw db");
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO job_runs(
                run_id, workspace_id, job_id, attempt, state, scheduled_at, created_at,
                resolved_crew, implementer_model
             ) VALUES (?1, ?2, ?3, 1, 'success', ?4, ?4, ?5, ?6)",
        rusqlite::params![
            "legacy-run",
            "ws_a",
            "legacy-job",
            now,
            "legacy-crew",
            "legacy-implementer-model"
        ],
    )
    .expect("insert legacy-shaped row");
    drop(conn);

    let loaded = SqliteJobRunStore::new(
        Store::open(&db_path).expect("reopen migrated store"),
        "ws_a",
    )
    .get_job_run("legacy-run")
    .expect("read legacy run")
    .expect("legacy run exists");

    assert_eq!(loaded.resolved_crew.as_deref(), Some("legacy-crew"));
    assert_eq!(
        loaded.crew_model.as_deref(),
        Some("legacy-implementer-model")
    );
}

/// The `EXPLAIN QUERY PLAN` detail lines for `sql` on a fresh store, so the
/// planner's choice over the schema the migrations really build is what the
/// assertions read.
fn plan_of(sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> String {
    let store = Store::open_in_memory().expect("open store");
    store
        .with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .map_err(|e| orbit_common::OrbitError::Store(e.to_string()))?;
            let rows = stmt
                .query_map(params, |row| row.get::<_, String>(3))
                .map_err(|e| orbit_common::OrbitError::Store(e.to_string()))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| orbit_common::OrbitError::Store(e.to_string()))
        })
        .expect("query plan")
        .join("\n")
}

/// A per-job listing, newest first, is served in index order: no run the job
/// ever had is sorted to find the newest few. These are the list read, the
/// catalog's last run per job, and the drain status's latest terminal run.
#[test]
fn per_job_newest_first_listings_use_the_job_created_index() {
    let queries = [
        JobRunQuery {
            job_id: Some("job".into()),
            ..JobRunQuery::default()
        },
        JobRunQuery {
            job_id: Some("job".into()),
            limit: Some(1),
            ..JobRunQuery::default()
        },
        JobRunQuery {
            job_id: Some("job".into()),
            terminal_only: true,
            limit: Some(1),
            include_steps: false,
            ..JobRunQuery::default()
        },
    ];
    for query in queries {
        let (sql, params) = job_run_list_sql("ws", &query);
        let refs = params.iter().map(|p| p.as_ref()).collect::<Vec<_>>();
        let plan = plan_of(&sql, &refs);
        assert!(
            plan.contains("idx_job_runs_ws_job_created"),
            "{query:?}\n{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{query:?}\n{plan}");
    }
}

#[test]
fn latest_job_runs_probes_each_job_index_without_scanning_history() {
    let plan = plan_of(&latest_job_runs_sql(2), &[&"ws", &"job-a", &"job-b"]);
    assert!(plan.contains("idx_job_runs_ws_job_created"), "{plan}");
    assert!(!plan.contains("SCAN job_runs"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

/// The keyed-submission window reads one job's newest runs on every keyed
/// admission; it must not sort the job's whole history to take them.
#[test]
fn keyed_run_window_uses_the_job_created_index() {
    let plan = plan_of(KEYED_RUN_WINDOW_SQL, &[&"ws", &"job", &50_i64]);
    assert!(plan.contains("idx_job_runs_ws_job_created"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

/// The retry-lineage walks find a run's children by `retry_source_run_id`. A
/// store that never ran automation still has to answer them from an index
/// rather than rescanning the workspace for every run in the lineage, and the
/// children read must not sort what it finds.
#[test]
fn retry_lineage_walks_use_an_index_without_automation() {
    let children = plan_of(
        "SELECT run_id FROM job_runs WHERE workspace_id=?1 AND retry_source_run_id=?2 \
         ORDER BY created_at,run_id LIMIT ?3",
        &[&"ws", &"run", &10_i64],
    );
    assert!(
        children.contains("idx_job_runs_ws_retry_created"),
        "{children}"
    );
    assert!(!children.contains("TEMP B-TREE"), "{children}");
    let lineage = plan_of(
        "SELECT j.run_id FROM job_runs j JOIN (SELECT ?2 AS run_id) l \
         ON j.retry_source_run_id = l.run_id WHERE j.workspace_id = ?1",
        &[&"ws", &"run"],
    );
    assert!(
        lineage.contains("idx_job_runs_ws_retry_created"),
        "{lineage}"
    );
}
