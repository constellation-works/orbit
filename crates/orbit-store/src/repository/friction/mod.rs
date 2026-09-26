//! SQLite-backed friction records (ORB-10680).
//!
//! Friction reads used to discover every Markdown record, parse every YAML
//! envelope and body, build the whole corpus as a `Vec`, and only then filter,
//! sort, paginate, or aggregate — so peak memory grew with retained history
//! even for a 50-row page. This store pushes the filter, the ordering, the
//! page, and every aggregate into SQLite, so a bounded request costs bounded
//! work.
//!
//! Identity is composite `(workspace_id, friction_id)` (L-0072). Friction IDs
//! stay workspace-local and monthly: the same `F2026-05-001` may exist in two
//! workspaces as two unrelated records, and allocation of the next counter
//! happens inside the same write transaction as the insert.
//!
//! The tag taxonomy stays a small YAML file under `files_root` — moving record
//! persistence does not require moving configuration. `files_root` is also the
//! legacy tree [`import`] reads once per workspace; afterwards it is read-only
//! rollback evidence and no file edit can affect a live read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use orbit_common::governance::friction::derive_title;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::{
    all_agent_families, infer_agent_family_from_model, normalize_optional_attribution_label,
    validate_friction_id,
};
use orbit_types::record::{FrictionRecord, FrictionStatus};
use orbit_types::task::{Task, TaskStatus};
use rusqlite::TransactionBehavior;
use serde_json::{Value, json};

use crate::Store;
use crate::driver::file::friction_store::{load_tag_taxonomy, load_tag_taxonomy_with_descriptions};

pub(crate) mod queries;
mod stats;

pub use crate::contracts::{
    FrictionAddParams, FrictionListFilter, FrictionRehomeOutcome, FrictionRehomeParams,
    FrictionReportedCount, FrictionUpdateParams, StoredFrictionRecord,
};

#[cfg(test)]
mod tests;

/// Live friction store for one logical workspace.
///
/// Construction performs no migration. Composition invokes the explicit
/// friction import workflow before returning this live repository.
pub struct FrictionStore {
    store: Store,
    workspace_id: String,
    files_root: PathBuf,
}

impl FrictionStore {
    pub fn open(
        store: Store,
        workspace_id: impl Into<String>,
        files_root: impl Into<PathBuf>,
    ) -> Result<Self, OrbitError> {
        let workspace_id = workspace_id.into();
        let files_root = files_root.into();
        validate_workspace_id(&workspace_id)?;
        Ok(Self {
            store,
            workspace_id,
            files_root,
        })
    }

    #[cfg(test)]
    pub(crate) fn import_report(
        &self,
    ) -> Result<crate::workflow::friction::FrictionImportReport, OrbitError> {
        crate::workflow::friction::import_workspace_frictions(
            &self.store,
            &self.workspace_id,
            &self.files_root,
        )
    }

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

