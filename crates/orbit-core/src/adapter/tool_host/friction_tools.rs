//! The friction handler table [ORB-10358].
//!
//! ADR-0209 bearing 1: friction verbs are declared once as data in
//! `orbit_common::governance::friction::operations`. Handlers need `&OrbitRuntime`, which
//! lives above that leaf crate, so the handler half of the table lives here and
//! is joined to the spec half by [`FrictionVerb`]. [`dispatch`] is the join
//! point, and its exhaustive `match` is what makes a spec without a handler a
//! compile error.

use std::str::FromStr;

use chrono::{DateTime, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::friction::{
    FRICTION_LIST_RESPONSE_MODE_WITH_NOTES, FrictionVerb, effective_title, normalize_title,
};
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_raw_string, optional_string, required_string,
};
use orbit_store::contracts::{
    FrictionAddParams, FrictionListFilter, FrictionStoreBackend, FrictionUpdateParams,
    StoredFrictionRecord,
};
use orbit_types::record::{FrictionRecord, FrictionStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::search::empty_whitespace_query_note;

/// Route one friction verb to its handler.
///
/// Exhaustive by construction: adding a [`FrictionVerb`] variant breaks this
/// match until the verb has an implementation.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: FrictionVerb,
    input: Value,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    match verb {
        FrictionVerb::Add => add(runtime, input, model),
        FrictionVerb::List => list(runtime, input),
        FrictionVerb::Show => show(runtime, input),
        FrictionVerb::Stats => stats(runtime),
        FrictionVerb::Tags => tags(runtime),
        FrictionVerb::Update => update(runtime, input),
        FrictionVerb::Resolve => resolve(runtime, input),
    }
}

fn add(runtime: &OrbitRuntime, input: Value, model: Option<String>) -> Result<Value, OrbitError> {
    let body = required_string(&input, &["body", "description"], "body")?;
    let title = optional_raw_string(&input, "title")?
        .map(|raw| normalize_title(&raw))
        .transpose()?;
    let tags = optional_csv_or_string_list_alias(&input, &["tags", "tag"])?.unwrap_or_default();
    let during_task =
        optional_string(&input, "during_task")?.or(optional_string(&input, "task_id")?);
    let model = model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OrbitError::InvalidInput("orbit.friction.add requires `model`".to_string())
        })?;
    let stored = crate::runtime::friction::store_for(runtime)?.add(FrictionAddParams {
        model,
        title,
        body,
        tags,
        during_task,
        created_at: Utc::now(),
    })?;
    record_to_json(stored)
}

fn list(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    list_in(
        crate::runtime::friction::store_for(runtime)?.as_ref(),
        input,
    )
}

/// Translate the wire filter into the store filter, including the page, so
/// SQLite decides which rows exist before any body is decoded.
fn list_in(store: &dyn FrictionStoreBackend, input: Value) -> Result<Value, OrbitError> {
    let response_mode = optional_string(&input, "response_mode")?;
    let with_notes = match response_mode.as_deref() {
        None => false,
        Some(FRICTION_LIST_RESPONSE_MODE_WITH_NOTES) => true,
        Some(value) => {
            return Err(OrbitError::InvalidInput(format!(
                "`response_mode` must be `{FRICTION_LIST_RESPONSE_MODE_WITH_NOTES}`, got '{value}'"
            )));
        }
    };
    let month_bounds = optional_string(&input, "month")?
        .map(|raw| parse_month_bounds(&raw))
        .transpose()?;
    let (month_from, month_to) = month_bounds
        .map(|(from, to)| (Some(from), Some(to)))
        .unwrap_or((None, None));
    let filter = FrictionListFilter {
        model: optional_string(&input, "model")?,
        status: optional_string(&input, "status")?
            .map(|status| parse_status(&status))
            .transpose()?,
        tag: optional_string(&input, "tag")?,
        q: optional_string(&input, "q")?,
        from: optional_string(&input, "from")?
            .map(|raw| parse_timestamp("from", &raw))
            .transpose()?
            .or(month_from),
        to: optional_string(&input, "to")?
            .map(|raw| parse_timestamp("to", &raw))
            .transpose()?
            .or(month_to),
        limit: optional_usize(&input, "limit")?,
        offset: optional_usize(&input, "offset")?.unwrap_or(0),
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
                .and_then(empty_whitespace_query_note)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        return json!({ "records": records, "notes": notes });
    }
    Value::Array(records)
}

fn show(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    show_in(
        crate::runtime::friction::store_for(runtime)?.as_ref(),
        input,
    )
}

fn show_in(store: &dyn FrictionStoreBackend, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let Some(stored) = store.show(&id)? else {
        return Err(OrbitError::InvalidInput(format!(
            "friction record not found: {id}"
        )));
    };
    record_to_json(stored)
}

