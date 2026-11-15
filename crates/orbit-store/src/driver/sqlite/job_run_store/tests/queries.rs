// Admitted unit test: SQL generation through SQLite's planner. Uses the same
// builder as the store's list method; no copied query oracle.
use super::super::queries::job_run_list_sql;
use crate::contracts::{JobRunOrder, JobRunQuery};
use crate::{Store, parse_timestamp};
use orbit_types::workflow::JobRunState;
use rusqlite::types::Value;

/// The recency page is what the dashboard's aggregate run list asks of every
/// workspace. Without an index over the recency expression SQLite read and
/// sorted every run of the workspace to return the first page [ORB-14595].
#[test]
fn recency_pages_walk_an_index_instead_of_sorting_the_workspace() {
    let store = Store::open_in_memory().unwrap();
    let conn = store.read().unwrap();
    // A `created_since` window on a single-state recency page (the failed-run
    // page behind the auth-failure scan) must still walk the state-recency
    // index. Without planner statistics, which the store never collects,
    // SQLite instead ranges over the created-at window and sorts that whole
    // retained cohort to answer a bounded page [ORB-20163].
    let windowed = Some(parse_timestamp("2026-10-23T08:50:22+00:00").unwrap());
    for (state, states, created_since) in [
        (None, Vec::new(), None),
        (Some(JobRunState::Failed), Vec::new(), None),
        (Some(JobRunState::Failed), Vec::new(), windowed),
        (None, vec![JobRunState::Failed], windowed),
    ] {
        let query = JobRunQuery {
            state,
            states,
            created_since,
            limit: Some(50),
            order_by: JobRunOrder::Recency,
            ..JobRunQuery::default()
        };
        let (sql, params) = job_run_list_sql("ws_a", &query);
        let values = params
            .iter()
            .map(|param| match param.to_sql().unwrap() {
                rusqlite::types::ToSqlOutput::Owned(value) => value,
                rusqlite::types::ToSqlOutput::Borrowed(value) => Value::try_from(value).unwrap(),
                other => panic!("unexpected parameter {other:?}"),
            })
            .collect::<Vec<Value>>();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(values), |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let index = if query.state.is_some() || !query.states.is_empty() {
            "idx_job_runs_ws_state_recency"
        } else {
            "idx_job_runs_ws_recency"
        };
        assert!(
            plan.iter()
                .any(|line| line.starts_with("SEARCH job_runs USING INDEX") && line.contains(index)),
            "a recency page must search {index}: {plan:?}"
        );
        assert!(
            !plan.iter().any(|line| line.contains("TEMP B-TREE")),
            "a recency page must not sort the workspace: {plan:?}"
        );
    }
}

/// An unbounded live-run scan (the dashboard's active triage/recovery
/// bindings, read on every task detail) must seek the live states. Left to
/// the planner, `ORDER BY created_at` walked the workspace's whole history
/// through the created-at index and discarded every finished run: ~18 ms per
/// state on a 41k-run workspace, three states per task detail [ORB-21825].
#[test]
fn live_run_scans_seek_the_state_index_instead_of_walking_history() {
    let store = Store::open_in_memory().unwrap();
    let conn = store.read().unwrap();
    for query in [
        JobRunQuery {
            state: Some(JobRunState::Running),
            include_steps: false,
            ..JobRunQuery::default()
        },
        JobRunQuery {
            states: vec![JobRunState::Pending, JobRunState::Retrying],
            ..JobRunQuery::default()
        },
        JobRunQuery {
            active_only: true,
            job_id: Some("task_pilot_pipeline".into()),
            ..JobRunQuery::default()
        },
    ] {
        let plan = query_plan(&conn, &query);
        assert!(
            plan.iter().any(|line| {
                line.starts_with("SEARCH job_runs USING INDEX idx_job_runs_ws_state (")
            }),
            "a live-run scan must seek idx_job_runs_ws_state: {query:?} {plan:?}"
        );
    }
    // A finished state can be most of the history, where the state index
    // only adds a sort; its plan stays the planner's.
    let plan = query_plan(
        &conn,
        &JobRunQuery {
            state: Some(JobRunState::Success),
            ..JobRunQuery::default()
        },
    );
    assert!(
        !plan
            .iter()
            .any(|line| line.contains("idx_job_runs_ws_state ")),
        "a finished-state scan is not pinned to the state index: {plan:?}"
    );
}

fn query_plan(conn: &rusqlite::Connection, query: &JobRunQuery) -> Vec<String> {
    let (sql, params) = job_run_list_sql("ws_a", query);
    let values = params
        .iter()
        .map(|param| match param.to_sql().unwrap() {
            rusqlite::types::ToSqlOutput::Owned(value) => value,
            rusqlite::types::ToSqlOutput::Borrowed(value) => Value::try_from(value).unwrap(),
            other => panic!("unexpected parameter {other:?}"),
        })
        .collect::<Vec<Value>>();
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    stmt.query_map(rusqlite::params_from_iter(values), |row| row.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
