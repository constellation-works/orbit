use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::model::pricing::normalize_token_usage;
use orbit_store::contracts::{
    ActivityInvocationMetrics, InvocationAccountingFact, InvocationAccountingQuery,
    InvocationInsertParams, InvocationQuery, InvocationRecord, InvocationSample,
    ProviderLedgerEntry, TaskInvocationMetrics, TaskListFilter, ToolInvocationMetrics,
};
use orbit_types::telemetry::{ProviderLimitObservation, TokenUsage};
use orbit_types::workflow::ProviderCrewOutage;
use serde::{Deserialize, Serialize};

use crate::RuntimeKernel;
use crate::cost::{
    IMPLEMENT_INVOCATION_IDS, InvocationSpend, NormalizedTokenSummary, OrchestrationModelSummary,
    TaskTokenSpend, TaskTokenTotals,
};

pub use crate::cost::{
    OrchestrationBucketKind as OrchestratorMetricsBucketKind,
    OrchestrationBucketSummary as OrchestratorInvocationMetricsBucket,
};

/// Aggregate completed invocations without consulting mutable crew configuration.
/// Missing dispatch identity is a separate historical `unknown` population.
pub fn agent_effort_usage<'a>(
    facts: impl IntoIterator<Item = &'a InvocationAccountingFact>,
) -> Vec<orbit_types::telemetry::AgentEffortUsage> {
    let mut accumulator = AgentEffortUsageAccumulator::default();
    for fact in facts {
        record_agent_effort_usage(&mut accumulator, fact);
    }
    finish_agent_effort_usage(accumulator)
}

type AgentEffortKey = (Option<String>, Option<String>, Option<String>);
type AgentEffortPopulation = (orbit_types::telemetry::AgentEffortUsage, BTreeSet<String>);

/// Bounded by distinct crew/model/effort identities and linked task ids, without
/// retaining another copy of the full invocation population.
#[derive(Debug, Default)]
pub struct AgentEffortUsageAccumulator {
    rows: BTreeMap<AgentEffortKey, AgentEffortPopulation>,
}

/// Add exactly one completed invocation to its recorded population.
pub fn record_agent_effort_usage(
    accumulator: &mut AgentEffortUsageAccumulator,
    fact: &InvocationAccountingFact,
) {
    use orbit_types::telemetry::AgentEffortUsage;
    let crew = fact
        .execution
        .as_ref()
        .and_then(|execution| execution.crew.clone());
    let effort = fact
        .execution
        .as_ref()
        .map(|execution| execution.reasoning_effort.map(|effort| effort.to_string()))
        .unwrap_or(Some("unknown".to_string()));
    let (row, tasks) = accumulator
        .rows
        .entry((crew.clone(), fact.model.clone(), effort.clone()))
        .or_insert_with(|| {
            (
                AgentEffortUsage {
                    crew,
                    model: fact.model.clone(),
                    reasoning_effort: effort,
                    invocations: 0,
                    tasks: 0,
                    duration_ms: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_cost_usd: 0.0,
                    provider_cost_count: 0,
                },
                BTreeSet::new(),
            )
        });
    row.invocations += 1;
    row.duration_ms = row.duration_ms.saturating_add(fact.duration_ms);
    row.input_tokens = row.input_tokens.saturating_add(fact.input_tokens);
    row.output_tokens = row.output_tokens.saturating_add(fact.output_tokens);
    if let Some(cost) = fact
        .provider_cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
    {
        row.provider_cost_usd += cost;
        row.provider_cost_count += 1;
    }
    tasks.extend(fact.task_ids.iter().cloned());
}

/// Stable identity order, with distinct task support for each population.
pub fn finish_agent_effort_usage(
    accumulator: AgentEffortUsageAccumulator,
) -> Vec<orbit_types::telemetry::AgentEffortUsage> {
    accumulator
        .rows
        .into_values()
        .map(|(mut row, tasks)| {
            row.tasks = tasks.len() as u64;
            row
        })
        .collect()
}

