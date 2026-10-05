//! Invocation trace, linked task-id and tool-call inserts.

use std::sync::LazyLock;

use rusqlite::params;

use orbit_common::OrbitError;

use crate::contracts::InvocationInsertParams;
use crate::{Store, now_string};

/// Every column the invocation-trace insert binds, in bind order.
///
/// [ORB-10367] This list is the single source of truth for the INSERT
/// statement below, so the column list and bind arity cannot drift. A column
/// added here needs a matching schema migration; the migration ledger test
/// `v1_upgrade_and_fresh_database_have_identical_columns` keeps an upgraded
/// legacy database's columns identical to a fresh one's.
pub(crate) const INVOCATION_INSERT_COLUMNS: &[&str] = &[
    "ts",
    "workspace_id",
    "job_run_id",
    "activity_id",
    "agent",
    "model",
    "duration_ms",
    "input_tokens",
    "cache_read_tokens",
    "cache_create_tokens",
    "cache_create_1h_tokens",
    "output_tokens",
    "tool_call_count",
    "provider_cost_usd",
];

/// `INSERT INTO invocations(...) VALUES (?1, ...)` rendered from
/// [`INVOCATION_INSERT_COLUMNS`] so the column list and the bind arity can
/// never drift from each other.
static INVOCATION_INSERT_SQL: LazyLock<String> = LazyLock::new(|| {
    let columns = INVOCATION_INSERT_COLUMNS.join(", ");
    let placeholders = (1..=INVOCATION_INSERT_COLUMNS.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO invocations({columns}) VALUES ({placeholders})")
});

impl Store {
    pub fn insert_invocation_trace_record(
        &self,
        workspace_id: &str,
        params: &InvocationInsertParams,
    ) -> Result<(), OrbitError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction()
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        tx.execute(
            INVOCATION_INSERT_SQL.as_str(),
            params![
                now_string(),
                workspace_id,
                params.job_run_id,
                params.activity_id,
                params.agent,
                params.model,
                params.trace.duration_ms as i64,
                params.trace.usage.input as i64,
                params.trace.usage.cache_read as i64,
                params.trace.usage.cache_create as i64,
                params.trace.usage.cache_create_1h as i64,
                params.trace.usage.output as i64,
                params.trace.tool_calls.len() as i64,
                params.trace.provider_cost_usd,
            ],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;

        let invocation_id = tx.last_insert_rowid();
        insert_invocation_task_ids(&tx, invocation_id, &params.task_ids)?;
        insert_tool_calls(&tx, invocation_id, &params.trace.tool_calls)?;

        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }
}

fn insert_invocation_task_ids(
    tx: &rusqlite::Transaction<'_>,
    invocation_id: i64,
    task_ids: &[String],
) -> Result<(), OrbitError> {
    for task_id in task_ids {
        tx.execute(
            "INSERT OR IGNORE INTO invocation_tasks(invocation_id, task_id) VALUES (?1, ?2)",
            params![invocation_id, task_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    }
    Ok(())
}

fn insert_tool_calls(
    tx: &rusqlite::Transaction<'_>,
    invocation_id: i64,
    tool_calls: &[orbit_types::telemetry::ToolCallTrace],
) -> Result<(), OrbitError> {
    for tool_call in tool_calls {
        tx.execute(
            r#"INSERT INTO tool_calls(invocation_id, seq, tool_name, result_bytes)
               VALUES (?1, ?2, ?3, ?4)"#,
            params![
                invocation_id,
                tool_call.seq as i64,
                tool_call.tool_name,
                tool_call.result_bytes as i64,
            ],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    }
    Ok(())
}
