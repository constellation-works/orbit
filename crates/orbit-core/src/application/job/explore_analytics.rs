//! Does delegating read-only exploration pay off? [ORB-16814]
//!
//! For each implementing crew, compares the implement runs whose agent
//! delegated at least one explorer with those that delegated none: the
//! implement step's duration, the implementing crew's own cost, and how
//! often the run widened its task's declared footprint (a footprint miss).
//! Each cohort carries its sample count, so a difference over a handful of
//! runs is not read as a finding.
//!
//! Built from records Orbit already keeps: implement invocation records,
//! explorer runs (whose input names the run that delegated them) and the
//! `context_files_widened` entries of task history.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::ContextFilesWidening;
use orbit_types::workflow::activity_job::{EXPLORE_JOB, EXPLORE_PARENT_RUN_KEY};
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::run_input::singular_task_id_from_input;
use crate::{InvocationQuery, InvocationRecord};

/// Version of the [`ExploreComparison`] shape.
pub const EXPLORE_COMPARISON_SCHEMA_VERSION: u32 = 1;

/// The implementing activity the comparison measures.
const IMPLEMENT_ACTIVITY: &str = "agent_implement";

/// The explorer activity whose spend is reported beside each cohort.
const EXPLORE_ACTIVITY: &str = "agent_explore";

/// Newest implement invocations read for one comparison. A window holding
/// more reports `truncated`.
const IMPLEMENT_SCAN_LIMIT: usize = 2_000;

/// Newest explorer invocations read for one comparison.
const EXPLORE_SCAN_LIMIT: usize = 5_000;

/// Implement runs with and without explorer calls, per implementing crew.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ExploreComparison {
    pub schema_version: u32,
    /// Implement runs the comparison read.
    pub runs_scanned: u64,
    /// Whether the window held more implement invocations than were read.
    pub truncated: bool,
    /// One row per implementing crew, most-sampled first.
    pub crews: Vec<ExploreCrewComparison>,
}

/// One implementing crew's two cohorts.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ExploreCrewComparison {
    pub crew: String,
    pub with_explore: ExploreCohort,
    pub without_explore: ExploreCohort,
}

/// One cohort's means over its runs, each with its own sample count.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ExploreCohort {
    /// Implement runs in the cohort: the sample count.
    pub runs: u64,
    /// Mean implement-step duration over every run, in milliseconds.
    pub mean_duration_ms: Option<u64>,
    /// Mean implementing-crew cost over the `priced_runs` whose every
    /// invocation was priced.
    pub mean_cost_usd: Option<f64>,
    pub priced_runs: u64,
    /// Runs whose task's footprint was widened by that run, over the
    /// `footprint_known_runs` whose task history could be read.
    pub footprint_miss_runs: u64,
    pub footprint_known_runs: u64,
    pub footprint_miss_rate: Option<f64>,
    /// Explorer calls the cohort's runs made, and what those explorers cost
    /// where priced. Zero for the cohort without explorers.
    pub explore_calls: u64,
    pub explore_cost_usd: f64,
}

/// One implement run, as the comparison counts it.
#[derive(Debug, Clone, PartialEq)]
struct ImplementRunFact {
    crew: String,
    duration_ms: u64,
    cost_usd: Option<f64>,
    explore_calls: u64,
    explore_cost_usd: f64,
    footprint_miss: Option<bool>,
}

