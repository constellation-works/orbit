//! The model ranking (model-ranking design §2–§4, §6): one row per
//! implementer model, its crews and effort levels pooled, scored on six
//! measures against the implementer scorecard's same-mix expectations.
//!
//! Each measure's residual per unit is divided by a frozen noise scale,
//! the weighted mean over the measures that count for the row is shrunk
//! toward the other eligible models (K = 30), and a within-tier bootstrap
//! gives the interval, the paired probabilities and the tie bands, each band
//! ordered by cost. Measures whose events Orbit does not record yet report
//! as unmeasured and leave every weight denominator.
//!
//! The ranked window is the 30 days ending at the settled cutoff,
//! `until − settle_lag`; `live` ends it at `until` and marks it
//! provisional. Scales come from the newest season `orbit agents rank
//! calibrate` stored; before the first one they are estimated from the
//! ranked window itself and the payload says so.

mod board;
mod measures;
mod sample;
mod season;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Map, Value, json};

use self::board::ModelRow;
use self::measures::{COUNTING_ORDER, MEASURE_COUNT, MEASURES, measures_json, weighted_terms};
use self::sample::{DRAWS, Record};
pub use self::season::RankSeasons;
use self::season::{DEFINITION_VERSION, Season, SeasonScale};
use super::query::{SpanError, parse_span};
use super::{
    AgentFacts, AgentQuery, ExecutionMode, MODEL_UNKNOWN, Owner, WindowBounds, WindowSpan, roles,
};
use crate::OrbitRuntime;

const DEFAULT_SETTLE_LAG: &str = "14d";

/// A validated ranking selection.
#[derive(Debug, Clone)]
pub struct RankQuery {
    /// The ranked window, ending at the settled cutoff (or at `until` when
    /// live).
    query: AgentQuery,
    until: DateTime<Utc>,
    settle_lag: String,
    live: bool,
}

impl RankQuery {
    /// Validate a selection. `window` is a time window (default `30d`);
    /// `until` an RFC 3339 instant or a duration ago (default now);
    /// `settle_lag` a duration (default `14d`) the ranked window ends
    /// before `until`, unless `live`; `execution` the scorecard's selector.
    pub fn parse(
        window: Option<&str>,
        until: Option<&str>,
        settle_lag: Option<&str>,
        execution: Option<&str>,
        live: bool,
    ) -> Result<Self, OrbitError> {
        let query = AgentQuery::parse(window, None)?
            .with_execution(execution)?
            .with_until(until)?;
        if matches!(query.span, WindowSpan::Attempts(_)) {
            return Err(OrbitError::InvalidInput(format!(
                "the ranking needs a time window such as 30d; got '{}'",
                query.requested
            )));
        }
        let settle_lag = settle_lag
            .map(str::trim)
            .unwrap_or(DEFAULT_SETTLE_LAG)
            .to_string();
        let lag = parse_span(&settle_lag).map_err(|error| {
            OrbitError::InvalidInput(match error {
                SpanError::Malformed => {
                    format!("settle_lag must be a duration such as 14d; got '{settle_lag}'")
                }
                SpanError::OutOfRange => format!("settle_lag is out of range: {settle_lag}"),
            })
        })?;
        let until = query.until.unwrap_or_else(Utc::now);
        let cutoff = if live {
            until
        } else {
            until.checked_sub_signed(lag).ok_or_else(|| {
                OrbitError::InvalidInput(format!("settle_lag is out of range: {settle_lag}"))
            })?
        };
        Ok(Self {
            query: query.with_until_at(cutoff),
            until,
            settle_lag,
            live,
        })
    }

    fn header(&self, facts: &AgentFacts, window: WindowBounds, cache_ttl: Duration) -> Value {
        let mut header = self.query.header(facts, window, cache_ttl);
        if let Some(fields) = header.as_object_mut() {
            fields.remove("min_support");
            fields.remove("window_attempts");
        }
        header["role"] = json!("implementers");
        header["requested_until"] = json!(self.until.to_rfc3339_opts(SecondsFormat::Secs, true));
        header["settle_lag"] = json!(self.settle_lag);
        header["live"] = json!(self.live);
        // A live board's newest work has not had time to show its escaped
        // regressions; it is never frozen or published.
        header["provisional"] = json!(self.live);
        header
    }
}

/// The scales a board divides by, and where they came from.
struct Scales {
    scales: [Option<f64>; MEASURE_COUNT],
    floors: [Option<f64>; MEASURE_COUNT],
    season: Value,
    source: &'static str,
}

