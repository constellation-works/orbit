//! The wire form of a friction record, and the status names the flows share.

use std::str::FromStr;

use orbit_common::OrbitError;
use orbit_common::governance::friction::effective_title;
use orbit_store::contract::{FrictionRehomeOutcome, StoredFrictionRecord};
use orbit_types::record::{FrictionRecord, FrictionStatus};
use serde_json::{Value, json};

/// The resolved source record, with the owning workspace's copy as
/// `rehomed_as` and any tags the target taxonomy lacks as `dropped_tags`.
pub(super) fn rehome_to_json(
    outcome: FrictionRehomeOutcome,
    substitutions: Vec<(String, String)>,
) -> Result<Value, OrbitError> {
    let mut value = record_to_json_with_tag_normalizations(outcome.source, substitutions)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("rehomed_as".to_string(), record_to_json(outcome.target)?);
        if !outcome.dropped_tags.is_empty() {
            object.insert("dropped_tags".to_string(), json!(outcome.dropped_tags));
        }
    }
    Ok(value)
}

pub(super) fn parse_status(raw: &str) -> Result<FrictionStatus, OrbitError> {
    FrictionStatus::from_str(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("`status` {error}")))
}

pub(super) fn record_to_json(stored: StoredFrictionRecord) -> Result<Value, OrbitError> {
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

pub(crate) fn record_to_json_with_tag_normalizations(
    stored: StoredFrictionRecord,
    substitutions: Vec<(String, String)>,
) -> Result<Value, OrbitError> {
    let mut value = record_to_json(stored)?;
    if !substitutions.is_empty()
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "tag_normalizations".to_string(),
            json!(
                substitutions
                    .into_iter()
                    .map(|(input, stored)| json!({ "input": input, "stored": stored }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(value)
}

fn record_title(record: &FrictionRecord) -> String {
    effective_title(record.title.as_deref(), &record.body, &record.id)
}
