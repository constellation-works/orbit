//! Fleet notices: short, expiring, attributed announcements every agent and
//! operator can read, stored in the owner's store.
//!
//! This module owns every decision: who is posting (`authority`), what a
//! post may set and whether it creates, updates or corroborates (`post`),
//! the effective state a read reports, and the audit row each write and each
//! refusal leaves (target type `notice`).

mod authority;
mod post;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_store::contracts::{NoticeEventRow, NoticeRow, NoticeStoreBackend as _};
use orbit_types::record::{
    AuthorClass, EffectiveStatus, Notice, NoticeAuthor, NoticeCloseCause, NoticeClosed, NoticeKind,
    NoticeScope,
};
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use serde::Serialize;

pub use authority::NoticeCaller;
pub use post::{NoticePostRequest, NoticePostResult};

use crate::OrbitRuntime;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};

/// The longest close reason.
const MAX_CLOSE_REASON_CHARS: usize = 240;

/// Which notice a close names.
#[derive(Debug, Clone)]
pub enum NoticeSelector {
    Id(String),
    /// The open notice of the home workspace holding this key.
    DedupeKey(String),
}

/// What a close did.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeCloseResult {
    pub notice: Notice,
    /// The notice was already closed or expired; nothing was written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub already_inactive: bool,
}

/// Effective-state filter for a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeStatusFilter {
    #[default]
    Open,
    Closed,
    Expired,
    All,
}

impl NoticeStatusFilter {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "expired" => Some(Self::Expired),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    fn admits(self, status: EffectiveStatus) -> bool {
        match self {
            Self::Open => status == EffectiveStatus::Open,
            Self::Closed => status == EffectiveStatus::Closed,
            Self::Expired => status == EffectiveStatus::Expired,
            Self::All => true,
        }
    }
}

/// A listing's filters.
#[derive(Debug, Clone)]
pub struct NoticeListFilter {
    pub status: NoticeStatusFilter,
    pub include_global: bool,
    pub scope: Option<NoticeScope>,
    pub kinds: Vec<NoticeKind>,
    pub linked_task: Option<String>,
    pub author_class: Option<AuthorClass>,
    pub host: Option<String>,
    pub limit: usize,
}

impl Default for NoticeListFilter {
    fn default() -> Self {
        Self {
            status: NoticeStatusFilter::Open,
            include_global: true,
            scope: None,
            kinds: Vec::new(),
            linked_task: None,
            author_class: None,
            host: None,
            limit: 50,
        }
    }
}

/// A listing: the matching notices and the homes that could not be read.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeListing {
    pub notices: Vec<Notice>,
    pub degraded: Vec<String>,
}

/// One history entry of a notice, shown to the operator.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeHistoryEntry {
    pub at: DateTime<Utc>,
    pub action: String,
    pub poster_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_crew: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_task_id: Option<String>,
    pub detail: serde_json::Value,
}

/// One notice, with its history when the caller holds the operator
/// capability.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeView {
    pub notice: Notice,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<NoticeHistoryEntry>>,
}

/// The audit row a notice write or refusal leaves.
pub(crate) struct NoticeAudit {
    verb: &'static str,
    target: Option<String>,
    poster: Option<AuthorClass>,
    detail: serde_json::Value,
}

impl NoticeAudit {
    fn new(verb: &'static str, target: Option<String>) -> Self {
        Self {
            verb,
            target,
            poster: None,
            detail: serde_json::json!({}),
        }
    }

    /// Record the outcome. An audit failure is logged, never raised: it
    /// must not turn a committed write into a reported failure.
    fn record(mut self, runtime: &OrbitRuntime, result: &Result<String, &OrbitError>) {
        let status = match result {
            Ok(id) => {
                self.target = Some(id.clone());
                AuditEventStatus::Success
            }
            Err(error) => {
                if let Some(object) = self.detail.as_object_mut() {
                    object.insert("error".into(), serde_json::json!(error.to_string()));
                }
                if error.is_policy_denial()
                    || matches!(
                        error,
                        OrbitError::NoticeRefused { .. } | OrbitError::CapabilityRefused(_)
                    )
                {
                    AuditEventStatus::Denied
                } else {
                    AuditEventStatus::Failure
                }
            }
        };
        if let Some(object) = self.detail.as_object_mut() {
            object.insert(
                "poster_class".into(),
                serde_json::json!(self.poster.map(AuthorClass::as_str)),
            );
        }
        let tool_name = format!("orbit.notice.{}", self.verb);
        let workspace_id = runtime.workspace_id().ok();
        if let Err(error) = record_system_audit_event(
            runtime,
            SystemAuditEvent {
                execution_id_prefix: "audit-notice",
                command: "notice",
                subcommand: Some(self.verb),
                tool_name: Some(&tool_name),
                target_type: "notice",
                target_id: self.target.as_deref(),
                role: self.poster.map_or("unidentified", AuthorClass::as_str),
                status,
                arguments_json: self.detail.to_string(),
                workspace_id: workspace_id.as_deref(),
                task_id: None,
                attribution: AuditRunAttribution::Environment { run_id: None },
            },
        ) {
            tracing::warn!(%error, verb = self.verb, "failed to record the notice audit row");
        }
    }
}

