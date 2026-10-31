use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    SelectiveBeforeLandingMode, SelectiveUnknownRisk, validate_risk_threshold,
    validate_selective_path_glob,
};

use super::keys::{
    SELECTIVE_MODE_KEY, SELECTIVE_PATHS_KEY, SELECTIVE_RISK_THRESHOLD_KEY,
    SELECTIVE_UNKNOWN_RISK_KEY,
};
use crate::layering::value_at_path;

/// Parse `review.selective_before_landing.mode`.
pub fn selective_mode_parse(raw: &str) -> Result<SelectiveBeforeLandingMode, OrbitError> {
    match raw.trim() {
        "off" => Ok(SelectiveBeforeLandingMode::Off),
        "observe" => Ok(SelectiveBeforeLandingMode::Observe),
        "enforce" => Ok(SelectiveBeforeLandingMode::Enforce),
        other => Err(OrbitError::InvalidInput(format!(
            "{SELECTIVE_MODE_KEY} has invalid value '{other}'; expected one of: {}",
            SelectiveBeforeLandingMode::CHOICES.join(", ")
        ))),
    }
}

/// Parse `review.selective_before_landing.unknown_risk`.
pub fn selective_unknown_risk_parse(raw: &str) -> Result<SelectiveUnknownRisk, OrbitError> {
    match raw.trim() {
        "review" => Ok(SelectiveUnknownRisk::Review),
        "skip" => Ok(SelectiveUnknownRisk::Skip),
        other => Err(OrbitError::InvalidInput(format!(
            "{SELECTIVE_UNKNOWN_RISK_KEY} has invalid value '{other}'; expected one of: {}",
            SelectiveUnknownRisk::CHOICES.join(", ")
        ))),
    }
}

/// Validate a settings-snapshot `risk_threshold` and keep its canonical
/// decimal spelling; unset stays unset (the risk predicate is off).
pub(crate) fn selective_threshold_value(raw: Option<f64>) -> Result<Option<String>, OrbitError> {
    let Some(value) = raw else { return Ok(None) };
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(OrbitError::InvalidInput(format!(
            "{SELECTIVE_RISK_THRESHOLD_KEY} has invalid value {value}; expected a finite \
             probability in [0, 1]"
        )));
    }
    Ok(Some(if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }))
}

/// Validate one layer's `review.selective_before_landing.paths`: every
/// pattern a positive repository-relative glob. The list replaces an
/// earlier layer's list outright, never appends.
pub(crate) fn selective_paths(raw: Option<Vec<String>>) -> Result<Option<Vec<String>>, OrbitError> {
    let Some(raw) = raw else { return Ok(None) };
    let mut paths = Vec::with_capacity(raw.len());
    for (index, pattern) in raw.iter().enumerate() {
        let pattern = pattern.trim();
        validate_selective_path_glob(pattern).map_err(|error| {
            OrbitError::InvalidInput(format!("{SELECTIVE_PATHS_KEY}[{index}]: {error}"))
        })?;
        if !paths.iter().any(|known| known == pattern) {
            paths.push(pattern.to_string());
        }
    }
    Ok(Some(paths))
}

/// Read `risk_threshold` from a document: a TOML float or integer in
/// `[0, 1]`, kept in its canonical decimal spelling.
pub(super) fn selective_threshold(
    document: &toml::Value,
    config_path: &Path,
) -> Result<Option<String>, OrbitError> {
    let Some(value) = value_at_path(document, SELECTIVE_RISK_THRESHOLD_KEY) else {
        return Ok(None);
    };
    let raw = match value {
        toml::Value::Float(value) => {
            if value.fract() == 0.0 && value.is_finite() {
                format!("{value:.1}")
            } else {
                value.to_string()
            }
        }
        toml::Value::Integer(value) => value.to_string(),
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "{}: {SELECTIVE_RISK_THRESHOLD_KEY} must be a number in [0, 1]",
                config_path.display()
            )));
        }
    };
    validate_risk_threshold(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!("{SELECTIVE_RISK_THRESHOLD_KEY}: {error}"))
    })?;
    Ok(Some(raw))
}

/// The selective statements one layer makes. Each subfield inherits
/// independently; a stated `paths` list (even empty) replaces the lower
/// layer's list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectiveLayer {
    /// Explicit `review.selective_before_landing.mode`.
    pub mode: Option<SelectiveBeforeLandingMode>,
    /// Explicit `risk_threshold`, canonical decimal spelling.
    pub risk_threshold: Option<String>,
    /// Explicit `paths` replacement list.
    pub paths: Option<Vec<String>>,
    /// Explicit `unknown_risk`.
    pub unknown_risk: Option<SelectiveUnknownRisk>,
}