impl OrbitRuntime {
    /// Compare implement runs with and without explorer calls since `since`
    /// (the whole history when `None`).
    pub fn explore_comparison(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<ExploreComparison, OrbitError> {
        let until = Utc::now();
        let implement = self.workspace_invocation_records(InvocationQuery {
            activity_id: Some(IMPLEMENT_ACTIVITY.to_string()),
            since,
            until: Some(until),
            limit: IMPLEMENT_SCAN_LIMIT,
            ..InvocationQuery::default()
        })?;
        let truncated = implement.len() >= IMPLEMENT_SCAN_LIMIT;
        let mut by_run: BTreeMap<String, Vec<InvocationRecord>> = BTreeMap::new();
        for record in implement {
            by_run
                .entry(record.job_run_id.clone())
                .or_default()
                .push(record);
        }

        // Explorers by the run that delegated them. Only an implement
        // agent's own calls classify its run.
        let explorers = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(EXPLORE_JOB.to_string()),
            created_since: since,
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let explorer_cost = per_run_cost(&self.workspace_invocation_records(InvocationQuery {
            activity_id: Some(EXPLORE_ACTIVITY.to_string()),
            since,
            until: Some(until),
            limit: EXPLORE_SCAN_LIMIT,
            ..InvocationQuery::default()
        })?);
        let mut explore_by_parent: BTreeMap<String, (u64, f64)> = BTreeMap::new();
        for explorer in &explorers {
            let input = explorer.input.as_ref();
            let field = |key: &str| {
                input
                    .and_then(|input| input.get(key))
                    .and_then(Value::as_str)
            };
            if field("parent_activity") != Some(IMPLEMENT_ACTIVITY) {
                continue;
            }
            let Some(parent) = field(EXPLORE_PARENT_RUN_KEY) else {
                continue;
            };
            let entry = explore_by_parent.entry(parent.to_string()).or_default();
            entry.0 += 1;
            entry.1 += explorer_cost
                .get(&explorer.run_id)
                .copied()
                .flatten()
                .unwrap_or(0.0);
        }

        let mut histories: BTreeMap<String, Option<BTreeSet<String>>> = BTreeMap::new();
        let mut facts = Vec::with_capacity(by_run.len());
        for (run_id, records) in &by_run {
            let run = self.get_job_run_backend(run_id)?;
            let crew = run
                .as_ref()
                .and_then(|run| run.resolved_crew.clone())
                .unwrap_or_else(|| crew_label(records));
            let task_id = run
                .as_ref()
                .and_then(|run| run.input.as_ref())
                .and_then(singular_task_id_from_input)
                .map(ToOwned::to_owned);
            let footprint_miss = task_id.and_then(|task_id| {
                histories
                    .entry(task_id.clone())
                    .or_insert_with(|| self.widening_runs(&task_id))
                    .as_ref()
                    .map(|runs| runs.contains(run_id))
            });
            let (explore_calls, explore_cost_usd) =
                explore_by_parent.get(run_id).copied().unwrap_or_default();
            facts.push(ImplementRunFact {
                crew,
                duration_ms: records.iter().map(|record| record.duration_ms).sum(),
                cost_usd: records_cost(records),
                explore_calls,
                explore_cost_usd,
                footprint_miss,
            });
        }
        Ok(compare(&facts, truncated))
    }

    /// The runs that widened `task_id`'s footprint; `None` when its history
    /// cannot be read.
    fn widening_runs(&self, task_id: &str) -> Option<BTreeSet<String>> {
        let history = self.get_task_history(task_id).ok()?;
        Some(
            history
                .iter()
                .filter_map(ContextFilesWidening::from_entry)
                .map(|widening| widening.run_id)
                .collect(),
        )
    }
}

/// A run's cost: provider-reported when every invocation reported one, else
/// price-table derived when every invocation is priced, else unknown.
fn records_cost(records: &[InvocationRecord]) -> Option<f64> {
    records
        .iter()
        .map(|record| record.provider_cost_usd)
        .sum::<Option<f64>>()
        .or_else(|| {
            records
                .iter()
                .map(|record| record.derived_cost_usd)
                .sum::<Option<f64>>()
        })
}

fn per_run_cost(records: &[InvocationRecord]) -> BTreeMap<String, Option<f64>> {
    let mut grouped: BTreeMap<String, Vec<InvocationRecord>> = BTreeMap::new();
    for record in records {
        grouped
            .entry(record.job_run_id.clone())
            .or_default()
            .push(record.clone());
    }
    grouped
        .into_iter()
        .map(|(run_id, records)| (run_id, records_cost(&records)))
        .collect()
}

/// A crew label for a run that recorded none: its provider and model.
fn crew_label(records: &[InvocationRecord]) -> String {
    records.first().map_or_else(
        || "unknown".to_string(),
        |record| match &record.model {
            Some(model) => format!("{}/{model}", record.agent),
            None => record.agent.clone(),
        },
    )
}

fn compare(facts: &[ImplementRunFact], truncated: bool) -> ExploreComparison {
    let mut crews: BTreeMap<&str, (Vec<&ImplementRunFact>, Vec<&ImplementRunFact>)> =
        BTreeMap::new();
    for fact in facts {
        let entry = crews.entry(fact.crew.as_str()).or_default();
        if fact.explore_calls > 0 {
            entry.0.push(fact);
        } else {
            entry.1.push(fact);
        }
    }
    let mut rows: Vec<ExploreCrewComparison> = crews
        .into_iter()
        .map(|(crew, (with, without))| ExploreCrewComparison {
            crew: crew.to_string(),
            with_explore: cohort(&with),
            without_explore: cohort(&without),
        })
        .collect();
    rows.sort_by(|left, right| {
        let total = |row: &ExploreCrewComparison| row.with_explore.runs + row.without_explore.runs;
        total(right)
            .cmp(&total(left))
            .then_with(|| left.crew.cmp(&right.crew))
    });
    ExploreComparison {
        schema_version: EXPLORE_COMPARISON_SCHEMA_VERSION,
        runs_scanned: facts.len() as u64,
        truncated,
        crews: rows,
    }
}

fn cohort(runs: &[&ImplementRunFact]) -> ExploreCohort {
    let count = runs.len() as u64;
    let priced: Vec<f64> = runs.iter().filter_map(|run| run.cost_usd).collect();
    let known: Vec<bool> = runs.iter().filter_map(|run| run.footprint_miss).collect();
    let misses = known.iter().filter(|missed| **missed).count() as u64;
    ExploreCohort {
        runs: count,
        mean_duration_ms: (count > 0)
            .then(|| runs.iter().map(|run| run.duration_ms).sum::<u64>() / count),
        mean_cost_usd: (!priced.is_empty())
            .then(|| priced.iter().sum::<f64>() / priced.len() as f64),
        priced_runs: priced.len() as u64,
        footprint_miss_runs: misses,
        footprint_known_runs: known.len() as u64,
        footprint_miss_rate: (!known.is_empty()).then(|| misses as f64 / known.len() as f64),
        explore_calls: runs.iter().map(|run| run.explore_calls).sum(),
        explore_cost_usd: runs.iter().map(|run| run.explore_cost_usd).sum(),
    }
}