/// The stored row of a notice.
pub(crate) fn notice_row(notice: &Notice) -> Result<NoticeRow, OrbitError> {
    Ok(NoticeRow {
        workspace_id: notice.workspace.id.clone(),
        id: notice.id.clone(),
        scope: notice.scope.as_str().to_string(),
        kind: notice.kind.as_str().to_string(),
        dedupe_key: notice.dedupe_key.clone(),
        author_class: notice.author.class.as_str().to_string(),
        author_crew: notice.author.crew.clone(),
        author_task_id: notice.author.task_id.clone(),
        linked_task_id: notice.linked_task.as_ref().map(|task| task.id.clone()),
        close_on_linked_done: notice.close_on_linked_done,
        expires_at: notice.expires_at,
        opened_at: notice.opened_at,
        updated_at: notice.updated_at,
        closed_at: notice.closed.as_ref().map(|closed| closed.at),
        revision: notice.revision,
        record_json: serde_json::to_string(notice)
            .map_err(|error| OrbitError::Execution(format!("serialize notice: {error}")))?,
    })
}

/// The notice a stored row holds.
pub(crate) fn parse_notice_row(row: &NoticeRow) -> Result<Notice, OrbitError> {
    let mut notice: Notice = serde_json::from_str(&row.record_json)
        .map_err(|error| OrbitError::Store(format!("notice {} is unreadable: {error}", row.id)))?;
    if let Some(linked) = notice.linked_task.as_mut() {
        linked.status = None;
    }
    Ok(notice)
}

fn refuse(code: NoticeRefusalCode, message: impl Into<String>) -> OrbitError {
    OrbitError::notice_refused(code, message)
}

impl OrbitRuntime {
    /// The registered name of this workspace, or its checkout label when the
    /// registry cannot be read.
    pub(crate) fn notice_workspace_name(&self, workspace_id: &str) -> String {
        let path = orbit_registry::workspace_registry::registry_path_for(&self.global_root());
        orbit_registry::workspace_registry::load_registry_from_read_only(&path)
            .ok()
            .and_then(|load| {
                orbit_registry::workspace_registry::find_workspace_by_id(
                    &load.registry,
                    workspace_id,
                )
                .map(|workspace| workspace.name.clone())
            })
            .unwrap_or_else(|| self.workspace_label())
    }

