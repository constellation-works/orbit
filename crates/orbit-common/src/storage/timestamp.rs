//! Parsing of RFC 3339 timestamps read back from stored records.

use crate::OrbitError;
use chrono::{DateTime, Utc};

/// Parses an RFC 3339 timestamp read from a stored record, as UTC.
pub fn parse_stored_timestamp(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}