/// Stable, managed-execution-only accounting snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestratorInvocationMetrics {
    pub as_of: DateTime<Utc>,
    pub since: Option<DateTime<Utc>>,
    /// Effective exclusive upper cutoff (never later than `as_of`).
    pub until: DateTime<Utc>,
    pub buckets: Vec<OrchestratorInvocationMetricsBucket>,
    pub normalized_tokens: NormalizedTokenSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BucketKey {
    kind: OrchestratorMetricsBucketKind,
    orchestrator: Option<String>,
}

#[derive(Debug, Default)]
struct BucketAccumulator {
    effort_usage: AgentEffortUsageAccumulator,
    linked_task_ids: BTreeSet<String>,
    invocation_count: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_create_tokens: u64,
    cache_create_1h_tokens: u64,
    output_tokens: u64,
    provider_cost_usd: f64,
    provider_cost_count: u64,
    derived_cost_usd: f64,
    derived_cost_count: u64,
    comparable_provider_cost_usd: f64,
    comparable_derived_cost_usd: f64,
    comparable_cost_count: u64,
    missing_provider_count: u64,
    unpriced_derived_count: u64,
    normalized_tokens: TokenAccumulator,
    models: BTreeMap<String, TokenAccumulator>,
}

#[derive(Debug, Default)]
struct TokenAccumulator {
    linked_task_ids: BTreeSet<String>,
    uncovered_task_ids: BTreeSet<String>,
    summary: NormalizedTokenSummary,
}

impl TokenAccumulator {
    fn add(&mut self, fact: &InvocationAccountingFact) {
        self.summary.invocation_count = self.summary.invocation_count.saturating_add(1);
        self.linked_task_ids.extend(fact.task_ids.iter().cloned());
        let Some(model) = fact.model.as_deref() else {
            self.uncovered_task_ids
                .extend(fact.task_ids.iter().cloned());
            self.summary.unknown_input_basis_or_model_count = self
                .summary
                .unknown_input_basis_or_model_count
                .saturating_add(1);
            return;
        };
        let usage = TokenUsage {
            input: fact.input_tokens,
            cache_read: fact.cache_read_tokens,
            cache_create: fact.cache_create_tokens,
            cache_create_1h: fact.cache_create_1h_tokens,
            output: fact.output_tokens,
        };
        let Some(usage) = normalize_token_usage(model, fact.ts, &usage) else {
            self.uncovered_task_ids
                .extend(fact.task_ids.iter().cloned());
            self.summary.unknown_input_basis_or_model_count = self
                .summary
                .unknown_input_basis_or_model_count
                .saturating_add(1);
            return;
        };
        self.add_usage(&usage, !fact.task_ids.is_empty());
    }

    /// Count one covered invocation. `linked` says it names a task, so its
    /// tokens also join the per-linked-task numerator.
    fn add_usage(&mut self, usage: &TokenUsage, linked: bool) {
        self.summary.covered_invocation_count =
            self.summary.covered_invocation_count.saturating_add(1);
        self.summary.uncached_input_tokens = self
            .summary
            .uncached_input_tokens
            .saturating_add(usage.input);
        self.summary.cache_read_tokens = self
            .summary
            .cache_read_tokens
            .saturating_add(usage.cache_read);
        self.summary.cache_create_tokens = self
            .summary
            .cache_create_tokens
            .saturating_add(usage.cache_create);
        self.summary.cache_create_1h_tokens = self
            .summary
            .cache_create_1h_tokens
            .saturating_add(usage.cache_create_1h);
        self.summary.output_tokens = self.summary.output_tokens.saturating_add(usage.output);
        let total = usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_create)
            .saturating_add(usage.cache_create_1h)
            .saturating_add(usage.output);
        self.summary.normalized_token_total =
            self.summary.normalized_token_total.saturating_add(total);
        if linked {
            self.summary.linked_normalized_token_total = self
                .summary
                .linked_normalized_token_total
                .saturating_add(total);
        }
    }

    fn finish(&self) -> NormalizedTokenSummary {
        let mut summary = self.summary.clone();
        summary.linked_task_count = self.linked_task_ids.len() as u64;
        summary.covered_linked_task_count = self
            .linked_task_ids
            .difference(&self.uncovered_task_ids)
            .count() as u64;
        summary
    }
}

