use std::time::Duration;

use chrono::Utc;
use orbit_common::OrbitError;
use serde_json::{Map, Value, json};

use super::super::{AgentFacts, MODEL_UNKNOWN, WindowBounds, roles};
use super::board::ModelRow;
use super::execution::{execution_census, execution_json};
use super::measures::{COUNTING_ORDER, MEASURE_COUNT, MEASURES, measures_json};
use super::query::RankQuery;
use super::render::{crews_json, not_scored, round, row_json};
use super::sample::{DRAWS, Record};
use super::season::{DEFINITION_VERSION, RankSeasons, Season, SeasonScale};
use super::{board, sample};
use crate::OrbitRuntime;

/// The scales a board divides by, and where they came from.
pub(super) struct Scales {
    pub(super) scales: [Option<f64>; MEASURE_COUNT],
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
