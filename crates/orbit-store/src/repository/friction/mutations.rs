//! Friction add, update, re-home and resolve mutations.

use chrono::{DateTime, Utc};
use orbit_common::governance::friction::derive_title;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::validate_friction_id;
use orbit_types::record::{FrictionRecord, FrictionStatus};
use rusqlite::TransactionBehavior;

use super::store::validate_workspace_id;
use super::{
    FrictionAddParams, FrictionRehomeOutcome, FrictionRehomeParams, FrictionStore,
    FrictionUpdateParams, StoredFrictionRecord, queries,
};
use crate::contracts::normalize_friction_tags as normalize_and_validate_tags;
use crate::driver::file::friction_store::load_tag_taxonomy;

impl FrictionStore {
    pub fn add(&self, params: FrictionAddParams) -> Result<StoredFrictionRecord, OrbitError> {
        let model = params.model.trim().to_string();
        if model.is_empty() {
            return Err(OrbitError::InvalidInput(
                "friction model must not be empty".to_string(),
            ));
        }
        // Taxonomy load is file I/O; keep it outside the write transaction.
        let taxonomy = load_tag_taxonomy(&self.files_root)?;
        let tags = normalize_and_validate_tags(params.tags, &taxonomy)?;
        let month = params.created_at.format("%Y-%m").to_string();
        // Derivation runs here, not on read, so every new record carries an
        // explicit handle its next reader can see and correct.
        let title = params.title.clone().or_else(|| derive_title(&params.body));

        self.store
            .with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let conn = tx.connection();
                let seq = queries::next_month_seq(conn, &self.workspace_id, &month)?;
                let record = FrictionRecord {
                    id: format!("F{month}-{seq:03}"),
                    title,
                    model,
                    created_at: params.created_at,
                    status: FrictionStatus::Open,
                    tags,
                    resolved_at: None,
                    during_task: params.during_task,
                    resolved_by_task: None,
                    rehome_to: None,
                    body: params.body,
                };
                queries::upsert_record(conn, &self.workspace_id, &record, &month, seq, None)?;
                Ok(StoredFrictionRecord { record, path: None })
            })
    }

    pub fn update(
        &self,
        id: &str,
        params: FrictionUpdateParams,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        self.update_unless(id, params, |_| false)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Friction, id))
    }

    /// Apply `params` to `id` unless `keep` accepts the record as it stands
    /// under the write lock, in which case it is returned untouched.
    /// `Ok(None)` means no such record exists. The check and the write share
    /// one `BEGIN IMMEDIATE` transaction, so no other writer can land between
    /// them.
    fn update_unless(
        &self,
        id: &str,
        params: FrictionUpdateParams,
        keep: impl FnOnce(&StoredFrictionRecord) -> bool,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        validate_friction_id(id)?;
        let taxonomy = match params.tags {
            Some(_) => Some(load_tag_taxonomy(&self.files_root)?),
            None => None,
        };
        let (month, seq) = split_friction_id(id)
            .ok_or_else(|| OrbitError::InvalidInput(format!("malformed friction id: {id}")))?;

        self.store
            .with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let conn = tx.connection();
                let Some(mut stored) = queries::show_record(conn, &self.workspace_id, id)? else {
                    return Ok(None);
                };
                if keep(&stored) {
                    return Ok(Some(stored));
                }
                if let (Some(tags), Some(taxonomy)) = (params.tags.clone(), taxonomy.as_ref()) {
                    stored.record.tags = normalize_and_validate_tags(tags, taxonomy)?;
                }
                if let Some(title) = params.title.clone() {
                    stored.record.title = title;
                }
                if let Some(body) = params.body.clone() {
                    stored.record.body = body;
                }
                if let Some(status) = params.status {
                    stored.record.status = status;
                    stored.record.resolved_at = match status {
                        FrictionStatus::Resolved => {
                            Some(stored.record.resolved_at.unwrap_or(params.updated_at))
                        }
                        FrictionStatus::Open | FrictionStatus::Triaged => {
                            stored.record.resolved_by_task = None;
                            None
                        }
                    };
                }
                // Unlike `resolved_at`, which keeps the first resolution
                // instant, the resolving task is whatever the caller names:
                // re-resolving against a corrected task must not silently
                // keep the wrong one.
                if let Some(resolved_by_task) = params.resolved_by_task.clone() {
                    stored.record.resolved_by_task = Some(resolved_by_task);
                }
                if let Some(rehome_to) = params.rehome_to.clone() {
                    stored.record.rehome_to = rehome_to;
                }
                queries::upsert_record(
                    conn,
                    &self.workspace_id,
                    &stored.record,
                    &month,
                    seq,
                    stored
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy())
                        .as_deref(),
                )?;
                Ok(Some(stored))
            })
    }

    /// Move `id` into the workspace that owns it.
    ///
    /// One transaction: the owning workspace gains a copy under an ID it
    /// allocates — same title, reporter, creation time, task, triage status,
    /// and body, plus a provenance note — and the source is resolved with
    /// `rehome_to` set and a forwarding note naming the new ID. Tags the
    /// owning taxonomy does not define are dropped and reported, never
    /// invented. A resolved record has nothing left to move.
    pub fn rehome(
        &self,
        id: &str,
        params: FrictionRehomeParams,
    ) -> Result<FrictionRehomeOutcome, OrbitError> {
        validate_friction_id(id)?;
        validate_workspace_id(&params.target_workspace_id)?;
        if params.target_workspace_id == self.workspace_id {
            return Err(OrbitError::InvalidInput(format!(
                "friction {id} already belongs to workspace '{}'; re-home needs another workspace",
                params.target_label
            )));
        }
        let (month, seq) = split_friction_id(id)
            .ok_or_else(|| OrbitError::InvalidInput(format!("malformed friction id: {id}")))?;
        // Taxonomy load is file I/O; keep it outside the write transaction.
        let target_taxonomy = load_tag_taxonomy(&params.target_files_root)?;
        let at = params.rehomed_at;
        let stamp = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        self.store
            .with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let conn = tx.connection();
                let mut source = queries::show_record(conn, &self.workspace_id, id)?
                    .ok_or_else(|| OrbitError::not_found(NotFoundKind::Friction, id))?;
                if source.record.status == FrictionStatus::Resolved {
                    return Err(OrbitError::InvalidInput(format!(
                        "friction {id} is already resolved; there is nothing to re-home"
                    )));
                }

                let (kept, dropped_tags): (Vec<String>, Vec<String>) = source
                    .record
                    .tags
                    .iter()
                    .cloned()
                    .partition(|tag| target_taxonomy.contains(tag));
                let tags = normalize_and_validate_tags(kept, &target_taxonomy)?;
                let target_month = source.record.created_at.format("%Y-%m").to_string();
                let target_seq =
                    queries::next_month_seq(conn, &params.target_workspace_id, &target_month)?;
                let target_id = format!("F{target_month}-{target_seq:03}");
                let target = FrictionRecord {
                    id: target_id.clone(),
                    title: source.record.title.clone(),
                    model: source.record.model.clone(),
                    created_at: source.record.created_at,
                    status: source.record.status,
                    tags,
                    resolved_at: None,
                    during_task: source.record.during_task.clone(),
                    resolved_by_task: None,
                    rehome_to: None,
                    body: format!(
                        "{}\n\n---\n\nRe-homed from workspace `{}` friction `{id}` on {stamp}.",
                        source.record.body.trim_end(),
                        params.source_label,
                    ),
                };
                queries::upsert_record(
                    conn,
                    &params.target_workspace_id,
                    &target,
                    &target_month,
                    target_seq,
                    None,
                )?;

                source.record.body = format!(
                    "{}\n\n---\n\nRe-homed to workspace `{}` as friction `{target_id}` on {stamp}; \
                     track it there.",
                    source.record.body.trim_end(),
                    params.target_label,
                );
                source.record.status = FrictionStatus::Resolved;
                source.record.resolved_at = Some(at);
                source.record.rehome_to = Some(params.target_label.clone());
                queries::upsert_record(
                    conn,
                    &self.workspace_id,
                    &source.record,
                    &month,
                    seq,
                    source
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy())
                        .as_deref(),
                )?;

                Ok(FrictionRehomeOutcome {
                    source,
                    target: StoredFrictionRecord {
                        record: target,
                        path: None,
                    },
                    dropped_tags,
                })
            })
    }

    pub fn resolve(
        &self,
        id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        self.update(
            id,
            FrictionUpdateParams {
                status: Some(FrictionStatus::Resolved),
                tags: None,
                title: None,
                body: None,
                resolved_by_task: None,
                rehome_to: None,
                updated_at: resolved_at,
            },
        )
    }

    pub fn resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        self.update(id, resolve_by_task_params(task_id, resolved_at))
    }

    /// Resolve `id` as a side effect of `task_id` completing, unless someone
    /// already resolved it. A task's `resolves` relation is a claim, not an
    /// override: an existing resolution (and the task it names) is kept and
    /// returned untouched. The check happens under the write lock, so a
    /// resolution committed by a concurrent writer is never overwritten.
    /// `Ok(None)` means no such record exists locally.
    pub fn auto_resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        self.update_unless(id, resolve_by_task_params(task_id, resolved_at), |stored| {
            stored.record.status == FrictionStatus::Resolved
        })
    }
}

fn split_friction_id(id: &str) -> Option<(String, u32)> {
    orbit_types::identity::validate_friction_id(id).ok()?;
    let month = id.get(1..8)?.to_string();
    let seq = id.get(9..12)?.parse::<u32>().ok()?;
    (seq > 0).then_some((month, seq))
}

fn resolve_by_task_params(task_id: &str, resolved_at: DateTime<Utc>) -> FrictionUpdateParams {
    FrictionUpdateParams {
        status: Some(FrictionStatus::Resolved),
        tags: None,
        title: None,
        body: None,
        resolved_by_task: Some(task_id.to_string()),
        rehome_to: None,
        updated_at: resolved_at,
    }
}
