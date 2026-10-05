use chrono::{DateTime, Utc};

/// Parses a duration-relative string like "1h", "90d", "30m", "2w"
/// or an RFC3339/naive timestamp into a `DateTime<Utc>`.
/// For bare durations, the result is `now - duration`.
pub(crate) fn parse_since(raw: &str) -> Result<DateTime<Utc>, orbit_core::OrbitError> {
    let value = raw.trim();

    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }

    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return Ok(naive.and_utc());
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return Ok(naive.and_utc());
    }

    let seconds = parse_duration_seconds(value)?;
    let seconds = i64::try_from(seconds).map_err(|_| {
        orbit_core::OrbitError::InvalidInput(format!(
            "duration '{raw}' is too large to convert into a timestamp"
        ))
    })?;
    let duration = chrono::Duration::try_seconds(seconds).ok_or_else(|| {
        orbit_core::OrbitError::InvalidInput(format!(
            "duration '{raw}' is too large to convert into a timestamp"
        ))
    })?;
    Utc::now().checked_sub_signed(duration).ok_or_else(|| {
        orbit_core::OrbitError::InvalidInput(format!(
            "duration '{raw}' is too large to convert into a timestamp"
        ))
    })
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