/// The reference sample size: the median eligible model's tasks, or the
/// median model's when none is eligible yet.
fn reference_size(rows: &[ModelRow]) -> usize {
    let attributed: Vec<&ModelRow> = rows
        .iter()
        .filter(|row| row.model != MODEL_UNKNOWN)
        .collect();
    let eligible: Vec<f64> = attributed
        .iter()
        .filter(|row| row.ineligibility().is_none())
        .map(|row| row.units() as f64)
        .collect();
    let mut units = if eligible.is_empty() {
        attributed.iter().map(|row| row.units() as f64).collect()
    } else {
        eligible
    };
    let median = sample::percentile(&mut units, 50.0);
    median.map_or(1, |median| (median.round() as usize).max(1))
}

fn calibrate_rows(rows: &[ModelRow], window: WindowBounds, season: u32, execution: &str) -> Season {
    let records: Vec<&Record> = rows.iter().flat_map(|row| &row.records).collect();
    let n_ref = reference_size(rows);
    let calibrated = sample::calibrate(&records, window.since(), n_ref);
    Season {
        season,
        definition: DEFINITION_VERSION,
        calibrated_at: Utc::now(),
        since: window.since(),
        until: window.reported_until(),
        execution: execution.to_string(),
        n_ref,
        draws: DRAWS,
        tasks: records.len(),
        scales: MEASURES
            .iter()
            .zip(calibrated)
            .map(|(measure, calibrated)| SeasonScale {
                key: measure.key.to_string(),
                weight: measure.weight,
                scale: calibrated.scale,
                raw: calibrated.raw,
                floor: calibrated.floor,
                weekly: calibrated.weekly,
            })
            .collect(),
    }
}

/// The ranking for `query`, with the newest current season's scales, or
/// scales estimated from the ranked window when no season is calibrated.
/// `cache_ttl` is how long the caller keeps `facts`.
pub fn agent_rank(
    facts: &AgentFacts,
    query: &RankQuery,
    seasons: &RankSeasons,
    cache_ttl: Duration,
) -> Value {
    let window = query.query.bounds(facts);
    let execution = query.query.execution;
    // Shown beside the ranking, not filtered by the execution selector, so
    // interactive work stays visible while the default counts leave it out.
    let (execution_board, execution_by_model) = execution_census(facts, window);
    let rows = board::pool(roles::implementer_ledger(facts, window, execution));
    let scales = match seasons.current() {
        Some(season) => Scales {
            scales: season.scales(),
            floors: season.floors(),
            season: season.json(),
            source: "season",
        },
        None => {
            let estimate = calibrate_rows(&rows, window, 0, execution.as_str());
            Scales {
                scales: estimate.scales(),
                floors: estimate.floors(),
                season: Value::Null,
                source: "window_estimate",
            }
        }
    };
    let board = board::compute(&rows, &scales.scales);

    let mut payload = query.header(facts, window, cache_ttl);
    payload["status"] = json!("ok");
    payload["measures"] = json!(measures_json(&scales.scales, &scales.floors));
    payload["weights_total"] = json!(MEASURES.iter().map(|m| m.weight).sum::<f64>());
    payload["counting_order"] = json!(COUNTING_ORDER);
    payload["season"] = scales.season.clone();
    payload["definition"] = json!(DEFINITION_VERSION);
    payload["scales_source"] = json!(scales.source);
    payload["qualified"] = json!(board.qualified);
    payload["eligible_models"] = json!(board.placed.iter().filter(|p| p.eligible).count());
    payload["rules"] = json!({
        "shrinkage_k": board::SHRINKAGE_K,
        "min_tasks": board::MIN_TASKS,
        "min_tiers": board::MIN_TIERS,
        "min_tier_tasks": board::MIN_TIER_TASKS,
        "min_eligible_models": board::MIN_ELIGIBLE_MODELS,
        "band_probability": board::BAND_PROBABILITY,
        "min_cost_coverage": board::MIN_COST_COVERAGE,
        "interval_percentiles": [board::INTERVAL.0, board::INTERVAL.1],
        "draws": DRAWS,
    });
    let unattributed = rows.iter().find(|row| row.model == MODEL_UNKNOWN);
    payload["unattributed"] = json!({
        "tasks": unattributed.map_or(0, ModelRow::units),
        "crews": unattributed.map(crews_json).unwrap_or_default(),
    });
    payload["execution_split"] = execution_json(&execution_board);
    payload["not_scored"] = not_scored(facts, window);
    payload["rows"] = Value::Array(
        board
            .order
            .iter()
            .filter(|&&index| rows[index].model != MODEL_UNKNOWN)
            .map(|&index| {
                let execution = execution_by_model
                    .get(&rows[index].model)
                    .cloned()
                    .unwrap_or_default();
                row_json(
                    facts,
                    window,
                    &rows[index],
                    &board,
                    index,
                    &scales,
                    &execution,
                )
            })
            .collect(),
    );
    let mut paired = Map::new();
    for (&(a, b), &p) in &board.paired {
        let entry = paired
            .entry(rows[a].model.clone())
            .or_insert_with(|| json!({}));
        entry[rows[b].model.as_str()] = round(p);
    }
    payload["paired_probabilities"] = Value::Object(paired);
    payload
}

