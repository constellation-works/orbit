//! Frozen noise scales, one record per season (§4.2). A season is appended
//! by `orbit agents rank calibrate` and never edited: the ranking reads the
//! newest season calibrated under the current measure definition, and
//! records which one it used.

use std::io::Read as _;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, open_read_only_no_follow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::measures::{MEASURE_COUNT, MEASURES};
use crate::OrbitRuntime;

/// Bumped whenever a measure's definition, weight or attribution rule
/// changes, which starts a new season: scales calibrated under an older
/// definition are kept on file but no longer used.
pub(super) const DEFINITION_VERSION: u32 = 1;

const SEASONS_FILE: &str = "agents/rank-seasons.json";

/// One measure's frozen scale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SeasonScale {
    pub(super) key: String,
    pub(super) weight: f64,
    /// `None` for a measure unrecorded or without variation then.
    pub(super) scale: Option<f64>,
    pub(super) raw: Option<f64>,
    pub(super) floor: Option<f64>,
    pub(super) weekly: Vec<f64>,
}

/// One season's frozen scales and the calibration behind them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Season {
    pub(super) season: u32,
    pub(super) definition: u32,
    pub(super) calibrated_at: DateTime<Utc>,
    /// The calibration window, `[since, until)`.
    pub(super) since: DateTime<Utc>,
    pub(super) until: DateTime<Utc>,
    pub(super) execution: String,
    pub(super) n_ref: usize,
    pub(super) draws: usize,
    pub(super) tasks: usize,
    pub(super) scales: Vec<SeasonScale>,
}

impl Season {
    /// The frozen scale per measure, in measure order.
    pub(super) fn scales(&self) -> [Option<f64>; MEASURE_COUNT] {
        std::array::from_fn(|index| self.scale(index).and_then(|scale| scale.scale))
    }

    pub(super) fn floors(&self) -> [Option<f64>; MEASURE_COUNT] {
        std::array::from_fn(|index| self.scale(index).and_then(|scale| scale.floor))
    }

    fn scale(&self, index: usize) -> Option<&SeasonScale> {
        self.scales
            .iter()
            .find(|scale| scale.key == MEASURES[index].key)
    }

    pub(super) fn json(&self) -> Value {
        json!(self)
    }
}

/// Every calibrated season of a workspace, oldest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RankSeasons {
    seasons: Vec<Season>,
}

impl RankSeasons {
    /// The newest season calibrated under the current definition.
    pub(super) fn current(&self) -> Option<&Season> {
        self.seasons
            .iter()
            .rev()
            .find(|season| season.definition == DEFINITION_VERSION)
    }

    pub(super) fn next_number(&self) -> u32 {
        self.seasons
            .iter()
            .map(|season| season.season + 1)
            .max()
            .unwrap_or(0)
    }

    pub(super) fn push(&mut self, season: Season) {
        self.seasons.push(season);
    }

    pub(super) fn len(&self) -> usize {
        self.seasons.len()
    }

    pub(super) fn newest(&self) -> Option<&Season> {
        self.seasons.last()
    }
}

fn path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.paths().state_dir.join(SEASONS_FILE)
}

impl OrbitRuntime {
    /// The workspace's calibrated ranking seasons; none before the first
    /// `orbit agents rank calibrate`. An unreadable file is an error rather
    /// than an empty history, so a calibration never overwrites it.
    pub fn agent_rank_seasons(&self) -> Result<RankSeasons, OrbitError> {
        let path = path(self);
        let mut text = String::new();
        match open_read_only_no_follow(&path).and_then(|mut file| file.read_to_string(&mut text)) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RankSeasons::default());
            }
            Err(error) => return Err(error.into()),
        }
        serde_json::from_str(&text).map_err(|error| {
            OrbitError::Store(format!(
                "ranking seasons at {} are unreadable: {error}",
                path.display()
            ))
        })
    }

    pub(super) fn write_agent_rank_seasons(&self, seasons: &RankSeasons) -> Result<(), OrbitError> {
        let path = path(self);
        let text = serde_json::to_string_pretty(seasons)
            .map_err(|error| OrbitError::Store(format!("serialize ranking seasons: {error}")))?;
        atomic_write_text(&path, &text).map_err(|error| OrbitError::from_write_io(&path, error))
    }
}