    /// The linked tasks of `rows` that are done, with when they last
    /// changed, read from this workspace.
    pub(crate) fn linked_done_tasks(&self, rows: &[NoticeRow]) -> LinkedDone {
        rows.iter()
            .filter(|row| row.closed_at.is_none() && row.close_on_linked_done)
            .filter_map(|row| row.linked_task_id.as_deref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let task = self.get_task(id).ok()?;
                (task.status == TaskStatus::Done).then(|| (id.to_string(), task.updated_at))
            })
            .collect()
    }

    /// Close a notice for `caller`, recording an audit row for the write or
    /// the refusal.
    pub fn close_notice(
        &self,
        caller: &NoticeCaller,
        selector: NoticeSelector,
        reason: &str,
    ) -> Result<NoticeCloseResult, OrbitError> {
        let mut audit = NoticeAudit::new(
            "close",
            match &selector {
                NoticeSelector::Id(id) => Some(id.clone()),
                NoticeSelector::DedupeKey(_) => None,
            },
        );
        audit.detail = serde_json::json!({ "reason": reason });
        if let NoticeSelector::DedupeKey(key) = &selector {
            audit.detail["dedupe_key"] = serde_json::json!(key);
        }
        let result = self.close_notice_unaudited(caller, selector, reason, &mut audit);
        if result.as_ref().is_ok_and(|closed| closed.already_inactive) {
            // Nothing was written, so there is no write to audit.
            return result;
        }
        audit.record(
            self,
            &result.as_ref().map(|closed| closed.notice.id.clone()),
        );
        result
    }

    fn close_notice_unaudited(
        &self,
        caller: &NoticeCaller,
        selector: NoticeSelector,
        reason: &str,
        audit: &mut NoticeAudit,
    ) -> Result<NoticeCloseResult, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let poster = self.resolve_notice_poster(caller)?;
        audit.poster = Some(poster.class);
        let reason = reason.trim();
        if reason.is_empty()
            || reason.chars().count() > MAX_CLOSE_REASON_CHARS
            || reason.chars().any(char::is_control)
        {
            return Err(refuse(
                NoticeRefusalCode::InvalidField,
                format!("`reason` is 1 to {MAX_CLOSE_REASON_CHARS} characters of single-line text"),
            ));
        }
        let store = self.sqlite_store()?;
        let workspace_id = self.workspace_id()?;
        let now = Utc::now();
        let row = match &selector {
            NoticeSelector::Id(id) => store.notice_row(id)?,
            NoticeSelector::DedupeKey(key) => {
                let rows = store.notice_rows(Some(std::slice::from_ref(&workspace_id)), false)?;
                let done = self.linked_done_tasks(&rows);
                rows.into_iter()
                    .filter(|row| row.dedupe_key == *key)
                    .find(|row| {
                        let mut notice = parse_notice_row(row).ok();
                        notice.as_mut().is_some_and(|notice| {
                            notice.resolve_effective_status(now, linked_done_at(row, &done));
                            notice.status == EffectiveStatus::Open
                        })
                    })
            }
        };
        let row = row.ok_or_else(|| {
            refuse(
                NoticeRefusalCode::NotFound,
                "no such notice in this workspace",
            )
        })?;
        audit.target = Some(row.id.clone());
        let mut notice = parse_notice_row(&row)?;
        let done = self.linked_done_tasks(std::slice::from_ref(&row));
        notice.resolve_effective_status(now, linked_done_at(&row, &done));
        if notice.status != EffectiveStatus::Open {
            return Ok(NoticeCloseResult {
                notice,
                already_inactive: true,
            });
        }
        if !poster.may_edit(&row.author_class, row.author_task_id.as_deref()) {
            return Err(refuse(
                NoticeRefusalCode::CloseRefused,
                format!(
                    "this {} may not close a notice posted by an {}",
                    poster.class.as_str(),
                    row.author_class
                ),
            ));
        }
        notice.closed = Some(NoticeClosed {
            at: now,
            by: poster.author(),
            reason: Some(reason.to_string()),
            cause: NoticeCloseCause::Manual,
        });
        notice.status = EffectiveStatus::Closed;
        let event = NoticeEventRow {
            workspace_id: row.workspace_id.clone(),
            notice_id: row.id.clone(),
            at: now,
            action: "closed".into(),
            poster_class: poster.class.as_str().to_string(),
            poster_crew: poster.crew.clone(),
            poster_task_id: poster.task_id.clone(),
            detail_json: serde_json::json!({ "reason": reason }).to_string(),
        };
        if !store.close_notice(&notice_row(&notice)?, row.revision, &event)? {
            return Err(OrbitError::Execution(format!(
                "notice {} changed while closing it; read it again",
                row.id
            )));
        }
        Ok(NoticeCloseResult {
            notice,
            already_inactive: false,
        })
    }

    /// The notices of this workspace (and global ones) matching `filter`,
    /// in listing order.
    pub fn list_notices(&self, filter: &NoticeListFilter) -> Result<NoticeListing, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let store = self.sqlite_store()?;
        let rows = store.notice_rows(
            Some(std::slice::from_ref(&workspace_id)),
            filter.include_global,
        )?;
        let now = Utc::now();
        let retention = Duration::days(i64::from(self.notice_settings().retention_days));
        let done = self.linked_done_tasks(&rows);
        let mut notices = Vec::new();
        for row in &rows {
            let mut notice = parse_notice_row(row)?;
            notice.resolve_effective_status(now, linked_done_at(row, &done));
            let inactive_since = match notice.status {
                EffectiveStatus::Open => None,
                EffectiveStatus::Closed => notice.closed.as_ref().map(|closed| closed.at),
                EffectiveStatus::Expired => Some(notice.expires_at),
            };
            if inactive_since.is_some_and(|at| at < now - retention)
                || !filter.status.admits(notice.status)
                || filter.scope.is_some_and(|scope| scope != notice.scope)
                || (!filter.kinds.is_empty() && !filter.kinds.contains(&notice.kind))
                || filter.linked_task.as_deref().is_some_and(|task| {
                    notice
                        .linked_task
                        .as_ref()
                        .is_none_or(|linked| !linked.id.eq_ignore_ascii_case(task))
                })
                || filter
                    .author_class
                    .is_some_and(|class| class != notice.author.class)
                || filter.host.as_deref().is_some_and(|host| {
                    notice.kind != NoticeKind::Maintenance
                        || !notice.hosts.iter().any(|named| named == host)
                })
            {
                continue;
            }
            self.fill_linked_status(&mut notice);
            notices.push(notice);
        }
        notices.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        notices.truncate(filter.limit.clamp(1, 200));
        Ok(NoticeListing {
            notices,
            degraded: Vec::new(),
        })
    }

    /// One notice in any effective state; the history is included for the
    /// operator capability.
    pub fn show_notice(&self, id: &str, with_history: bool) -> Result<NoticeView, OrbitError> {
        let store = self.sqlite_store()?;
        let row = store
            .notice_row(id)?
            .ok_or_else(|| refuse(NoticeRefusalCode::NotFound, "no such notice"))?;
        let workspace_id = self.workspace_id()?;
        if row.workspace_id != workspace_id && row.scope != "global" {
            return Err(refuse(NoticeRefusalCode::NotFound, "no such notice"));
        }
        let now = Utc::now();
        let mut notice = parse_notice_row(&row)?;
        let done = self.linked_done_tasks(std::slice::from_ref(&row));
        notice.resolve_effective_status(now, linked_done_at(&row, &done));
        self.fill_linked_status(&mut notice);
        let history = if with_history {
            Some(
                store
                    .notice_events(&row.id)?
                    .into_iter()
                    .map(|event| NoticeHistoryEntry {
                        at: event.at,
                        action: event.action,
                        poster_class: event.poster_class,
                        poster_crew: event.poster_crew,
                        poster_task_id: event.poster_task_id,
                        detail: serde_json::from_str(&event.detail_json)
                            .unwrap_or(serde_json::Value::Null),
                    })
                    .collect(),
            )
        } else {
            None
        };
        Ok(NoticeView { notice, history })
    }

    fn fill_linked_status(&self, notice: &mut Notice) {
        if let Some(linked) = notice.linked_task.as_mut()
            && let Ok(task) = self.get_task(&linked.id)
        {
            linked.status = Some(task.status.to_string());
        }
    }

    /// Store the closure of every open notice linked to `task_id`, now that
    /// the task is done. Reads already report these notices closed; this
    /// makes the closure durable and frees their dedupe keys. Best effort: a
    /// failure is logged, never raised into the task update.
    pub(crate) fn materialize_notice_closures_for_done_task(&self, task_id: &str) {
        if let Err(error) = self.materialize_notice_closures(task_id) {
            tracing::warn!(task = task_id, %error, "failed to close notices linked to a done task");
        }
    }

    fn materialize_notice_closures(&self, task_id: &str) -> Result<(), OrbitError> {
        if self.coordination_write_owner().is_some() {
            return Ok(());
        }
        let store = self.sqlite_store()?;
        let now = Utc::now();
        for row in store.notices_linked_to(task_id)? {
            if !row.close_on_linked_done {
                continue;
            }
            let mut notice = parse_notice_row(&row)?;
            notice.closed = Some(NoticeClosed {
                at: now,
                by: NoticeAuthor::system(),
                reason: None,
                cause: NoticeCloseCause::LinkedTaskDone,
            });
            notice.status = EffectiveStatus::Closed;
            let event = NoticeEventRow {
                workspace_id: row.workspace_id.clone(),
                notice_id: row.id.clone(),
                at: now,
                action: "linked_task_done".into(),
                poster_class: AuthorClass::System.as_str().to_string(),
                poster_crew: None,
                poster_task_id: None,
                detail_json: serde_json::json!({ "task": task_id }).to_string(),
            };
            if store.close_notice(&notice_row(&notice)?, row.revision, &event)? {
                let mut audit = NoticeAudit::new("close", Some(row.id.clone()));
                audit.poster = Some(AuthorClass::System);
                audit.detail = serde_json::json!({ "cause": "linked_task_done", "task": task_id });
                audit.record(self, &Ok(row.id.clone()));
            }
        }
        Ok(())
    }
}

/// Done linked tasks by id, with the time each was last updated.
pub(crate) type LinkedDone = BTreeMap<String, DateTime<Utc>>;

/// When a row's linked task became done, for its effective state.
fn linked_done_at(row: &NoticeRow, done: &LinkedDone) -> Option<DateTime<Utc>> {
    row.linked_task_id
        .as_ref()
        .and_then(|task| done.get(task).copied())
}
