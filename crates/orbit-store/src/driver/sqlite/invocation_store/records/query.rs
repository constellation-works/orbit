//! Invocation list and accounting queries, and the list filter builder.

use chrono::{DateTime, Duration, Utc};
use rusqlite::types::ToSql;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use super::hydrate::{
    hydrate_invocation_records, map_invocation_accounting_fact, map_invocation_record,
};
use crate::Store;
use crate::contracts::{
    InvocationAccountingFact, InvocationAccountingQuery, InvocationQuery, InvocationRecord,
    ProviderLedgerEntry,
};

impl Store {
    /// Latest invocation model for each requested run, scoped to the workspace
    /// and activity. One indexed batch; no steps, traces or tool calls loaded.
    pub fn latest_activity_models(
        &self,
        workspace_id: &str,
        activity_id: &str,
        run_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, OrbitError> {
        let mut models = std::collections::HashMap::new();
        if run_ids.is_empty() {
            return Ok(models);
        }
        let ids =
            serde_json::to_string(run_ids).map_err(|error| OrbitError::Store(error.to_string()))?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT job_run_id, model FROM invocations
             WHERE workspace_id = ?1 AND activity_id = ?2
               AND job_run_id IN (SELECT value FROM json_each(?3))
             ORDER BY ts DESC, id DESC",
            )
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(rusqlite::params![workspace_id, activity_id, ids], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(sqlite_store_error)?;
        for row in rows {
            let (run, model) = row.map_err(sqlite_store_error)?;
            models.entry(run).or_insert(model);
        }
        Ok(models)
    }

    pub fn list_invocation_records(
        &self,
        filter: &InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        let conn = self.read()?;
        let (sql, params) = build_invocation_list_query(filter);
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|value| value.as_ref()).collect();

        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), map_invocation_record)
            .map_err(sqlite_store_error)?;

        let mut records = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        drop(stmt);
        drop(conn);

        if records.is_empty() {
            return Ok(records);
        }

        hydrate_invocation_records(self, &mut records)?;
        Ok(records)
    }

    /// Every invocation of the provider since `since`, oldest first, on any
    /// workspace of this host [ORB-14699].
    ///
    /// A row's provider is its `provider` column, else its `agent` for a row
    /// recorded without one. The timestamps are text of varying fractional
    /// width, so the query reads a second early and the cut is made on the
    /// parsed time.
    pub fn list_provider_ledger_entries(
        &self,
        provider_names: &[String],
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderLedgerEntry>, OrbitError> {
        if provider_names.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read()?;
        let mut filters = InvocationListQuery::default();
        filters.push_filter("i.ts >= ?", (since - Duration::seconds(1)).to_rfc3339());
        let names = provider_names
            .iter()
            .map(|name| {
                filters.push_value(name.to_ascii_lowercase());
                format!("?{}", filters.len())
            })
            .collect::<Vec<_>>()
            .join(", ");
        filters
            .conditions
            .push(format!("COALESCE(i.provider, LOWER(i.agent)) IN ({names})"));
        let sql = format!(
            "SELECT i.ts, i.input_tokens, i.output_tokens, i.provider_cost_usd \
             FROM invocations i {} ORDER BY i.ts ASC, i.id ASC",
            filters.where_clause()
        );
        let param_refs = filters
            .params
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let ts_raw: String = row.get(0)?;
                Ok(ProviderLedgerEntry {
                    ts: crate::parse_timestamp(&ts_raw)?,
                    tokens: (row.get::<_, i64>(1)? as u64)
                        .saturating_add(row.get::<_, i64>(2)? as u64),
                    cost_usd: row.get(3)?,
                })
            })
            .map_err(sqlite_store_error)?;
        let mut entries = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        entries.retain(|entry| entry.ts >= since);
        Ok(entries)
    }

    /// Loads every invocation in the requested workspace and half-open window exactly once.
    ///
    /// This intentionally bypasses the detailed-list limit and hydrates only
    /// distinct linked task ids, never tool-call rows.
    pub fn list_invocation_accounting_facts(
        &self,
        query: &InvocationAccountingQuery,
    ) -> Result<Vec<InvocationAccountingFact>, OrbitError> {
        let conn = self.read()?;
        let mut filters = InvocationListQuery::default();
        if let Some(workspace_id) = &query.workspace_id {
            filters.push_filter("i.workspace_id = ?", workspace_id.clone());
        }
        if let Some(since) = query.since {
            filters.push_filter("i.ts >= ?", since.to_rfc3339());
        }
        filters.push_filter("i.ts < ?", query.until.to_rfc3339());
        let sql = format!(
            "SELECT i.id, i.ts, i.model, i.input_tokens, i.cache_read_tokens, \
                    i.cache_create_tokens, i.cache_create_1h_tokens, i.output_tokens, \
                    i.provider_cost_usd \
             FROM invocations i {} ORDER BY i.ts ASC, i.id ASC",
            filters.where_clause()
        );
        let param_refs = filters
            .params
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), map_invocation_accounting_fact)
            .map_err(sqlite_store_error)?;
        let mut facts = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
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

    if filter.succeeded_candidate_only {
        // Outcomes are workspace-qualified. EXISTS preserves one row per
        // invocation even when an activity appears more than once in a job.
        // A later successful retry must not relabel an earlier attempt.
        query.conditions.push(
            "EXISTS (SELECT 1 FROM job_runs jr
             WHERE jr.workspace_id = i.workspace_id AND jr.run_id = i.job_run_id
               AND jr.state = 'success'
               AND jr.commit_decision = 'performed'
               AND jr.commit_committed = 1)
             AND EXISTS (SELECT 1 FROM job_run_steps s
             WHERE s.workspace_id = i.workspace_id AND s.run_id = i.job_run_id
               AND s.target_type = 'activity' AND s.target_id = i.activity_id AND s.state = 'success'
               AND julianday(i.ts) BETWEEN julianday(s.started_at) AND julianday(s.finished_at)
               AND NOT EXISTS (SELECT 1 FROM invocations later
                 WHERE later.workspace_id = i.workspace_id AND later.job_run_id = i.job_run_id
                   AND later.activity_id = i.activity_id
                   AND julianday(later.ts) BETWEEN julianday(s.started_at) AND julianday(s.finished_at)
                   AND (julianday(later.ts), later.id) > (julianday(i.ts), i.id)))"
                .to_string(),
        );
    }

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
