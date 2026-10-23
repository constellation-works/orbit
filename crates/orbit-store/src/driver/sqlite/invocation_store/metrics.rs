use std::collections::BTreeMap;

use rusqlite::params;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::Store;

use crate::contracts::{ActivityInvocationMetrics, TaskInvocationMetrics, ToolInvocationMetrics};

#[derive(Debug, Clone)]
struct InvocationSample {
    activity_id: String,
    agent: String,
    model: Option<String>,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_create_tokens: u64,
    output_tokens: u64,
    tool_call_count: u64,
}

#[derive(Debug, Clone, Default)]
struct MetricBucket {
    invocation_count: u64,
    total_input_tokens: u64,
    total_cache_read_tokens: u64,
    total_cache_create_tokens: u64,
    total_output_tokens: u64,
    total_tool_calls: u64,
    totals: Vec<u64>,
}

impl MetricBucket {
    fn add(&mut self, sample: &InvocationSample) {
        self.invocation_count += 1;
        self.total_input_tokens += sample.input_tokens;
        self.total_cache_read_tokens += sample.cache_read_tokens;
        self.total_cache_create_tokens += sample.cache_create_tokens;
        self.total_output_tokens += sample.output_tokens;
        self.total_tool_calls += sample.tool_call_count;
        self.totals
            .push(sample.input_tokens.saturating_add(sample.output_tokens));
    }
}

impl Store {
    pub fn list_activity_invocation_metrics(
        &self,
    ) -> Result<Vec<ActivityInvocationMetrics>, OrbitError> {
        let samples = self.load_model_attributed_invocation_samples()?;
        let grouped = self.group_invocation_metrics(samples, |sample| {
            (
                sample.activity_id.clone(),
                sample.agent.clone(),
                sample.model.clone(),
            )
        });
        let mut rows = grouped
            .into_iter()
            .map(|((activity_id, agent, model), bucket)| {
                let mut totals = bucket.totals;
                totals.sort_unstable();
                ActivityInvocationMetrics {
                    activity_id,
                    agent,
                    model,
                    invocation_count: bucket.invocation_count,
                    total_input_tokens: bucket.total_input_tokens,
                    total_cache_read_tokens: bucket.total_cache_read_tokens,
                    total_cache_create_tokens: bucket.total_cache_create_tokens,
                    total_output_tokens: bucket.total_output_tokens,
                    total_tokens: bucket
                        .total_input_tokens
                        .saturating_add(bucket.total_output_tokens),
                    avg_tokens: average(&totals),
                    p50_tokens: percentile(&totals, 50),
                    p95_tokens: percentile(&totals, 95),
                    total_tool_calls: bucket.total_tool_calls,
                }
            })
            .collect::<Vec<_>>();

        rows.sort_by(|left, right| {
            right
                .total_tokens
                .cmp(&left.total_tokens)
                .then_with(|| left.activity_id.cmp(&right.activity_id))
                .then_with(|| left.agent.cmp(&right.agent))
                .then_with(|| left.model.cmp(&right.model))
        });
        Ok(rows)
    }

    pub fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<TaskInvocationMetrics, OrbitError> {
        let mut rows = self.list_task_invocation_metrics(task_id)?;
        Ok(rows.pop().unwrap_or_else(|| empty_task_metrics(task_id)))
    }

    pub fn list_tool_invocation_metrics(&self) -> Result<Vec<ToolInvocationMetrics>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT i.activity_id, tc.tool_name, COUNT(*) AS call_count,
                       COALESCE(AVG(tc.result_bytes), 0), COALESCE(SUM(tc.result_bytes), 0)
                FROM tool_calls tc
                INNER JOIN invocations i ON i.id = tc.invocation_id
                GROUP BY i.activity_id, tc.tool_name
                ORDER BY call_count DESC, i.activity_id ASC, tc.tool_name ASC
                "#,
            )
            .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map([], |row| {
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

    /// Every invocation with a resolved model, in storage order. The callers
    /// group into ordered maps and sort each bucket before taking
    /// percentiles, so an `ORDER BY` here would only add a full-table sort;
    /// the model filter runs in SQL so unattributed rows are never
    /// materialized.
    fn load_model_attributed_invocation_samples(
        &self,
    ) -> Result<Vec<InvocationSample>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT activity_id, agent, model, input_tokens, cache_read_tokens,
                       cache_create_tokens, output_tokens, tool_call_count
                FROM invocations
                WHERE model IS NOT NULL AND TRIM(model) <> ''
                "#,
            )
            .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map([], |row| {
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

    fn group_invocation_metrics<K, F>(
        &self,
        samples: Vec<InvocationSample>,
        key_fn: F,
    ) -> BTreeMap<K, MetricBucket>
    where
        K: Ord,
        F: Fn(&InvocationSample) -> K,
    {
        let mut grouped: BTreeMap<K, MetricBucket> = BTreeMap::new();
        for sample in samples {
            let key = key_fn(&sample);
            grouped.entry(key).or_default().add(&sample);
        }
        grouped
    }

    /// Aggregates one task's invocations in SQL rather than reading the
    /// whole invocation join.
    fn list_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<Vec<TaskInvocationMetrics>, OrbitError> {
        let conn = self.read()?;
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
        let rows = stmt
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
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
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

fn average(values: &[u64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<u64>() as f64 / values.len() as f64
    }
}

fn percentile(sorted_values: &[u64], percentile: u64) -> u64 {
    if sorted_values.is_empty() {
        return 0;
    }
    let n = sorted_values.len();
    let rank = (percentile as usize * n).div_ceil(100);
    let index = rank.saturating_sub(1).min(n - 1);
    sorted_values[index]
}
