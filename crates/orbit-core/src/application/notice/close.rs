//! Manual and linked-task notice closure.

use chrono::{DateTime, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_store::Store;
use orbit_store::contract::{NoticeEventRow, NoticeRow, NoticeScopeFilter};
use orbit_types::record::{
    AuthorClass, EffectiveStatus, NoticeAuthor, NoticeCloseCause, NoticeClosed,
};

use crate::OrbitRuntime;
use crate::application::task::TaskNoticePort;

#[cfg(feature = "test-support")]
use super::test_support;
use super::{
    LinkedDone, NoticeAudit, NoticeCaller, NoticeCloseResult, NoticeSelector, notice_row,
    parse_notice_row, refuse,
};

/// The longest close reason.
const MAX_CLOSE_REASON_CHARS: usize = 240;

impl OrbitRuntime {
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
            NoticeSelector::Id(id) => store.read()?.notice().get(id)?,
            NoticeSelector::DedupeKey(key) => {
                let rows = store
                    .read()?
                    .notice()
                    .list(NoticeScopeFilter::Workspace(&workspace_id))?;
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
        // A corroboration can commit after this snapshot. It does not move
        // `revision`, so the closing transaction must keep its reports.
        #[cfg(feature = "test-support")]
        test_support::after_notice_close_snapshot();
        // The snapshot above can omit a corroboration that committed before
        // the write. The stored row is what the close returns.
        let Some(stored) =
            commit_notice_close(&store, &notice_row(&notice)?, row.revision, &event)?
        else {
            return Err(OrbitError::Execution(format!(
                "notice {} changed while closing it; read it again",
                row.id
            )));
        };
        Ok(NoticeCloseResult {
            notice: parse_notice_row(&stored)?,
            already_inactive: false,
        })
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

    /// One sweep pass over this workspace's open notices: store the
    /// closure of every one whose linked task — in this workspace or
    /// another workspace this host owns — is done. The done-task
    /// transition writer covers links it can see; this covers the rest (a
    /// task already done when the notice was posted, a transition another
    /// workspace's runtime made). Reads already report these notices
    /// closed either way. Returns how many linked tasks it closed
    /// notices for.
    pub fn run_notice_closure_tick(&self) -> Result<usize, OrbitError> {
        if self.coordination_write_owner().is_some() {
            return Ok(0);
        }
        let workspace_id = self.workspace_id()?;
        let store = self.sqlite_store()?;
        let rows = store
            .read()?
            .notice()
            .list(NoticeScopeFilter::Workspace(&workspace_id))?;
        let done = self.linked_done_tasks(&rows);
        for task_id in done.keys() {
            self.materialize_notice_closures(task_id)?;
        }
        Ok(done.len())
    }

    fn materialize_notice_closures(&self, task_id: &str) -> Result<(), OrbitError> {
        if self.coordination_write_owner().is_some() {
            return Ok(());
        }
        let store = self.sqlite_store()?;
        let now = Utc::now();
        let linked = store.read()?.notice().list_linked(task_id)?;
        for row in linked {
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
            // Same snapshot window as a manual close: a corroboration committed
            // after this read shares `revision` and must survive the write.
            #[cfg(feature = "test-support")]
            test_support::after_notice_close_snapshot();
            if commit_notice_close(&store, &notice_row(&notice)?, row.revision, &event)?.is_some() {
                let mut audit = NoticeAudit::new("close", Some(row.id.clone()));
                audit.poster = Some(AuthorClass::System);
                audit.detail = serde_json::json!({ "cause": "linked_task_done", "task": task_id });
                audit.record(self, &Ok(row.id.clone()));
            }
        }
        Ok(())
    }
}

/// Close `row` at `expected_revision` inside one IMMEDIATE transaction,
/// preserving any corroboration `reports` committed since the snapshot read.
/// Returns the stored closed row, or `None` when the notice is already closed
/// or no longer at `expected_revision`.
fn commit_notice_close(
    store: &Store,
    row: &NoticeRow,
    expected_revision: u32,
    event: &NoticeEventRow,
) -> Result<Option<NoticeRow>, OrbitError> {
    let Some(closed_at) = row.closed_at else {
        return Err(OrbitError::Store(format!(
            "closing notice {} without a close time",
            row.id
        )));
    };
    store.write(|tx| {
        let notices = tx.notice();
        // Read inside the write lock. A corroboration that committed after
        // the caller snapped the row still matches `revision`, and its
        // `reports` must survive the replacement of `record_json`.
        let Some(stored) = notices.get(&row.id)? else {
            return Ok(None);
        };
        if stored.closed_at.is_some() || stored.revision != expected_revision {
            return Ok(None);
        }
        let record_json =
            closing_record_with_stored_reports(&stored.record_json, &row.record_json)?;
        if !notices.close(&row.id, expected_revision, closed_at, &record_json)? {
            return Ok(None);
        }
        notices.put_event(event)?;
        let written = notices.get(&row.id)?.ok_or_else(|| {
            OrbitError::Store(format!("notice {} was missing after it closed", row.id))
        })?;
        Ok(Some(written))
    })
}

/// The closing document, with `reports` taken from the row locked for the
/// close. A post can change `reports` without moving `revision`.
fn closing_record_with_stored_reports(
    stored_json: &str,
    closing_json: &str,
) -> Result<String, OrbitError> {
    let mut closing: serde_json::Value = serde_json::from_str(closing_json).map_err(|error| {
        OrbitError::Store(format!("closing notice document is unreadable: {error}"))
    })?;
    let stored: serde_json::Value = serde_json::from_str(stored_json).map_err(|error| {
        OrbitError::Store(format!("stored notice document is unreadable: {error}"))
    })?;
    let Some(document) = closing.as_object_mut() else {
        return Err(OrbitError::Store(
            "closing notice document is not an object".into(),
        ));
    };
    if let Some(reports) = stored.get("reports").cloned() {
        document.insert("reports".to_string(), reports);
    }
    serde_json::to_string(&closing).map_err(|error| {
        OrbitError::Store(format!("closing notice document is unreadable: {error}"))
    })
}

impl TaskNoticePort for OrbitRuntime {
    fn close_linked_notices_for_done_transition(&self, task_id: &str) {
        self.materialize_notice_closures_for_done_task(task_id);
    }
}

/// When a row's linked task became done, for its effective state.
pub(super) fn linked_done_at(row: &NoticeRow, done: &LinkedDone) -> Option<DateTime<Utc>> {
    row.linked_task_id
        .as_ref()
        .and_then(|task| done.get(task).copied())
}