    pub fn list(
        &self,
        filter: &FrictionListFilter,
    ) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
        read_page(&self.store, &self.workspace_id, filter)
    }

    pub fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        validate_friction_id(id)?;
        self.store
            .with_read_connection(|conn| queries::show_record(conn, &self.workspace_id, id))
    }

    /// Other workspaces on this host that already hold `id`.
    ///
    /// Returns IDs only — never another workspace's body — so callers can
    /// refuse an unqualified cross-workspace `resolves` edge without treating
    /// friction IDs as global (ORB-11078).
    pub fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        validate_friction_id(id)?;
        self.store
            .with_read_connection(|conn| queries::foreign_owners_of(conn, &self.workspace_id, id))
    }

    pub fn update(
        &self,
        id: &str,
        params: FrictionUpdateParams,
    ) -> Result<StoredFrictionRecord, OrbitError> {
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
                let mut stored = queries::show_record(conn, &self.workspace_id, id)?
                    .ok_or_else(|| OrbitError::not_found(NotFoundKind::Friction, id))?;
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
                Ok(stored)
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
        self.update(
            id,
            FrictionUpdateParams {
                status: Some(FrictionStatus::Resolved),
                tags: None,
                title: None,
                body: None,
                resolved_by_task: Some(task_id.to_string()),
                rehome_to: None,
                updated_at: resolved_at,
            },
        )
    }

    /// Resolve `id` as a side effect of `task_id` completing, unless someone
    /// already resolved it. A task's `resolves` relation is a claim, not an
    /// override: an existing resolution (and the task it names) is kept and
    /// returned untouched. `Ok(None)` means no such record exists locally.
    pub fn auto_resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        let Some(stored) = self.show(id)? else {
            return Ok(None);
        };
        if stored.record.status == FrictionStatus::Resolved {
            return Ok(Some(stored));
        }
        self.resolve_by_task(id, task_id, resolved_at).map(Some)
    }

    pub fn tags(&self) -> Result<Vec<String>, OrbitError> {
        Ok(load_tag_taxonomy(&self.files_root)?.into_iter().collect())
    }

    pub fn tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError> {
        Ok(load_tag_taxonomy_with_descriptions(&self.files_root)?
            .into_iter()
            .collect())
    }

    /// Friction counts by reporting model over an optional window, for the
    /// scoreboard. Bounded by distinct model labels.
    pub fn reported_by_model(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<FrictionReportedCount>, OrbitError> {
        self.store
            .with_read_connection(|conn| stats::counts_by_model(conn, &self.workspace_id, since))
    }

    /// The `orbit.friction.stats` projection, computed entirely from SQL
    /// aggregates plus the caller's task attribution.
    pub fn stats(&self, tasks: &[Task]) -> Result<Value, OrbitError> {
        let now = Utc::now();
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .ok_or_else(|| OrbitError::Store("compute current friction month".to_string()))?;
        let (next_year, next_month) = if now.month() == 12 {
            (now.year() + 1, 1)
        } else {
            (now.year(), now.month() + 1)
        };
        let next_month_start = Utc
            .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
            .single()
            .ok_or_else(|| OrbitError::Store("compute next friction month".to_string()))?;

        let (counts, resolved_this_month, by_model, by_tag_model) =
            self.store.with_read_connection(|conn| {
                Ok((
                    stats::status_counts(conn, &self.workspace_id)?,
                    stats::resolved_in_window(
                        conn,
                        &self.workspace_id,
                        month_start,
                        next_month_start,
                    )?,
                    stats::counts_by_model(conn, &self.workspace_id, None)?,
                    stats::counts_by_tag_and_model(conn, &self.workspace_id)?,
                ))
            })?;

        let tasks_done = completed_tasks_by_family(tasks);
        let mut frictions_by_family: BTreeMap<String, u64> = BTreeMap::new();
        let mut families = BTreeSet::new();
        for entry in &by_model {
            let family = friction_family_key(&entry.model);
            families.insert(family.clone());
            *frictions_by_family.entry(family).or_insert(0) += entry.count;
        }
        let mut frictions_by_tag_family: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for (tag, model, count) in by_tag_model {
            let family = friction_family_key(&model);
            families.insert(family.clone());
            *frictions_by_tag_family
                .entry(tag)
                .or_default()
                .entry(family)
                .or_insert(0) += count;
        }
        families.extend(tasks_done.keys().cloned());
        families.extend(known_family_keys());

        let mut by_family = serde_json::Map::new();
        for family in &families {
            let frictions = frictions_by_family.get(family).copied().unwrap_or(0);
            let done = tasks_done.get(family).copied().unwrap_or(0);
            by_family.insert(family.clone(), rate_row(frictions, done));
        }

        let mut by_tag = serde_json::Map::new();
        for (tag, by_family_counts) in frictions_by_tag_family {
            let mut tag_map = serde_json::Map::new();
            for family in &families {
                let frictions = by_family_counts.get(family).copied().unwrap_or(0);
                let done = tasks_done.get(family).copied().unwrap_or(0);
                tag_map.insert(family.clone(), rate_row(frictions, done));
            }
            by_tag.insert(tag, Value::Object(tag_map));
        }

        Ok(json!({
            "total": counts.total(),
            "open": counts.get("open"),
            "triaged": counts.get("triaged"),
            "resolved": counts.get("resolved"),
            "resolved_this_month": resolved_this_month,
            "by_family": Value::Object(by_family),
            "by_tag": Value::Object(by_tag),
        }))
    }
}

