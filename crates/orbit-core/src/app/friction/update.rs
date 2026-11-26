//! `update`: update a record's triage metadata, or move it to its owner.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::friction::{normalize_friction_tag_aliases, normalize_title};
use orbit_common::governance::operation::OperationInput;
use orbit_store::contract::FrictionUpdateParams;
use orbit_types::record::FrictionStatus;
use serde_json::Value;

use super::record::{parse_status, record_to_json_with_tag_normalizations, rehome_to_json};
use crate::OrbitRuntime;

pub(super) fn update(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let status = input
        .optional_string("status")?
        .map(|status| parse_status(&status))
        .transpose()?;
    let (tags, substitutions) = match input.optional_string_list("tags")? {
        Some(tags) => {
            let (tags, substitutions) = normalize_friction_tag_aliases(tags);
            (Some(tags), substitutions)
        }
        None => (None, Vec::new()),
    };
    let body = input.optional_string("body")?;
    // An explicit empty `title` clears the stored one, which restores
    // derivation from the body — distinct from omitting the field entirely.
    let title = match input.optional_raw_string("title")? {
        None => None,
        Some(raw) if raw.trim().is_empty() => Some(None),
        Some(raw) => Some(Some(normalize_title(&raw)?)),
    };
    // Like `title`, an explicit empty `rehome_to` clears the disposition.
    let rehome_to = input.optional_raw_string("rehome_to")?.map(|raw| {
        let raw = raw.trim();
        (!raw.is_empty()).then(|| raw.to_string())
    });
    let move_record = input.optional_bool("move")?;
    if move_record.is_some() && !matches!(rehome_to, Some(Some(_))) {
        return Err(OrbitError::InvalidInput(
            "`move` applies only with a non-empty `rehome_to`".to_string(),
        ));
    }
    let edits = FrictionUpdateParams {
        status,
        tags,
        title,
        body,
        resolved_by_task: None,
        rehome_to: None,
        updated_at: Utc::now(),
    };
    let has_edits = edits.status.is_some()
        || edits.tags.is_some()
        || edits.body.is_some()
        || edits.title.is_some();
    // A replica may close its local legacy records with supporting evidence,
    // including a recorded owner disposition, but may not move or reopen them.
    let moving = matches!(rehome_to, Some(Some(_))) && move_record != Some(false);
    if edits.status == Some(FrictionStatus::Resolved) && !moving {
        runtime.ensure_local_friction_resolution_permitted(&id)?;
    } else {
        runtime.ensure_coordination_task_write_permitted()?;
    }
    if let Some(Some(to_workspace)) = &rehome_to
        && move_record != Some(false)
    {
        let outcome = runtime.rehome_friction(&id, to_workspace, has_edits.then_some(edits))?;
        return rehome_to_json(outcome, substitutions);
    }
    let stored = crate::runtime::stored::friction::store_for(runtime)?
        .update(&id, FrictionUpdateParams { rehome_to, ..edits })?;
    record_to_json_with_tag_normalizations(stored, substitutions)
}
