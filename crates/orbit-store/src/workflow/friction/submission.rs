//! Friction submissions shared by ordinary reports and the task commit boundary.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::governance::friction::{derive_title, normalize_friction_tags};
use orbit_common::observability::logging;
use orbit_types::record::{FrictionRecord, FrictionStatus};

use crate::contracts::{FrictionAddParams, StoredFrictionRecord};
use crate::repository::friction::taxonomy::load_tag_taxonomy;
use crate::{Store, Tx};

/// Project a newly committed submission, never an uncommitted allocation,
/// imported record, re-home copy, or replay. Keep the body out of live logs;
/// redact even when the caller installed a subscriber without redacting fields.
pub(crate) fn project_committed_friction(workspace_id: &str, record: &FrictionRecord) {
    let agent = orbit_types::identity::infer_agent_family_from_model(&record.model);
    orbit_common::tracing::warn!(
        target: "orbit.friction.reported",
        workspace_id = %logging::redact_event_text(workspace_id),
        friction_id = %record.id,
        task_id = %logging::redact_event_text(record.during_task.as_deref().unwrap_or_default()),
        agent = agent.as_deref().unwrap_or("unknown"),
        model = %logging::redact_event_text(&record.model),
        summary = %logging::redact_event_text(record.title.as_deref().unwrap_or_default()),
        "friction reported"
    );
}

/// Submit one friction report.
pub fn add(
    store: &Store,
    workspace_id: &str,
    files_root: &Path,
    params: FrictionAddParams,
) -> Result<StoredFrictionRecord, OrbitError> {
    submit(store, workspace_id, files_root, params, |_| Ok(None)).map(|(row, _)| row)
}

/// Submit a friction report or reuse the oldest report with its dedupe marker.
pub fn add_or_reuse(
    store: &Store,
    workspace_id: &str,
    files_root: &Path,
    dedupe_key: &str,
    params: FrictionAddParams,
) -> Result<StoredFrictionRecord, OrbitError> {
    if dedupe_key.is_empty() || dedupe_key.contains(['\n', '\r']) {
        return Err(OrbitError::InvalidInput(
            "friction dedupe key must be a single non-empty line".to_string(),
        ));
    }
    // The trailing newline distinguishes keys with the same prefix.
    let marker = format!("dedupe-key: {dedupe_key}\n");
    if !params.body.contains(&marker) {
        return Err(OrbitError::InvalidInput(
            "friction body must contain its dedupe-key line".to_string(),
        ));
    }
    submit(store, workspace_id, files_root, params, |tx| {
        tx.friction().first_with_body_marker(workspace_id, &marker)
    })
    .map(|(row, _)| row)
}

/// Submit a report if the caller's duplicate check finds no matching row.
/// The check and allocation share one writer transaction. The returned flag
/// identifies a newly created row.
pub fn submit(
    store: &Store,
    workspace_id: &str,
    files_root: &Path,
    mut params: FrictionAddParams,
    existing: impl FnOnce(&Tx<'_>) -> Result<Option<StoredFrictionRecord>, OrbitError>,
) -> Result<(StoredFrictionRecord, bool), OrbitError> {
    normalize_model(&mut params)?;
    params.tags = normalize_friction_tags(params.tags, &load_tag_taxonomy(files_root)?)?;
    let (stored, created) = store.write(|tx| {
        if let Some(row) = existing(tx)? {
            return Ok((row, false));
        }
        let record = allocate(tx, workspace_id, params)?;
        Ok((StoredFrictionRecord { record, path: None }, true))
    })?;
    if created {
        project_committed_friction(workspace_id, &stored.record);
    }
    Ok((stored, created))
}

fn normalize_model(params: &mut FrictionAddParams) -> Result<(), OrbitError> {
    let model = params.model.trim();
    if model.is_empty() {
        return Err(OrbitError::InvalidInput(
            "friction model must not be empty".into(),
        ));
    }
    if model.len() != params.model.len() {
        params.model = model.to_owned();
    }
    Ok(())
}

/// Allocate a row in the caller's transaction. The task journal uses this
/// path so friction publication and its replay receipt commit together.
pub(crate) fn allocate(
    tx: &Tx<'_>,
    workspace_id: &str,
    mut params: FrictionAddParams,
) -> Result<FrictionRecord, OrbitError> {
    normalize_model(&mut params)?;
    let month = params.created_at.format("%Y-%m").to_string();
    let seq = next_friction_sequence(tx, workspace_id, &month)?;
    let record = FrictionRecord {
        id: format!("F{month}-{seq:03}"),
        title: params.title.or_else(|| derive_title(&params.body)),
        model: params.model,
        created_at: params.created_at,
        status: params.status,
        tags: params.tags,
        resolved_at: (params.status == FrictionStatus::Resolved).then_some(params.created_at),
        during_task: params.during_task,
        resolved_by_task: None,
        rehome_to: None,
        body: params.body,
    };
    tx.friction()
        .put(workspace_id, &record, &month, seq, None)?;
    Ok(record)
}

/// Select the next sequence that fits the public `FYYYY-MM-NNN` ID grammar
/// while the caller holds the transaction used to store the record.
pub fn next_friction_sequence(
    tx: &Tx<'_>,
    workspace_id: &str,
    month: &str,
) -> Result<u32, OrbitError> {
    const MAX_MONTH_SEQ: i64 = 999;
    let next = tx.friction().next_month_seq(workspace_id, month)?;
    if next > MAX_MONTH_SEQ {
        return Err(OrbitError::InvalidInput(format!(
            "friction log for {month} is full ({MAX_MONTH_SEQ} records); \
             resolve or consolidate existing records before adding more"
        )));
    }
    u32::try_from(next).map_err(|_| {
        OrbitError::Store(format!(
            "friction counter for workspace '{workspace_id}' month '{month}' overflowed"
        ))
    })
}
