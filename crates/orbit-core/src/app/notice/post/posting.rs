use crate::app::resident;
use chrono::Duration;
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_store::contract::{NoticePosterEventFilter, NoticeScopeFilter};
use orbit_types::record::{
    AuthorClass, Notice, NoticeKind, NoticeScope, NoticeWorkspace, PostOutcome,
    resident_status_notice_key,
};

use super::request::{NoticePostRequest, validate_notice_post};
use super::transaction::{self, NoticePostPlan, NoticePostSnapshot};
use crate::OrbitRuntime;
use crate::app::notice::authority::{
    NoticeCaller, NoticePoster, resident_control_line_poster, resolve_notice_poster,
};
use crate::app::notice::{NoticeAudit, limit as limits, parse_notice_row, read, refuse};
use crate::app::resident::ResidentControlState;

/// What a post did, and the notice it left.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoticePostResult {
    pub notice: Notice,
    pub outcome: PostOutcome,
}

/// Post a notice for `caller`, recording an audit row for the write or
/// the refusal.
pub fn post_notice(
    rt: &OrbitRuntime,
    caller: &NoticeCaller,
    request: NoticePostRequest,
) -> Result<NoticePostResult, OrbitError> {
    let mut audit = NoticeAudit::new("post", None);
    audit.detail = serde_json::json!({
        "kind": request.kind.as_str(),
        "scope": request.scope.map(NoticeScope::as_str),
        "dedupe_key": request.dedupe_key,
        "linked_task": request.linked_task,
    });
    let result = post_notice_unaudited(rt, caller, request, &mut audit);
    if !result.as_ref().is_ok_and(|(_, repeated)| *repeated) {
        audit.record(
            rt,
            &result.as_ref().map(|(posted, _)| posted.notice.id.clone()),
        );
    }
    result.map(|(posted, _)| posted)
}

fn post_notice_unaudited(
    rt: &OrbitRuntime,
    caller: &NoticeCaller,
    request: NoticePostRequest,
    audit: &mut NoticeAudit,
) -> Result<(NoticePostResult, bool), OrbitError> {
    rt.ensure_coordination_task_write_permitted()?;
    let poster = resolve_notice_poster(rt, caller)?;
    if let (Some(task_id), Some(status_key), Some(_)) = (
        poster.task_id.as_deref(),
        poster.resident_status_key(),
        poster.run_id.as_ref(),
    ) && request.dedupe_key.as_deref().map(str::trim) == Some(status_key.as_str())
    {
        // Lock order: mandate task, then notice store. Control records
        // use this same task lock. A cycle either posts before control,
        // or observes it and leaves the final line alone.
        let mut request = Some(request);
        let mut posted = None;
        rt.stores().tasks().with_task_write_lock(task_id, &mut || {
            let request = request.take().ok_or_else(|| {
                OrbitError::Execution("the resident status post was already consumed".into())
            })?;
            posted = Some(post_notice_as(rt, &poster, request, audit)?);
            Ok(())
        })?;
        return posted.ok_or_else(|| {
            OrbitError::Execution("the resident status post was not executed".into())
        });
    }
    post_notice_as(rt, &poster, request, audit)
}

/// Replace a resident mandate's status line with the line a control
/// action leaves (`paused`, `stopped`, ...), so the notice says the
/// mandate no longer cycles instead of keeping its last cycle's words.
/// It is written as the mandate's own assignment: the next cycle's line
/// replaces it in place once manager control permits cycling again.
pub(crate) fn post_resident_control_line(
    rt: &OrbitRuntime,
    task_id: &str,
    title: &str,
    body: &str,
    lasting: bool,
) -> Result<NoticePostResult, OrbitError> {
    let single_line = |text: &str, max_chars: usize| -> String {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|ch| !ch.is_control())
            .take(max_chars)
            .collect()
    };
    let request = NoticePostRequest {
        kind: NoticeKind::Info,
        title: single_line(title, limits::MAX_TITLE_CHARS as usize),
        body: Some(single_line(body, 1000)),
        scope: None,
        dedupe_key: Some(resident_status_notice_key(task_id)),
        linked_task: None,
        close_on_linked_done: None,
        evidence: Vec::new(),
        hosts: Vec::new(),
        expires_in_hours: None,
        expires_at: None,
    };
    let mut audit = NoticeAudit::new("post", None);
    audit.detail = serde_json::json!({
        "kind": request.kind.as_str(),
        "dedupe_key": request.dedupe_key,
        "cause": "resident_control",
    });
    let result = rt
        .ensure_coordination_task_write_permitted()
        .and_then(|()| resident_control_line_poster(rt, task_id, lasting))
        .and_then(|poster| post_notice_as(rt, &poster, request, &mut audit));
    if !result.as_ref().is_ok_and(|(_, repeated)| *repeated) {
        audit.record(
            rt,
            &result.as_ref().map(|(posted, _)| posted.notice.id.clone()),
        );
    }
    result.map(|(posted, _)| posted)
}

