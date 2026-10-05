//! Invocation and accounting row mapping, and grouped task-id/tool-call hydration.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::types::ToSql;

use orbit_common::OrbitError;
use orbit_common::model::pricing::derive_cost_usd;
use orbit_types::telemetry::TokenUsage;

use crate::Store;
use crate::contracts::{InvocationAccountingFact, InvocationRecord, InvocationToolCallRecord};

impl Store {
    pub(super) fn load_invocation_task_ids(
        &self,
        invocation_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<String>>, OrbitError> {
        let conn = self.connection_handle()?;
        load_grouped_strings(
            &conn,
            invocation_ids,
            "SELECT invocation_id, task_id FROM invocation_tasks WHERE invocation_id IN ({placeholders}) ORDER BY invocation_id ASC, task_id ASC",
        )
    }

    fn load_invocation_tool_calls(
        &self,
        invocation_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<InvocationToolCallRecord>>, OrbitError> {
        if invocation_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = self.connection_handle()?;
        let mut grouped: HashMap<i64, Vec<InvocationToolCallRecord>> = HashMap::new();
        for chunk in invocation_ids.chunks(SQL_IN_LIST_CHUNK) {
            let placeholders = sql_placeholders(chunk.len());
            let sql = format!(
                "SELECT invocation_id, seq, tool_name, result_bytes FROM tool_calls WHERE invocation_id IN ({placeholders}) ORDER BY invocation_id ASC, seq ASC"
            );
            let params: Vec<Box<dyn ToSql>> = chunk
                .iter()
                .map(|id| Box::new(*id) as Box<dyn ToSql>)
                .collect();
            let param_refs: Vec<&dyn ToSql> = params.iter().map(|value| value.as_ref()).collect();

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok(InvocationToolCallRecord {
                        invocation_id: row.get(0)?,
                        seq: row.get::<_, i64>(1)? as u64,
                        tool_name: row.get(2)?,
                        result_bytes: row.get::<_, i64>(3)? as u64,
                    })
                })
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            for row in rows {
                let call = row.map_err(|e| OrbitError::Store(e.to_string()))?;
                grouped.entry(call.invocation_id).or_default().push(call);
            }
        }
        Ok(grouped)
    }

    fn connection_handle(
        &self,
    ) -> Result<crate::driver::sqlite::read_pool::ReadGuard<'_>, OrbitError> {
        self.read()
    }
}

/// Ids per `IN (...)` list. SQLite caps bound parameters per statement
/// (`SQLITE_MAX_VARIABLE_NUMBER`, 32766 on the bundled build), and the
/// accounting path hydrates every invocation in a window in one go, so an
/// unchunked list fails outright once the window holds more rows than that.
const SQL_IN_LIST_CHUNK: usize = 500;

pub(super) fn map_invocation_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvocationRecord> {
    let ts_raw: String = row.get(1)?;
    let model: Option<String> = row.get(5)?;
    let ts = parse_rfc3339_timestamp(&ts_raw)?;
    let input_tokens = row.get::<_, i64>(7)? as u64;
    let cache_read_tokens = row.get::<_, i64>(8)? as u64;
    let cache_create_tokens = row.get::<_, i64>(9)? as u64;
    let cache_create_1h_tokens = row.get::<_, i64>(10)? as u64;
    let output_tokens = row.get::<_, i64>(11)? as u64;
    let provider_cost_usd: Option<f64> = row.get(13)?;

    let derived_cost_usd = model.as_deref().and_then(|model| {
        derive_cost_usd(
            model,
            ts,
            &TokenUsage {
                input: input_tokens,
                cache_read: cache_read_tokens,
                cache_create: cache_create_tokens,
                cache_create_1h: cache_create_1h_tokens,
                output: output_tokens,
            },
        )
    });

    Ok(InvocationRecord {
        id: row.get(0)?,
        ts,
        job_run_id: row.get(2)?,
        activity_id: row.get(3)?,
        agent: row.get(4)?,
        model,
        duration_ms: row.get::<_, i64>(6)? as u64,
        input_tokens,
        cache_read_tokens,
        cache_create_tokens,
        cache_create_1h_tokens,
        output_tokens,
        total_tokens: input_tokens.saturating_add(output_tokens),
        tool_call_count: row.get::<_, i64>(12)? as u64,
        task_ids: Vec::new(),
        tool_calls: Vec::new(),
        provider_cost_usd,
        derived_cost_usd,
    })
}

pub(super) fn map_invocation_accounting_fact(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<InvocationAccountingFact> {
    let ts_raw: String = row.get(1)?;
    let ts = parse_rfc3339_timestamp(&ts_raw)?;
    let model: Option<String> = row.get(2)?;
    let input_tokens = row.get::<_, i64>(3)? as u64;
    let cache_read_tokens = row.get::<_, i64>(4)? as u64;
    let cache_create_tokens = row.get::<_, i64>(5)? as u64;
    let cache_create_1h_tokens = row.get::<_, i64>(6)? as u64;
    let output_tokens = row.get::<_, i64>(7)? as u64;
    let provider_cost_usd = row.get(8)?;
    let derived_cost_usd = model.as_deref().and_then(|model| {
        derive_cost_usd(
            model,
            ts,
            &TokenUsage {
                input: input_tokens,
                cache_read: cache_read_tokens,
                cache_create: cache_create_tokens,
                cache_create_1h: cache_create_1h_tokens,
                output: output_tokens,
            },
        )
    });

    Ok(InvocationAccountingFact {
        id: row.get(0)?,
        ts,
        model,
        input_tokens,
        cache_read_tokens,
        cache_create_tokens,
        cache_create_1h_tokens,
        output_tokens,
        task_ids: Vec::new(),
        provider_cost_usd,
        derived_cost_usd,
    })
}

pub(super) fn hydrate_invocation_records(
    store: &Store,
    records: &mut [InvocationRecord],
) -> Result<(), OrbitError> {
    let invocation_ids = records.iter().map(|record| record.id).collect::<Vec<_>>();
    let task_ids = store.load_invocation_task_ids(&invocation_ids)?;
    let tool_calls = store.load_invocation_tool_calls(&invocation_ids)?;
    let index_by_id = records
        .iter()
        .enumerate()
        .map(|(index, record)| (record.id, index))
        .collect::<HashMap<_, _>>();

    for (invocation_id, values) in task_ids {
        if let Some(index) = index_by_id.get(&invocation_id).copied() {
            records[index].task_ids = values;
        }
    }
    for (invocation_id, values) in tool_calls {
        if let Some(index) = index_by_id.get(&invocation_id).copied() {
            records[index].tool_calls = values;
        }
    }

    Ok(())
}

fn parse_rfc3339_timestamp(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                raw.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn load_grouped_strings(
    conn: &rusqlite::Connection,
    invocation_ids: &[i64],
    sql_template: &str,
) -> Result<HashMap<i64, Vec<String>>, OrbitError> {
    if invocation_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut grouped: HashMap<i64, Vec<String>> = HashMap::new();
    for chunk in invocation_ids.chunks(SQL_IN_LIST_CHUNK) {
        let placeholders = sql_placeholders(chunk.len());
        let sql = sql_template.replace("{placeholders}", &placeholders);
        let params: Vec<Box<dyn ToSql>> = chunk
            .iter()
            .map(|id| Box::new(*id) as Box<dyn ToSql>)
            .collect();
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|value| value.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        for row in rows {
            let (invocation_id, value) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
            grouped.entry(invocation_id).or_default().push(value);
        }
    }
    Ok(grouped)
}

fn sql_placeholders(count: usize) -> String {
    (0..count)
        .map(|index| format!("?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ")
}
