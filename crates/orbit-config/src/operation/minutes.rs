use std::path::Path;

use orbit_common::OrbitError;

use super::keys::{LEGACY_REVIEW_MINUTES_KEY, REVIEW_MINUTES_KEY};
use crate::layering::{set_value_at_path, value_at_path};
use crate::registry::read_optional;

const MAX_REVIEW_MINUTES: u32 = 1_440;

/// Translate one document's deprecated `operation.review_minutes` into
/// `review.minutes`, in place, unless the document already states it
/// [ORB-13992]. The legacy key stays in the document so the load warns about
/// it by name.
pub(crate) fn translate_legacy_review_minutes(
    document: &mut toml::Value,
    config_path: &Path,
) -> Result<(), OrbitError> {
    let minutes = bounded(
        read_optional::<u32>(document, LEGACY_REVIEW_MINUTES_KEY, config_path)?,
        LEGACY_REVIEW_MINUTES_KEY,
        1,
        MAX_REVIEW_MINUTES,
    )?;
    if let Some(minutes) = minutes
        && value_at_path(document, REVIEW_MINUTES_KEY).is_none()
    {
        set_value_at_path(
            document,
            REVIEW_MINUTES_KEY,
            toml::Value::Integer(i64::from(minutes)),
        );
    }
    Ok(())
}

pub(crate) fn review_minutes(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(raw, REVIEW_MINUTES_KEY, 1, MAX_REVIEW_MINUTES)
}

fn bounded<T>(raw: Option<T>, key: &str, min: T, max: T) -> Result<Option<T>, OrbitError>
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    match raw {
        Some(value) if value < min || value > max => Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value {value}; expected {min}..={max}"
        ))),
        other => Ok(other),
    }
}
