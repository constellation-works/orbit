use chrono::{DateTime, Utc};

/// Parses a duration-relative string like "1h", "90d", "30m", "2w"
/// or an RFC3339/naive timestamp into a `DateTime<Utc>`.
/// For bare durations, the result is `now - duration`.
pub(crate) fn parse_since(raw: &str) -> Result<DateTime<Utc>, orbit_core::OrbitError> {
    orbit_common::protocol::tool_input::parse_since_with(raw, parse_duration_seconds)
}

/// Dashboard durations: a bare number means seconds; anything else follows
/// the shared `30m`/`2h`/`1d` grammar, which rejects overflow.
pub(crate) fn parse_duration_seconds(raw: &str) -> Result<u64, orbit_core::OrbitError> {
    let value = raw.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse::<u64>().map_err(|_| {
            orbit_core::OrbitError::InvalidInput(format!("invalid duration '{raw}'"))
        });
    }
    orbit_common::protocol::tool_input::parse_duration_seconds(value)
}
