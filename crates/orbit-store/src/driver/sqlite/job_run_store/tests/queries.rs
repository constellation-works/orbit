// Admitted unit test: SQL generation through SQLite's planner. Uses the same
// builder as the store's list method; no copied query oracle.
use super::*;
use rusqlite::types::Value;

/// The recency page is what the dashboard's aggregate run list asks of every
/// workspace. Without an index over the recency expression SQLite read and
/// sorted every run of the workspace to return the first page [ORB-14595].
#[test]
fn recency_pages_walk_an_index_instead_of_sorting_the_workspace() {
    let store = Store::open_in_memory().unwrap();
    let conn = store.read().unwrap();
    for state in [None, Some(JobRunState::Failed)] {
        let query = JobRunQuery {
            state,
            limit: Some(50),
            order_by: JobRunOrder::Recency,
            ..JobRunQuery::default()
        };
        let (sql, params) = job_run_list_sql("ws_a", &query);
        let values = params
            .iter()
            .map(|param| match param.to_sql().unwrap() {
                rusqlite::types::ToSqlOutput::Owned(value) => value,
                rusqlite::types::ToSqlOutput::Borrowed(value) => value.into(),
                other => panic!("unexpected parameter {other:?}"),
            })
            .collect::<Vec<Value>>();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(values), |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let index = if state.is_some() {
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
