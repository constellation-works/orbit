//! `list`: list friction records through the wire filter.

use chrono::{DateTime, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::friction::FRICTION_LIST_RESPONSE_MODE_WITH_NOTES;
use orbit_common::governance::operation::OperationInput;
use orbit_store::contract::FrictionListFilter;
use orbit_types::record::FrictionStatus;
use serde_json::{Value, json};

use super::record::{parse_status, record_to_json};
use crate::OrbitRuntime;
use crate::application::search::whitespace_query_note;

pub(super) fn list(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    list_in(&crate::runtime::friction::store_for(runtime)?, input)
}

/// Translate the wire filter into the store filter, including the page, so
/// SQLite decides which rows exist before any body is decoded.
fn list_in(
    store: &crate::runtime::friction::WorkspaceFrictions,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    let response_mode = input.optional_string("response_mode")?;
    let with_notes = match response_mode.as_deref() {
        None => false,
        Some(FRICTION_LIST_RESPONSE_MODE_WITH_NOTES) => true,
        Some(value) => {
            return Err(OrbitError::InvalidInput(format!(
                "`response_mode` must be `{FRICTION_LIST_RESPONSE_MODE_WITH_NOTES}`, got '{value}'"
            )));
        }
    };
    let month_bounds = input
        .optional_string("month")?
        .map(|raw| parse_month_bounds(&raw))
        .transpose()?;
    let (month_from, month_to) = month_bounds
        .map(|(from, to)| (Some(from), Some(to)))
        .unwrap_or((None, None));
    let newest_first = match input.optional_string("order")?.as_deref() {
        None | Some("oldest") => false,
        Some("newest") => true,
        Some(value) => {
            return Err(OrbitError::InvalidInput(format!(
                "`order` must be `oldest` or `newest`, got '{value}'"
            )));
        }
    };
    let filter = FrictionListFilter {
        model: input.optional_string("model")?,
        statuses: input
            .optional_string("status")?
            .map(|statuses| parse_status_filter(&statuses))
            .transpose()?
            .unwrap_or_default(),
        tag: input.optional_string("tag")?,
        q: input.optional_string("q")?,
        from: input
            .optional_string("from")?
            .map(|raw| parse_timestamp("from", &raw))
            .transpose()?
            .or(month_from),
        to: input
            .optional_string("to")?
            .map(|raw| parse_timestamp("to", &raw))
            .transpose()?
            .or(month_to),
        limit: input.optional_count("limit")?,
        offset: input.optional_count("offset")?.unwrap_or(0),
        newest_first,
    };
    let records = store
        .list(&filter)?
        .into_iter()
        .map(record_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list_payload(records, filter.q.as_deref(), with_notes))
}

/// Preserve the historical array unless the caller explicitly requests the
/// stable notes envelope. The envelope shape does not depend on query results.
fn list_payload(records: Vec<Value>, query: Option<&str>, with_notes: bool) -> Value {
    if with_notes {
        let notes = if records.is_empty() {
            query
                .and_then(|query| whitespace_query_note(query, true))
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        return json!({ "records": records, "notes": notes });
    }
    Value::Array(records)
}

fn parse_timestamp(field: &str, raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::InvalidInput(format!("`{field}` must be RFC3339: {error}")))
}

fn parse_month_bounds(raw: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), OrbitError> {
    let bytes = raw.as_bytes();
    let format_ok = bytes.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit);
    if !format_ok {
        return Err(OrbitError::InvalidInput(format!(
            "`month` must be in YYYY-MM format, got '{raw}'"
        )));
    }
    let year = raw[..4].parse::<i32>().map_err(|_| {
        OrbitError::InvalidInput(format!("invalid year component in `month`: {raw}"))
    })?;
    let month = raw[5..].parse::<u32>().map_err(|_| {
        OrbitError::InvalidInput(format!("invalid month component in `month`: {raw}"))
    })?;
    if !(1..=12).contains(&month) {
        return Err(OrbitError::InvalidInput(format!(
            "`month` component must be 01-12, got '{raw}'"
        )));
    }
    let start = Utc
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| OrbitError::InvalidInput(format!("invalid `month`: {raw}")))?;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end_exclusive = Utc
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| OrbitError::InvalidInput(format!("invalid `month`: {raw}")))?;
    Ok((start, end_exclusive - chrono::Duration::nanoseconds(1)))
}

/// A list's status filter: one status, several comma-separated, or `all`
/// for every status (an empty filter).
fn parse_status_filter(raw: &str) -> Result<Vec<FrictionStatus>, OrbitError> {
    if raw.trim() == "all" {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(parse_status)
        .collect()
}