impl BucketAccumulator {
    fn add(&mut self, fact: &InvocationAccountingFact) {
        record_agent_effort_usage(&mut self.effort_usage, fact);
        self.invocation_count = self.invocation_count.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(fact.input_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(fact.cache_read_tokens);
        self.cache_create_tokens = self
            .cache_create_tokens
            .saturating_add(fact.cache_create_tokens);
        self.cache_create_1h_tokens = self
            .cache_create_1h_tokens
            .saturating_add(fact.cache_create_1h_tokens);
        self.output_tokens = self.output_tokens.saturating_add(fact.output_tokens);
        self.linked_task_ids.extend(fact.task_ids.iter().cloned());
        self.normalized_tokens.add(fact);
        if let Some(model) = fact.model.as_deref() {
            let usage = TokenUsage {
                input: fact.input_tokens,
                cache_read: fact.cache_read_tokens,
                cache_create: fact.cache_create_tokens,
                cache_create_1h: fact.cache_create_1h_tokens,
                output: fact.output_tokens,
            };
            if let Some(usage) = normalize_token_usage(model, fact.ts, &usage) {
                let model_tokens = self.models.entry(model.to_string()).or_default();
                model_tokens.summary.invocation_count =
                    model_tokens.summary.invocation_count.saturating_add(1);
                model_tokens
                    .linked_task_ids
                    .extend(fact.task_ids.iter().cloned());
                model_tokens.add_usage(&usage, !fact.task_ids.is_empty());
            }
        }

        let provider = fact.provider_cost_usd.filter(|cost| valid_cost(*cost));
        let derived = fact.derived_cost_usd.filter(|cost| valid_cost(*cost));
        match provider {
            Some(cost) => {
                self.provider_cost_usd += cost;
                self.provider_cost_count = self.provider_cost_count.saturating_add(1);
            }
            None => self.missing_provider_count = self.missing_provider_count.saturating_add(1),
        }
        match derived {
            Some(cost) => {
                self.derived_cost_usd += cost;
                self.derived_cost_count = self.derived_cost_count.saturating_add(1);
            }
            None => self.unpriced_derived_count = self.unpriced_derived_count.saturating_add(1),
        }
        if let (Some(provider), Some(derived)) = (provider, derived) {
            self.comparable_provider_cost_usd += provider;
            self.comparable_derived_cost_usd += derived;
            self.comparable_cost_count = self.comparable_cost_count.saturating_add(1);
        }
    }

    fn finish(
        self,
        kind: OrchestratorMetricsBucketKind,
        orchestrator: Option<String>,
    ) -> OrchestratorInvocationMetricsBucket {
        let normalized_tokens = self.normalized_tokens.finish();
        let models = self
            .models
            .into_iter()
            .map(|(model, tokens)| OrchestrationModelSummary {
                model,
                tokens: tokens.finish(),
            })
            .collect();
        OrchestratorInvocationMetricsBucket {
            effort_breakdown: finish_agent_effort_usage(self.effort_usage),
            kind,
            orchestrator,
            invocation_count: self.invocation_count,
            linked_task_count: self.linked_task_ids.len() as u64,
            input_tokens: self.input_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_create_tokens: self.cache_create_tokens,
            cache_create_1h_tokens: self.cache_create_1h_tokens,
            output_tokens: self.output_tokens,
            provider_cost_usd: self.provider_cost_usd,
            provider_cost_count: self.provider_cost_count,
            derived_cost_usd: self.derived_cost_usd,
            derived_cost_count: self.derived_cost_count,
            comparable_provider_cost_usd: self.comparable_provider_cost_usd,
            comparable_derived_cost_usd: self.comparable_derived_cost_usd,
            comparable_cost_count: self.comparable_cost_count,
            comparable_cost_delta_usd: self.comparable_provider_cost_usd
                - self.comparable_derived_cost_usd,
            missing_provider_count: self.missing_provider_count,
            unpriced_derived_count: self.unpriced_derived_count,
            normalized_tokens,
            models,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ActivityMetricBucket {
    invocation_count: u64,
    total_input_tokens: u64,
    total_cache_read_tokens: u64,
    total_cache_create_tokens: u64,
    total_output_tokens: u64,
    total_tool_calls: u64,
    totals: Vec<u64>,
}

impl ActivityMetricBucket {
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

fn average_u64(values: &[u64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<u64>() as f64 / values.len() as f64
    }
}

fn percentile_u64(sorted_values: &[u64], percentile: u64) -> u64 {
    if sorted_values.is_empty() {
        return 0;
    }
    let n = sorted_values.len();
    let rank = (percentile as usize * n).div_ceil(100);
    let index = rank.saturating_sub(1).min(n - 1);
    sorted_values[index]
}

impl RuntimeKernel {
    /// Activity aggregates for this runtime's workspace partition, all time.
    /// The partition is the job-run store's persisted identity, the same one
    /// invocation writes use, not the logical workspace binding.
    pub fn activity_invocation_metrics(
        &self,
    ) -> Result<Vec<ActivityInvocationMetrics>, OrbitError> {
        let samples = self
            .stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list_model_attributed_samples(self.stores().jobs().workspace_id())?;
        let mut grouped: BTreeMap<(String, String, Option<String>), ActivityMetricBucket> =
            BTreeMap::new();
        for sample in samples {
            let key = (
                sample.activity_id.clone(),
                sample.agent.clone(),
                sample.model.clone(),
            );
            grouped.entry(key).or_default().add(&sample);
        }
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
                    avg_tokens: average_u64(&totals),
                    p50_tokens: percentile_u64(&totals, 50),
                    p95_tokens: percentile_u64(&totals, 95),
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

    pub fn task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<TaskInvocationMetrics, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .task_metrics(task_id)
    }

    /// Tool aggregates for this runtime's workspace partition, all time.
    /// The partition is the job-run store's persisted identity, the same one
    /// invocation writes use, not the logical workspace binding.
    pub fn tool_invocation_metrics(&self) -> Result<Vec<ToolInvocationMetrics>, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .tool_metrics(self.stores().jobs().workspace_id())
    }

    /// Lists invocation records. A run id only names a run within one
    /// workspace, so a `job_run_id` filter without a workspace is scoped to
    /// this runtime's.
    pub fn invocation_records(
        &self,
        mut filter: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        if filter.job_run_id.is_some() && filter.workspace_id.is_none() {
            filter.workspace_id = Some(self.stores().jobs().workspace_id().to_string());
        }
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list(&filter)
    }

    /// [`Self::invocation_records`] limited to the partition this runtime
    /// records invocations under, whatever `filter.workspace_id` names: every
    /// workspace on the host shares the invocation store.
    pub fn workspace_invocation_records(
        &self,
        mut filter: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        filter.workspace_id = Some(self.stores().jobs().workspace_id().to_string());
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list(&filter)
    }

    /// Records an invocation under this runtime's workspace, whatever
    /// workspace the caller-supplied params claim to come from.
    pub fn insert_invocation_trace_record(
        &self,
        params: &InvocationInsertParams,
    ) -> Result<(), OrbitError> {
        let workspace_id = self.stores().jobs().workspace_id();
        self.stored()
            .stores()
            .host
            .sqlite
            .write(|tx| tx.invocation().insert(workspace_id, params))
    }

    /// [ORB-14695] Records a provider usage limit in this host's store,
    /// unless a newer observation for its provider account, scope and window
    /// stands. Returns whether it was stored.
    pub fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<bool, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .write(|tx| tx.provider_limit().put(observation))
    }

    /// [ORB-25967] Records a provider's complete usage reading and retires
    /// the event observations an older reading stored that it no longer
    /// reports.
    pub fn record_provider_usage_reading(
        &self,
        observations: &[ProviderLimitObservation],
    ) -> Result<(), OrbitError> {
        if observations.is_empty() {
            return Ok(());
        }
        self.stored()
            .stores()
            .host
            .sqlite
            .write(|tx| tx.provider_limit().put_reading(observations))
    }

    /// [ORB-14695] The latest provider usage limit per provider account,
    /// scope and window this host observed, newest first.
    pub fn provider_limits(&self) -> Result<Vec<ProviderLimitObservation>, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .provider_limit()
            .list()
    }

    /// [ORB-16379] Records a crew this host's provider was unavailable for,
    /// unless a stored outage of the crew lasts at least as long. Returns
    /// whether it was stored.
    pub fn record_provider_crew_outage(
        &self,
        outage: &ProviderCrewOutage,
    ) -> Result<bool, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .write(|tx| tx.provider_limit().put_outage(outage))
    }

    /// [ORB-16379] This host's crew outages lasting past `now`.
    pub fn provider_crew_outages(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProviderCrewOutage>, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .provider_limit()
            .list_outages(now)
    }

    /// [ORB-14699] The provider account's invocations since `since`, oldest
    /// first, across this host; `provider_names` are the names the provider
    /// goes by, and `account` the crew account, `None` for the default login
    /// [ORB-17288].
    pub fn provider_ledger_entries(
        &self,
        provider_names: &[String],
        account: Option<&str>,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderLedgerEntry>, OrbitError> {
        self.stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list_provider_ledger_entries(provider_names, account, since)
    }

    /// Normalized token spend per linked task over this workspace's managed
    /// invocations recorded at or after `since` (all of them for `None`). An
    /// invocation linked to several tasks counts in full toward each.
    pub fn task_token_totals(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<BTreeMap<String, TaskTokenTotals>, OrbitError> {
        let facts = self
            .stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list_accounting_facts(&InvocationAccountingQuery {
                workspace_id: Some(self.stores().jobs().workspace_id().to_string()),
                since,
                until: Utc::now(),
            })?;
        let mut totals = BTreeMap::<String, TaskTokenTotals>::new();
        for fact in &facts {
            let normalized = normalized_token_total(fact);
            let implement = fact
                .activity_id
                .as_deref()
                .is_some_and(|activity| IMPLEMENT_INVOCATION_IDS.contains(&activity));
            for task_id in &fact.task_ids {
                let task = totals.entry(task_id.clone()).or_default();
                task.all.add(normalized);
                if implement {
                    task.implement.add(normalized);
                    // Facts are oldest first, so the last write is the latest.
                    task.implement_model.clone_from(&fact.model);
                }
            }
        }
        Ok(totals)
    }

    /// This workspace's managed invocations recorded at or after `since`
    /// (all of them for `None`), oldest first, each with its provider cost,
    /// table-derived cost and normalized token total.
    pub fn invocation_spend(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<InvocationSpend>, OrbitError> {
        let facts = self
            .stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list_accounting_facts(&InvocationAccountingQuery {
                workspace_id: Some(self.stores().jobs().workspace_id().to_string()),
                since,
                until: Utc::now(),
            })?;
        Ok(facts
            .iter()
            .map(|fact| InvocationSpend {
                job_run_id: fact.job_run_id.clone(),
                activity_id: fact.activity_id.clone(),
                ts: fact.ts,
                task_ids: fact.task_ids.clone(),
                provider_cost_usd: fact.provider_cost_usd.filter(|cost| valid_cost(*cost)),
                derived_cost_usd: fact.derived_cost_usd.filter(|cost| valid_cost(*cost)),
                normalized_tokens: normalized_token_total(fact),
                duration_ms: fact.duration_ms,
            })
            .collect())
    }

    /// Aggregates this workspace's managed invocation telemetry by canonical task orchestrator.
    ///
    /// The effective window is half-open (`since <= ts < until`). Missing
    /// tasks take precedence over unattributed tasks, which take precedence
    /// over named/shared attribution, so incomplete ownership never charges a
    /// known orchestrator.
    pub fn orchestrator_invocation_metrics(
        &self,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<OrchestratorInvocationMetrics, OrbitError> {
        let as_of = Utc::now();
        if let (Some(since), Some(until)) = (since, until)
            && since >= until
        {
            return Err(OrbitError::InvalidInput(
                "since must be earlier than until".to_string(),
            ));
        }
        let effective_until = until.map_or(as_of, |until| until.min(as_of));
        if since.is_some_and(|since| since >= effective_until) {
            return Err(OrbitError::InvalidInput(
                "since must be earlier than the effective upper cutoff".to_string(),
            ));
        }

        let facts = self
            .stored()
            .stores()
            .host
            .sqlite
            .read()?
            .invocation()
            .list_accounting_facts(&InvocationAccountingQuery {
                workspace_id: Some(self.stores().jobs().workspace_id().to_string()),
                since,
                until: effective_until,
            })?;
        let task_orchestrators = self
            .task_candidates(&TaskListFilter::default(), usize::MAX)?
            .items
            .into_iter()
            .map(|task| (task.id, task.orchestrator))
            .collect::<HashMap<_, _>>();

        let mut grouped = BTreeMap::<BucketKey, BucketAccumulator>::new();
        let mut normalized_tokens = TokenAccumulator::default();
        for fact in &facts {
            let key = classify_invocation(&fact.task_ids, &task_orchestrators);
            grouped.entry(key).or_default().add(fact);
            normalized_tokens.add(fact);
        }
        let buckets = grouped
            .into_iter()
            .map(|(key, bucket)| bucket.finish(key.kind, key.orchestrator))
            .collect();

        Ok(OrchestratorInvocationMetrics {
            as_of,
            since,
            until: effective_until,
            buckets,
            normalized_tokens: normalized_tokens.finish(),
        })
    }
}

fn classify_invocation(
    task_ids: &[String],
    task_orchestrators: &HashMap<String, Option<String>>,
) -> BucketKey {
    if task_ids.is_empty() {
        return BucketKey {
            kind: OrchestratorMetricsBucketKind::Unattributed,
            orchestrator: None,
        };
    }

    let distinct_task_ids = task_ids.iter().collect::<BTreeSet<_>>();
    if distinct_task_ids
        .iter()
        .any(|task_id| !task_orchestrators.contains_key(task_id.as_str()))
    {
        return BucketKey {
            kind: OrchestratorMetricsBucketKind::Missing,
            orchestrator: None,
        };
    }

    let owners = distinct_task_ids
        .iter()
        .filter_map(|task_id| task_orchestrators.get(task_id.as_str()))
        .collect::<Vec<_>>();
    if owners.iter().any(|owner| owner.is_none()) {
        return BucketKey {
            kind: OrchestratorMetricsBucketKind::Unattributed,
            orchestrator: None,
        };
    }
    let named = owners
        .into_iter()
        .filter_map(|owner| owner.as_ref())
        .collect::<BTreeSet<_>>();
    if named.len() == 1 {
        BucketKey {
            kind: OrchestratorMetricsBucketKind::Orchestrator,
            orchestrator: named.into_iter().next().cloned(),
        }
    } else {
        BucketKey {
            kind: OrchestratorMetricsBucketKind::Shared,
            orchestrator: None,
        }
    }
}

fn valid_cost(cost: f64) -> bool {
    cost.is_finite() && cost >= 0.0
}

/// The fact's normalized token total, or `None` when its model is missing
/// or not covered by the normalization table.
fn normalized_token_total(fact: &InvocationAccountingFact) -> Option<u64> {
    let usage = TokenUsage {
        input: fact.input_tokens,
        cache_read: fact.cache_read_tokens,
        cache_create: fact.cache_create_tokens,
        cache_create_1h: fact.cache_create_1h_tokens,
        output: fact.output_tokens,
    };
    let usage = normalize_token_usage(fact.model.as_deref()?, fact.ts, &usage)?;
    Some(
        usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_create)
            .saturating_add(usage.cache_create_1h)
            .saturating_add(usage.output),
    )
}

impl TaskTokenSpend {
    fn add(&mut self, normalized: Option<u64>) {
        self.invocation_count = self.invocation_count.saturating_add(1);
        match normalized {
            Some(tokens) => {
                self.normalized_token_total = self.normalized_token_total.saturating_add(tokens);
            }
            None => {
                self.uncovered_invocation_count = self.uncovered_invocation_count.saturating_add(1);
            }
        }
    }
}
