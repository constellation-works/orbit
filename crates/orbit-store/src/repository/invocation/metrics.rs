//! Invocation metric SQL reads: model-attributed samples, per-task totals, and
//! per-tool call aggregates.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, params};

use crate::contract::{InvocationSample, TaskInvocationMetrics, ToolInvocationMetrics};

/// Every invocation with a resolved model in one workspace partition, in
/// storage order. Callers group into buckets and compute quantiles.
pub(super) fn list_model_attributed_samples(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Vec<InvocationSample>, OrbitError> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT activity_id, agent, model, input_tokens, cache_read_tokens,
                   cache_create_tokens, output_tokens, tool_call_count
            FROM invocations
            WHERE workspace_id = ?1 AND model IS NOT NULL AND TRIM(model) <> ''
            "#,
        )
        .map_err(sqlite_store_error)?;

    let rows = stmt
        .query_map(params![workspace_id], |row| {
            Ok(InvocationSample {
                activity_id: row.get(0)?,
                agent: row.get(1)?,
                model: row.get(2)?,
                input_tokens: row.get::<_, i64>(3)? as u64,
                cache_read_tokens: row.get::<_, i64>(4)? as u64,
                cache_create_tokens: row.get::<_, i64>(5)? as u64,
                output_tokens: row.get::<_, i64>(6)? as u64,
                tool_call_count: row.get::<_, i64>(7)? as u64,
            })
        })
        .map_err(sqlite_store_error)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

/// Aggregates one task's invocations in SQL rather than reading the whole
/// invocation join.
pub(super) fn task_metrics(
    conn: &Connection,
    task_id: &str,
) -> Result<TaskInvocationMetrics, OrbitError> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT it.task_id, COUNT(*),
                   COALESCE(SUM(i.input_tokens), 0), COALESCE(SUM(i.cache_read_tokens), 0),
                   COALESCE(SUM(i.cache_create_tokens), 0), COALESCE(SUM(i.output_tokens), 0),
                   COALESCE(SUM(i.input_tokens + i.output_tokens), 0),
                   COALESCE(SUM(i.tool_call_count), 0)
            FROM invocation_tasks it
            INNER JOIN invocations i ON i.id = it.invocation_id
            WHERE it.task_id = ?1
            GROUP BY it.task_id
            "#,
        )
        .map_err(sqlite_store_error)?;
    let mut rows = stmt
        .query_map(params![task_id], |row| {
            Ok(TaskInvocationMetrics {
                task_id: row.get(0)?,
                invocation_count: row.get::<_, i64>(1)? as u64,
                total_input_tokens: row.get::<_, i64>(2)? as u64,
                total_cache_read_tokens: row.get::<_, i64>(3)? as u64,
                total_cache_create_tokens: row.get::<_, i64>(4)? as u64,
                total_output_tokens: row.get::<_, i64>(5)? as u64,
                total_tokens: row.get::<_, i64>(6)? as u64,
                total_tool_calls: row.get::<_, i64>(7)? as u64,
            })
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)?;
    Ok(rows.pop().unwrap_or_else(|| empty_task_metrics(task_id)))
}

/// Tool aggregates for one workspace partition, all time.
pub(super) fn tool_metrics(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Vec<ToolInvocationMetrics>, OrbitError> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT i.activity_id, tc.tool_name, COUNT(*) AS call_count,
                   COALESCE(AVG(tc.result_bytes), 0), COALESCE(SUM(tc.result_bytes), 0)
            FROM tool_calls tc
            INNER JOIN invocations i ON i.id = tc.invocation_id
            WHERE i.workspace_id = ?1
            GROUP BY i.activity_id, tc.tool_name
            ORDER BY call_count DESC, i.activity_id ASC, tc.tool_name ASC
            "#,
        )
        .map_err(sqlite_store_error)?;

    let rows = stmt
        .query_map(params![workspace_id], |row| {
            Ok(ToolInvocationMetrics {
                activity_id: row.get(0)?,
                tool_name: row.get(1)?,
                call_count: row.get::<_, i64>(2)? as u64,
                avg_result_bytes: row.get(3)?,
                total_result_bytes: row.get::<_, i64>(4)? as u64,
            })
        })
        .map_err(sqlite_store_error)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

fn empty_task_metrics(task_id: &str) -> TaskInvocationMetrics {
    TaskInvocationMetrics {
        task_id: task_id.to_string(),
        invocation_count: 0,
        total_input_tokens: 0,
        total_cache_read_tokens: 0,
        total_cache_create_tokens: 0,
        total_output_tokens: 0,
        total_tokens: 0,
        total_tool_calls: 0,
    }
}
