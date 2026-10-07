// Admitted unit test: combinatorial SQL generation through SQLite's planner.
// Uses the same query builders as the store methods; no copied query oracle.
use super::*;
use rusqlite::types::Value;

#[test]
fn tool_call_queries_search_the_invocation_index_for_every_window_and_limit() {
    let store = Store::open_in_memory().unwrap();
    let conn = store.read().unwrap();
    conn.execute_batch("INSERT INTO audit_events
        (execution_id, timestamp, command, subcommand, tool_name, role, status,
         exit_code, duration_ms, working_directory, pid) VALUES
        ('old', '2026-01-01T00:00:00+00:00', 'tool', 'run', 'orbit.task.show', 'codex', 'success', 0, 1, '.', 1),
        ('new', '2026-10-01T00:00:00+00:00', 'tool', 'run-mcp', 'orbit.task.show', 'codex', 'failure', 1, 1, '.', 1),
        ('denied', '2026-10-01T00:00:00+00:00', 'tool', 'run', 'orbit.task.show', 'codex', 'denied', 1, 1, '.', 1),
        ('other', '2026-10-01T00:00:00+00:00', 'authorization', 'run', 'orbit.task.show', 'codex', 'denied', 1, 1, '.', 1);").unwrap();
    for windowed in [false, true] {
        let mut queries = vec![
            tool_call_counts_by_role_sql(windowed).to_string(),
            tool_call_counts_by_surface_and_role_sql(windowed),
        ];
        for limited in [false, true] {
            queries.push(top_tool_calls_sql(windowed, limited));
        }
        for sql in queries {
            let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let mut parameters = Vec::new();
            if windowed {
                parameters.push(Value::Text("2026-09-01T00:00:00+00:00".into()));
            }
            if stmt.parameter_count() > parameters.len() {
                parameters.push(Value::Integer(50));
            }
            let plan: Vec<String> = stmt
                .query_map(rusqlite::params_from_iter(parameters), |row| row.get(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                plan.iter().any(|line| line.contains(
                    "SEARCH audit_events USING INDEX idx_audit_events_command_subcommand_timestamp"
                ) && (!windowed || line.contains("timestamp>?"))),
                "tool aggregates must search the invocation population and timestamp range: {plan:?}"
            );
        }
    }
    drop(conn);
    let cutoff = "2026-09-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    for (since, total) in [(None, 3), (Some(&cutoff), 2)] {
        let counts = store.get_audit_tool_call_counts_by_role(since).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!((counts[0].total, counts[0].failed), (total, 2));
        let surface = store
            .get_audit_tool_call_counts_by_surface_and_role(since)
            .unwrap();
        assert_eq!(
            (
                surface[0].surface.as_str(),
                surface[0].total,
                surface[0].failed
            ),
            ("task", total, 2)
        );
        assert_eq!(
            store.get_audit_top_tool_calls(since, 50).unwrap()[0].total,
            total
        );
    }
}
