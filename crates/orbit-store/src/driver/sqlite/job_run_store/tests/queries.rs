// Admitted unit test: SQL generation through SQLite's planner. Uses the same
// builder as the store's list method; no copied query oracle.
use super::*;
use rusqlite::types::Value;

/// A per-task listing reads the materialized ownership table. Parsing every
/// run's input instead made each call walk the workspace's whole history,
/// twice per enrolled task in an experiment readout [ORB-20412].
#[test]
fn per_task_listing_reads_ownership_instead_of_run_inputs() {
    let store = Store::open_in_memory().unwrap();
    let conn = store.read().unwrap();
    let query = JobRunQuery {
        task_id: Some("T-1".into()),
        ..JobRunQuery::default()
    };
    let (sql, _) = job_run_list_sql("ws_a", &query);
    let plan: Vec<String> = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(rusqlite::params!["ws_a", "T-1"], |row| row.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.starts_with("SEARCH job_run_tasks")),
        "a per-task listing must search job_run_tasks: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("VIRTUAL TABLE")),
        "a per-task listing must not walk run inputs with json_each: {plan:?}"
    );
}

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