fn stats(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    let tasks = runtime.list_tasks()?;
    crate::runtime::friction::store_for(runtime)?.stats(&tasks)
}

fn tags(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    Ok(json!(crate::runtime::friction::store_for(runtime)?.tags()?))
}

fn update(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let status = optional_string(&input, "status")?
        .map(|status| parse_status(&status))
        .transpose()?;
    let tags = optional_csv_or_string_list_alias(&input, &["tags", "tag"])?;
    let body = optional_string(&input, "body")?;
    // An explicit empty `title` clears the stored one, which restores
    // derivation from the body — distinct from omitting the field entirely.
    let title = match optional_raw_string(&input, "title")? {
        None => None,
        Some(raw) if raw.trim().is_empty() => Some(None),
        Some(raw) => Some(Some(normalize_title(&raw)?)),
    };
    if status.is_none() && tags.is_none() && body.is_none() && title.is_none() {
        return Err(OrbitError::InvalidInput(
            "orbit.friction.update requires `status`, `tags`, `body`, or `title`".to_string(),
        ));
    }
    let stored = crate::runtime::friction::store_for(runtime)?.update(
        &id,
        FrictionUpdateParams {
            status,
            tags,
            title,
            body,
            resolved_by_task: None,
            updated_at: Utc::now(),
        },
    )?;
    record_to_json(stored)
}

fn resolve(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let stored = crate::runtime::friction::store_for(runtime)?.resolve(&id, Utc::now())?;
    record_to_json(stored)
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

fn parse_status(raw: &str) -> Result<FrictionStatus, OrbitError> {
    FrictionStatus::from_str(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("`status` {error}")))
}

fn optional_usize(input: &Value, field: &str) -> Result<Option<usize>, OrbitError> {
    let Some(value) = input.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let n = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(raw) => raw.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("`{field}` must be a non-negative integer")))?;
    Ok(Some(n as usize))
}

fn record_to_json(stored: StoredFrictionRecord) -> Result<Value, OrbitError> {
    let mut value = serde_json::to_value(&stored.record)
        .map_err(|error| OrbitError::Store(format!("serialize friction record: {error}")))?;
    if let Some(object) = value.as_object_mut() {
        // ADR-0345: `path` stays on the wire, but it now reports the legacy
        // evidence file an imported record came from and `null` for anything
        // written after the SQLite cutover — never a fabricated location.
        object.insert(
            "path".to_string(),
            match &stored.path {
                Some(path) => json!(path.to_string_lossy()),
                None => Value::Null,
            },
        );
        // `title` is always present on the wire: a record written before the
        // field existed derives one here rather than reaching consumers blank.
        object.insert("title".to_string(), json!(record_title(&stored.record)));
    }
    Ok(value)
}

fn record_title(record: &FrictionRecord) -> String {
    effective_title(record.title.as_deref(), &record.body, &record.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_record(title: Option<&str>, body: &str) -> StoredFrictionRecord {
        StoredFrictionRecord {
            record: FrictionRecord {
                id: "F2026-05-007".to_string(),
                title: title.map(ToString::to_string),
                model: "codex".to_string(),
                created_at: Utc.with_ymd_and_hms(2026, 5, 17, 4, 5, 0).unwrap(),
                status: FrictionStatus::Resolved,
                tags: vec!["tooling".to_string()],
                resolved_at: Some(Utc.with_ymd_and_hms(2026, 5, 17, 4, 10, 0).unwrap()),
                during_task: None,
                resolved_by_task: Some("ORB-00093".to_string()),
                body: body.to_string(),
            },
            path: Some("frictions/2026-05/F007.md".into()),
        }
    }

    #[test]
    fn record_to_json_includes_resolved_by_task() {
        let value = record_to_json(stored_record(None, "Resolved by task")).unwrap();

        assert_eq!(value["resolved_by_task"], json!("ORB-00093"));
    }

    #[test]
    fn record_to_json_prefers_the_stored_title() {
        let value = record_to_json(stored_record(
            Some("Queued runs never reach a worker"),
            "## What happened\n\nSomething else entirely.",
        ))
        .unwrap();

        assert_eq!(value["title"], json!("Queued runs never reach a worker"));
    }

    /// A record written before the field existed still projects a usable
    /// handle, so the corpus needs no rewrite to become readable.
    #[test]
    fn record_to_json_derives_a_title_for_a_record_without_one() {
        let value = record_to_json(stored_record(
            None,
            "## What happened\n\nThe worker exited before claiming the run.\n\n## Evidence\n\nOne log line.",
        ))
        .unwrap();

        assert_eq!(
            value["title"],
            json!("The worker exited before claiming the run.")
        );
    }
}