/// Shared read entry point so the export workflow pages
/// through the same bounded query the tool surfaces use.
pub(crate) fn read_page(
    store: &Store,
    workspace_id: &str,
    filter: &FrictionListFilter,
) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
    store.with_read_connection(|conn| queries::list_records(conn, workspace_id, filter))
}

fn split_friction_id(id: &str) -> Option<(String, u32)> {
    orbit_types::identity::validate_friction_id(id).ok()?;
    let month = id.get(1..8)?.to_string();
    let seq = id.get(9..12)?.parse::<u32>().ok()?;
    (seq > 0).then_some((month, seq))
}

impl crate::contracts::FrictionStoreBackend for FrictionStore {
    fn add(&self, params: FrictionAddParams) -> Result<StoredFrictionRecord, OrbitError> {
        Self::add(self, params)
    }

    fn list(&self, filter: &FrictionListFilter) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
        Self::list(self, filter)
    }

    fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        Self::show(self, id)
    }

    fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        Self::foreign_owners_of(self, id)
    }

    fn update(
        &self,
        id: &str,
        params: FrictionUpdateParams,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::update(self, id, params)
    }

    fn rehome(
        &self,
        id: &str,
        params: FrictionRehomeParams,
    ) -> Result<FrictionRehomeOutcome, OrbitError> {
        Self::rehome(self, id, params)
    }

    fn resolve(
        &self,
        id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::resolve(self, id, resolved_at)
    }

    fn resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::resolve_by_task(self, id, task_id, resolved_at)
    }

    fn auto_resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        Self::auto_resolve_by_task(self, id, task_id, resolved_at)
    }

    fn tags(&self) -> Result<Vec<String>, OrbitError> {
        Self::tags(self)
    }

    fn tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError> {
        Self::tag_taxonomy(self)
    }

    fn reported_by_model(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<FrictionReportedCount>, OrbitError> {
        Self::reported_by_model(self, since)
    }

    fn stats(&self, tasks: &[Task]) -> Result<Value, OrbitError> {
        Self::stats(self, tasks)
    }
}

fn validate_workspace_id(workspace_id: &str) -> Result<(), OrbitError> {
    if workspace_id.trim().is_empty() || workspace_id.trim() != workspace_id {
        return Err(OrbitError::InvalidInput(format!(
            "invalid workspace ID '{workspace_id}' for friction records"
        )));
    }
    Ok(())
}

use crate::contracts::normalize_friction_tags as normalize_and_validate_tags;

fn completed_tasks_by_family(tasks: &[Task]) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for task in tasks {
        if !matches!(task.status, TaskStatus::Done | TaskStatus::Archived) {
            continue;
        }
        let Some(model) = normalize_optional_attribution_label(
            task.implemented_by.as_deref(),
            task.implemented_by.as_deref(),
        ) else {
            continue;
        };
        *counts.entry(friction_family_key(&model)).or_insert(0) += 1;
    }
    counts
}

pub(crate) fn friction_family_key(value: &str) -> String {
    let normalized = normalize_optional_attribution_label(Some(value), None).unwrap_or_default();
    infer_agent_family_from_model(&normalized).unwrap_or(normalized)
}

fn known_family_keys() -> impl Iterator<Item = String> {
    all_agent_families()
        .into_iter()
        .map(|family| family.to_string())
}

fn rate_row(frictions: u64, tasks_done: u64) -> Value {
    let rate = if tasks_done == 0 {
        json!("n/a")
    } else {
        let raw = (frictions as f64) * 10.0 / (tasks_done as f64);
        json!((raw * 10.0).round() / 10.0)
    };
    json!({
        "frictions": frictions,
        "tasks_done": tasks_done,
        "frictions_per_10_tasks": rate,
    })
}
