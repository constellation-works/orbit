//! Invocation list and accounting queries, and the list filter builder.

use rusqlite::types::ToSql;

use orbit_common::OrbitError;

use super::hydrate::{
    hydrate_invocation_records, map_invocation_accounting_fact, map_invocation_record,
};
use crate::Store;
use crate::contracts::{
    InvocationAccountingFact, InvocationAccountingQuery, InvocationQuery, InvocationRecord,
};

impl Store {
    /// Cheap insert-only watermark for the token scoreboard skip path.
    pub fn invocation_scoreboard_watermark(&self) -> Result<Option<u64>, OrbitError> {
        let conn = self.read()?;
        let max_id: Option<i64> = conn
            .query_row("SELECT MAX(id) FROM invocations", [], |row| row.get(0))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(Some(max_id.unwrap_or(0) as u64))
    }

    pub fn list_invocation_records(
        &self,
        filter: &InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        let conn = self.read()?;
        let (sql, params) = build_invocation_list_query(filter);
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|value| value.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), map_invocation_record)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut records = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        drop(stmt);
        drop(conn);

        if records.is_empty() {
            return Ok(records);
        }

        hydrate_invocation_records(self, &mut records)?;
        Ok(records)
    }

    /// Loads every invocation in the requested half-open window exactly once.
    ///
    /// This intentionally bypasses the detailed-list limit and hydrates only
    /// distinct linked task ids, never tool-call rows.
    pub fn list_invocation_accounting_facts(
        &self,
        query: &InvocationAccountingQuery,
    ) -> Result<Vec<InvocationAccountingFact>, OrbitError> {
        let conn = self.read()?;
        let (sql, params): (&str, Vec<Box<dyn ToSql>>) = match query.since {
            Some(since) => (
                r#"SELECT i.id, i.ts, i.model, i.input_tokens, i.cache_read_tokens,
                          i.cache_create_tokens, i.cache_create_1h_tokens, i.output_tokens,
                          i.provider_cost_usd
                   FROM invocations i
                   WHERE i.ts >= ?1 AND i.ts < ?2
                   ORDER BY i.ts ASC, i.id ASC"#,
                vec![
                    Box::new(since.to_rfc3339()),
                    Box::new(query.until.to_rfc3339()),
                ],
            ),
            None => (
                r#"SELECT i.id, i.ts, i.model, i.input_tokens, i.cache_read_tokens,
                          i.cache_create_tokens, i.cache_create_1h_tokens, i.output_tokens,
                          i.provider_cost_usd
                   FROM invocations i
                   WHERE i.ts < ?1
                   ORDER BY i.ts ASC, i.id ASC"#,
                vec![Box::new(query.until.to_rfc3339())],
            ),
        };
        let param_refs = params
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<_>>();
        let mut stmt = conn
            .prepare(sql)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), map_invocation_accounting_fact)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut facts = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        drop(stmt);
        drop(conn);

        if facts.is_empty() {
            return Ok(facts);
        }
        let invocation_ids = facts.iter().map(|fact| fact.id).collect::<Vec<_>>();
        let mut task_ids = self.load_invocation_task_ids(&invocation_ids)?;
        for fact in &mut facts {
            fact.task_ids = task_ids.remove(&fact.id).unwrap_or_default();
        }
        Ok(facts)
    }
}

fn build_invocation_list_query(filter: &InvocationQuery) -> (String, Vec<Box<dyn ToSql>>) {
    let mut query = InvocationListQuery::default();

    if let Some(since) = &filter.since {
        query.push_filter("i.ts >= ?", since.to_rfc3339());
    }
    if let Some(until) = &filter.until {
        query.push_filter("i.ts <= ?", until.to_rfc3339());
    }
    if let Some(workspace_id) = &filter.workspace_id {
        query.push_filter("i.workspace_id = ?", workspace_id.clone());
    }
    if let Some(job_run_id) = &filter.job_run_id {
        query.push_filter("i.job_run_id = ?", job_run_id.clone());
    }
    if let Some(activity_id) = &filter.activity_id {
        query.push_filter("i.activity_id = ?", activity_id.clone());
    }
    if let Some(task_id) = &filter.task_id {
        query.push_filter(
            "EXISTS (SELECT 1 FROM invocation_tasks it WHERE it.invocation_id = i.id AND it.task_id = ?)",
            task_id.clone(),
        );
    }
    if let Some(agent) = &filter.agent {
        query.push_filter("i.agent = ?", agent.clone());
    }
    if let Some(model) = &filter.model {
        query.push_filter("i.model = ?", model.clone());
    }
    if let Some(tool_name) = &filter.tool_name {
        query.push_filter(
            "EXISTS (SELECT 1 FROM tool_calls tc WHERE tc.invocation_id = i.id AND tc.tool_name = ?)",
            tool_name.clone(),
        );
    }

    let limit = if filter.limit == 0 { 100 } else { filter.limit };
    query.push_value(limit as i64);

    let sql = format!(
        "SELECT i.id, i.ts, i.job_run_id, i.activity_id, i.agent, i.model, i.duration_ms, \
         i.input_tokens, i.cache_read_tokens, i.cache_create_tokens, i.cache_create_1h_tokens, \
         i.output_tokens, i.tool_call_count, i.provider_cost_usd \
         FROM invocations i {} ORDER BY i.ts DESC, i.id DESC LIMIT ?{}",
        query.where_clause(),
        query.len()
    );

    (sql, query.params)
}

#[derive(Default)]
struct InvocationListQuery {
    conditions: Vec<String>,
    params: Vec<Box<dyn ToSql>>,
}

impl InvocationListQuery {
    fn push_filter<T>(&mut self, sql: &str, value: T)
    where
        T: ToSql + 'static,
    {
        self.push_value(value);
        // L-0024: nested EXISTS filters need the bind index inserted at the placeholder.
        let placeholder = format!("?{}", self.len());
        self.conditions.push(sql.replacen('?', &placeholder, 1));
    }

    fn push_value<T>(&mut self, value: T)
    where
        T: ToSql + 'static,
    {
        self.params.push(Box::new(value));
    }

    fn len(&self) -> usize {
        self.params.len()
    }

    fn where_clause(&self) -> String {
        if self.conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.conditions.join(" AND "))
        }
    }
}
