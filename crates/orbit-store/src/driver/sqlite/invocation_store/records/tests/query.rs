// Admitted unit test: SQL generation through SQLite's planner. Uses the same
// builder as the store's list method; no copied query oracle.
use rusqlite::types::{ToSql, Value};

use super::super::query::build_invocation_list_query;
use crate::Store;
use crate::contracts::InvocationQuery;

/// Succeeded-candidate selection behind the implement_one durations trend.
/// Without planner statistics, which the store never collects, SQLite probed
/// later invocations through `idx_invocations_reliability_window` with only
/// the workspace bound, walking the whole workspace once per candidate: a
/// 2–5 s read on a review-scale store [ORB-21827].
#[test]
fn succeeded_candidates_probe_later_invocations_by_run() {
    let store = Store::open_in_memory().unwrap();
    let conn = store.read().unwrap();
    let query = InvocationQuery {
        succeeded_candidate_only: true,
        since: Some(crate::parse_timestamp("2026-10-04T08:50:22+00:00").unwrap()),
        workspace_id: Some("ws_a".to_string()),
        activity_id: Some("implement_one".to_string()),
        limit: 100_000,
        skip_tool_calls: true,
        ..InvocationQuery::default()
    };
    let (sql, params) = build_invocation_list_query(&query);
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
    assert!(
        plan.iter()
            .any(|line| line.starts_with("SEARCH later USING INDEX")
                && line.contains(
                    "idx_invocations_workspace_job_run_id (workspace_id=? AND job_run_id=?)"
                )),
        "the later-invocation probe must be bounded to the candidate's run: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("SCAN later")
            || line.contains("later USING COVERING INDEX idx_invocations_reliability_window")),
        "the later-invocation probe must not walk the workspace: {plan:?}"
    );
}
