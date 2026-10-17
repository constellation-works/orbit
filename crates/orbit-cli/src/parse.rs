use chrono::{DateTime, Utc};
pub use orbit_common::protocol::tool_input::parse_duration_seconds;

/// Shared CSV parsing for task add/update
/// context file parsing and `orbit job`'s `--env-extra` handling.
pub fn csv_to_vec(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Parse a `--limit` value, rejecting zero: a zero limit would return nothing
/// (or, for some readers, everything), and neither is what the caller asked for.
pub fn positive_limit(raw: &str) -> Result<usize, String> {
    let value: usize = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a valid limit (expected a positive integer)"))?;
    if value == 0 {
        return Err("limit must be at least 1".to_string());
    }
    Ok(value)
}

/// Parses a duration-relative string like "1h", "90d", "30m", "2w"
/// or an RFC3339/naive timestamp into a `DateTime<Utc>`.
/// For bare durations, the result is `now - duration`.
pub(crate) fn parse_since(raw: &str) -> Result<DateTime<Utc>, orbit_core::OrbitError> {
    orbit_common::protocol::tool_input::parse_since_with(raw, parse_duration_seconds)
}