fn crews_json(row: &ModelRow) -> Vec<Value> {
    row.crews
        .iter()
        .map(|(crew, tasks)| json!({"crew": crew, "tasks": tasks}))
        .collect()
}

fn row_json(
    facts: &AgentFacts,
    window: WindowBounds,
    row: &ModelRow,
    board: &board::Board,
    index: usize,
    scales: &Scales,
    execution: &ExecutionCounts,
) -> Value {
    let placed = &board.placed[index];
    let totals = row.totals();
    let (terms, weight) = weighted_terms(&totals, &scales.scales);
    let mut measures = Map::new();
    for (position, measure) in MEASURES.iter().enumerate() {
        let total = totals[position];
        let (status, reason) = match (measure.unrecorded, terms[position]) {
            (Some(reason), _) => ("unmeasured", Some(reason)),
            (None, Some(_)) => ("measured", None),
            (None, None) if total.units == 0 => {
                ("unmeasured", Some("no units for this model in the window"))
            }
            (None, None) => ("no_scale", Some("no noise scale for this measure")),
        };
        let measured = measure.unrecorded.is_none() && total.units > 0;
        measures.insert(
            measure.key.to_string(),
            json!({
                "status": status,
                "reason": reason,
                "weight": measure.weight,
                "units": total.units,
                "unit": measure.units,
                "actual": measured.then(|| round(total.actual)),
                "expected": measured.then(|| round(total.expected)),
                "residual": total.residual(measure).filter(|_| measured).map(round),
                // Score points before shrinkage: 10 × w × resid ÷ scale ÷ Σw.
                "contribution": terms[position]
                    .filter(|_| weight > 0.0)
                    .map(|term| round(10.0 * term / weight)),
            }),
        );
    }
    let rank_label = placed.band.map(|band| {
        if placed.band_size > 1 {
            format!("{band}=")
        } else {
            band.to_string()
        }
    });
    let units = row.units();
    json!({
        "model": row.model,
        "rank": placed.band,
        "rank_label": rank_label,
        "band_size": (placed.band_size > 0).then_some(placed.band_size),
        "eligible": placed.eligible,
        "ineligible_reason": row.ineligibility().or_else(|| {
            (!placed.eligible).then(|| "no measure with units and a scale".to_string())
        }),
        "units": units,
        "tasks_by_tier": row.tiers(),
        "score": placed.score().map(round),
        "raw_score": placed.z.map(|z| round(100.0 + 10.0 * z)),
        "z": placed.z.map(round4),
        "z_loo": placed.z_loo.map(round4),
        "z_shrunk": placed.z_shrunk.map(round4),
        "weights_measured": weight,
        "interval": placed.interval.map(|(low, high)| json!({"p10": round(low), "p90": round(high)})),
        "cost": {
            "ratio": placed.cost.ratio.map(round4),
            "per_task": placed.cost.per_task.map(round4),
            "expected_per_task": placed.cost.expected_per_task.map(round4),
            "coverage": round4(placed.cost.coverage),
            "cost_unknown": placed.cost.unknown,
            "pricing": "recorded provider or derived list-price cost of the model's runs",
        },
        "measures": measures,
        "landed_tasks": round(row.landed),
        "landed_rate": (units > 0).then(|| round4(row.landed / units as f64)),
        "crews": crews_json(row),
        "effort_split": effort_split(facts, window, &row.model),
        "tiers": tiers_json(row, &scales.scales),
        "lines": { "changed": row.lines, "tasks": row.sized_tasks },
        "execution": execution_json(execution),
    })
}

fn tiers_json(row: &ModelRow, scales: &[Option<f64>; MEASURE_COUNT]) -> Vec<Value> {
    board::tier_points(row, scales)
        .into_iter()
        .map(|point| {
            json!({
                "tier": point.tier,
                "tasks": point.tasks,
                "score": point.score.map(round),
                "cost_per_task": point.cost_per_task.map(round4),
                "cost_coverage": round4(point.cost_coverage),
                "cost_unknown": point.cost_unknown,
            })
        })
        .collect()
}

/// Managed, interactive and unclassified task counts.
#[derive(Clone, Default)]
struct ExecutionCounts {
    managed: usize,
    interactive: usize,
    unclassified: usize,
}

impl ExecutionCounts {
    fn add(&mut self, mode: ExecutionMode) {
        match mode {
            ExecutionMode::Managed => self.managed += 1,
            ExecutionMode::Interactive => self.interactive += 1,
            ExecutionMode::Unclassified => self.unclassified += 1,
        }
    }
}

