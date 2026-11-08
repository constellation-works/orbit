//! Notice listing, history and linked-task projection.

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_store::contracts::{NoticeRow, NoticeStoreBackend as _};
use orbit_types::record::{EffectiveStatus, Notice, NoticeKind};
use orbit_types::task::TaskStatus;

use crate::OrbitRuntime;

use super::{
    LinkedDone, NoticeHistoryEntry, NoticeListFilter, NoticeListing, NoticeView, linked_done_at,
    parse_notice_row, refuse,
};

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
    /// changed, resolved through this owner's registered workspaces: a link
    /// may name a task in another workspace the same host owns, never one
    /// on another host.
    pub(crate) fn linked_done_tasks(&self, rows: &[NoticeRow]) -> LinkedDone {
        rows.iter()
            .filter(|row| row.closed_at.is_none() && row.close_on_linked_done)
            .filter_map(|row| row.linked_task_id.as_deref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let task = self.notice_linked_task(id)?;
                (task.status == TaskStatus::Done).then(|| (id.to_string(), task.updated_at))
            })
            .collect()
    }

    /// A notice's linked task as this owner holds it, in whichever of its
    /// workspaces owns the task. `None` when no workspace on this host
    /// holds it (deleted, or held by another host): the notice then stays
    /// open and reports the link's status as unknown.
    pub(crate) fn notice_linked_task(&self, id: &str) -> Option<orbit_types::task::Task> {
        use orbit_store::RegisteredTaskResolution;
        match self.resolve_dependency_task(id) {
            Ok(RegisteredTaskResolution::Resolved(task)) => Some(*task),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(task = id, %error, "failed to resolve a notice's linked task");
                None
            }
        }
    }

    /// The notices of this workspace (and global ones) matching `filter`,
    /// in listing order.
    pub fn list_notices(&self, filter: &NoticeListFilter) -> Result<NoticeListing, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let store = self.sqlite_store()?;
        let rows = if filter.all_workspaces {
            store.notice_rows(None, false)?
        } else {
            store.notice_rows(
                Some(std::slice::from_ref(&workspace_id)),
                filter.include_global,
            )?
        };
        let now = Utc::now();
        let retention = Duration::days(i64::from(super::limits::RETENTION_DAYS));
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
        if let Some(limit) = filter.limit {
            notices.truncate(limit.clamp(1, 200));
        }
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
        if let Some(linked) = notice.linked_task.as_mut() {
            linked.status = Some(
                self.notice_linked_task(&linked.id)
                    .map(|task| task.status.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
            );
        }
    }
}