fn post_notice_as(
    rt: &OrbitRuntime,
    poster: &NoticePoster,
    request: NoticePostRequest,
    audit: &mut NoticeAudit,
) -> Result<(NoticePostResult, bool), OrbitError> {
    audit.poster = Some(poster.class);
    #[cfg(feature = "test-support")]
    let now = crate::app::notice::test_support::notice_post_time();
    #[cfg(not(feature = "test-support"))]
    let now = chrono::Utc::now();
    let post = validate_notice_post(rt, poster, request)?;
    audit.detail["dedupe_key"] = serde_json::Value::String(post.dedupe_key.clone());
    if post.status_line
        && poster.run_id.is_some()
        && let Some(task_id) = poster.task_id.as_deref()
        && resident::resident_control_state(rt, task_id)? != ResidentControlState::Active
    {
        // An admitted cycle may still report and checkpoint after
        // pause, takeover or stop. Keep the manager's final status,
        // including its expiry, rather than making that cycle fail.
        post.resolve_expiry(poster, now)?;
        let notice = read::resident_status_line(rt, task_id)?.ok_or_else(|| {
            refuse(
                NoticeRefusalCode::InvalidField,
                "the controlled resident has no status line to preserve; checkpoint and finish the admitted cycle",
            )
        })?;
        audit.detail["outcome"] = serde_json::json!(PostOutcome::Updated.as_str());
        audit.detail["cause"] = serde_json::json!("resident_control_preserved");
        audit.detail["request_fingerprint"] =
            serde_json::json!(post.request_fingerprint(poster, notice.scope));
        return Ok((
            NoticePostResult {
                notice,
                outcome: PostOutcome::Updated,
            },
            false,
        ));
    }
    let workspace_id = rt.workspace_id()?;
    let workspace = NoticeWorkspace {
        name: read::notice_workspace_name(rt, &workspace_id),
        id: workspace_id,
    };
    let store = rt.sqlite_store()?;
    let existing = store
        .read()?
        .notice()
        .list(NoticeScopeFilter::WorkspaceAndGlobal(&workspace.id))?;
    let linked_done = read::linked_done_tasks(rt, &existing);
    let events_since = match poster.class {
        // A resident's rate is hourly, like an orchestrator's.
        AuthorClass::Agent if poster.resident.is_some() => now - Duration::hours(1),
        AuthorClass::Agent => now - Duration::hours(24),
        AuthorClass::Orchestrator => now - Duration::hours(1),
        AuthorClass::Operator | AuthorClass::System => now,
    };
    let purge_before = now - Duration::days(i64::from(limits::RETENTION_DAYS));
    let poster_events_filter = NoticePosterEventFilter {
        since: events_since,
        poster_task_id: match poster.class {
            AuthorClass::Agent => poster.task_id.as_deref(),
            AuthorClass::Operator | AuthorClass::Orchestrator | AuthorClass::System => None,
        },
        poster_crew: match poster.class {
            AuthorClass::Orchestrator => poster.crew.as_deref(),
            AuthorClass::Operator | AuthorClass::Agent | AuthorClass::System => None,
        },
    };
    let (row, outcome, repeated) = store.write(|tx| {
        let notices = tx.notice();
        let snapshot = NoticePostSnapshot {
            unclosed: notices.list_unclosed(&workspace.id)?,
            poster_events: notices.list_poster_events(&poster_events_filter)?,
            latest_events: notices.list_latest_events(&workspace.id)?,
        };
        let (plan, decided) =
            transaction::decide_post(poster, &workspace, &post, &snapshot, &linked_done, now)?;
        if !matches!(plan, NoticePostPlan::Unchanged { .. }) {
            notices.purge_inactive(&workspace.id, purge_before)?;
        }
        match plan {
            NoticePostPlan::Unchanged { row } => Ok((row, decided, true)),
            NoticePostPlan::Insert { row, event } => {
                notices.put(&row)?;
                notices.put_event(&event)?;
                Ok((row, decided, false))
            }
            NoticePostPlan::Update {
                row,
                expected_revision,
                event,
            } => {
                if !notices.replace_open(&row, expected_revision)? {
                    return Err(OrbitError::Store(format!(
                        "notice {} changed inside its posting transaction",
                        row.id
                    )));
                }
                notices.put_event(&event)?;
                Ok((row, decided, false))
            }
        }
    })?;
    let mut notice = parse_notice_row(&row)?;
    notice.resolve_effective_status(now, None);
    audit.detail["outcome"] = serde_json::Value::String(outcome.as_str().to_string());
    audit.detail["request_fingerprint"] =
        serde_json::json!(post.request_fingerprint(poster, notice.scope));
    Ok((NoticePostResult { notice, outcome }, repeated))
}