fn execution_json(counts: &ExecutionCounts) -> Value {
    json!({
        "managed_tasks": counts.managed,
        "interactive_tasks": counts.interactive,
        "unclassified_tasks": counts.unclassified,
    })
}

/// Window-wide, and not filtered by the ranking's execution selector.
fn execution_census(
    facts: &AgentFacts,
    window: WindowBounds,
) -> (ExecutionCounts, BTreeMap<String, ExecutionCounts>) {
    let mut board = ExecutionCounts::default();
    let mut by_model: BTreeMap<String, ExecutionCounts> = BTreeMap::new();
    for task in &facts.tasks {
        let mut models = BTreeSet::new();
        let mut counted = false;
        for attempt in &task.attempts {
            if !window.admits(attempt.start) || attempt.no_diff() {
                continue;
            }
            counted = true;
            if let Some(model) = attempt.runner.model.as_deref()
                && !model.is_empty()
                && model != MODEL_UNKNOWN
            {
                models.insert(model.to_string());
            }
        }
        if !counted {
            continue;
        }
        board.add(task.execution);
        for model in models {
            by_model.entry(model).or_default().add(task.execution);
        }
    }
    (board, by_model)
}

/// Not-scored charges whose anchor falls in the window. These are problems,
/// not tasks, and they are board-level: a Not-scored charge usually names no model.
fn not_scored(facts: &AgentFacts, window: WindowBounds) -> Value {
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    let mut count = 0usize;
    for charge in &facts.charges {
        let Owner::NotScored(reason) = charge.owner else {
            continue;
        };
        if !window.admits(charge.anchor) {
            continue;
        }
        count += 1;
        *reasons.entry(reason).or_default() += 1;
    }
    json!({
        "count": count,
        "reasons": reasons,
    })
}

/// The model's completed implementation invocations in the window by
/// reasoning effort.
fn effort_split(facts: &AgentFacts, window: WindowBounds, model: &str) -> Value {
    json!(orbit_store::contracts::agent_effort_usage(
        facts.invocations.iter().filter(|fact| {
            window.admits(fact.ts)
                && fact.model.as_deref() == Some(model)
                && matches!(
                    fact.activity_id.as_deref(),
                    Some("implement_one" | "agent_implement")
                )
        })
    ))
}

/// Rounded to two places; `+ 0.0` turns a negative zero into zero.
fn round(value: f64) -> Value {
    json!((value * 100.0).round() / 100.0 + 0.0)
}

fn round4(value: f64) -> Value {
    json!((value * 10_000.0).round() / 10_000.0 + 0.0)
}

/// `seasons` with a new season appended: noise scales calibrated on the
/// settled window of `query`, numbered after the seasons on file. A live
/// query is refused, and so is a window without implemented work.
pub fn agent_rank_calibration(
    facts: &AgentFacts,
    query: &RankQuery,
    seasons: &RankSeasons,
) -> Result<RankSeasons, OrbitError> {
    if query.live {
        return Err(OrbitError::InvalidInput(
            "a season is calibrated on the settled window; drop live".to_string(),
        ));
    }
    let window = query.query.bounds(facts);
    let execution = query.query.execution;
    let rows = board::pool(roles::implementer_ledger(facts, window, execution));
    if rows.iter().all(|row| row.units() == 0) {
        return Err(OrbitError::InvalidInput(
            "no implemented task in the calibration window; widen window or move until".to_string(),
        ));
    }
    let mut seasons = seasons.clone();
    seasons.push(calibrate_rows(
        &rows,
        window,
        seasons.next_number(),
        execution.as_str(),
    ));
    Ok(seasons)
}

impl OrbitRuntime {
    /// Scan, then rank implementer models for `query` with the workspace's
    /// current season, with a cache period of zero.
    pub fn agent_rank(&self, query: &RankQuery) -> Result<Value, OrbitError> {
        let seasons = self.agent_rank_seasons()?;
        Ok(agent_rank(
            &self.agent_scan()?,
            query,
            &seasons,
            Duration::ZERO,
        ))
    }

    /// Calibrate a new season's noise scales on the settled window of
    /// `query` and store it after the seasons on file. Returns the season.
    pub fn agent_rank_calibrate(&self, query: &RankQuery) -> Result<Value, OrbitError> {
        let seasons =
            agent_rank_calibration(&self.agent_scan()?, query, &self.agent_rank_seasons()?)?;
        self.write_agent_rank_seasons(&seasons)?;
        Ok(json!({
            "season": seasons.newest().map(Season::json),
            "seasons_on_file": seasons.len(),
            "definition": DEFINITION_VERSION,
        }))
    }
}
