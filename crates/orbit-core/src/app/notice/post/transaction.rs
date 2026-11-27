use chrono::{DateTime, Duration, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_store::contract::{NoticeEventRow, NoticeRow};
use orbit_types::record::{
    AuthorClass, EffectiveStatus, Notice, NoticeReports, NoticeScope, NoticeWorkspace, PostOutcome,
};

use super::request::{self, ValidatedPost};
use crate::app::notice::authority::NoticePoster;
use crate::app::notice::{LinkedDone, limit as limits, notice_row, parse_notice_row, refuse};

/// What a post's decision reads inside the posting transaction.
#[derive(Debug, Clone, Default)]
pub(super) struct NoticePostSnapshot {
    /// Every stored-open notice of the home workspace, and every stored-open
    /// `global` notice in this store, whatever its home.
    pub(super) unclosed: Vec<NoticeRow>,
    /// Post events since the poster's rate-limit window start.
    pub(super) poster_events: Vec<NoticeEventRow>,
    /// The latest event of any poster for each stored-open notice in the
    /// home workspace, ordered by insertion sequence rather than timestamp.
    pub(super) latest_events: Vec<NoticeEventRow>,
}

/// The write a post's decision chose.
#[derive(Debug, Clone)]
pub(super) enum NoticePostPlan {
    /// Return a snapshot row without writing a notice, event or purge.
    Unchanged { row: NoticeRow },
    /// A new notice.
    Insert {
        row: NoticeRow,
        event: NoticeEventRow,
    },
    /// Replace an open notice computed from the snapshot at
    /// `expected_revision`.
    Update {
        row: NoticeRow,
        expected_revision: u32,
        event: NoticeEventRow,
    },
}

/// Whether a stored-open row is in force at `now`.
fn effective_open(row: &NoticeRow, now: DateTime<Utc>, linked_done: &LinkedDone) -> bool {
    row.closed_at.is_none()
        && row.expires_at > now
        && !(row.close_on_linked_done
            && row
                .linked_task_id
                .as_ref()
                .is_some_and(|task| linked_done.contains_key(task)))
}

fn new_notice_id() -> Result<String, OrbitError> {
    let value = getrandom::u64()
        .map_err(|error| OrbitError::Execution(format!("mint notice id: {error}")))?;
    Ok(format!("ntc_{value:016x}"))
}

pub(super) fn decide_post(
    poster: &NoticePoster,
    workspace: &NoticeWorkspace,
    post: &ValidatedPost,
    snapshot: &NoticePostSnapshot,
    linked_done: &LinkedDone,
    now: DateTime<Utc>,
) -> Result<(NoticePostPlan, PostOutcome), OrbitError> {
    let open: Vec<&NoticeRow> = snapshot
        .unclosed
        .iter()
        .filter(|row| effective_open(row, now, linked_done))
        .collect();
    let open_globals = open.iter().filter(|row| row.scope == "global").count();
    let matched = open
        .iter()
        .find(|row| row.workspace_id == workspace.id && row.dedupe_key == post.dedupe_key);
    let fingerprint = post.request_fingerprint(
        poster,
        matched.map_or(NoticeScope::Workspace, |row| {
            if row.scope == "global" {
                NoticeScope::Global
            } else {
                NoticeScope::Workspace
            }
        }),
    );
    if let Some(matched) = matched
        && snapshot
            .latest_events
            .iter()
            .find(|event| event.notice_id == matched.id)
            .and_then(|event| serde_json::from_str::<serde_json::Value>(&event.detail_json).ok())
            .and_then(|detail| {
                detail["request_fingerprint"]
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .as_deref()
            == Some(&fingerprint)
    {
        let outcome = if poster.may_edit(&matched.author_class, matched.author_task_id.as_deref()) {
            PostOutcome::Updated
        } else {
            PostOutcome::Corroborated
        };
        return Ok((
            NoticePostPlan::Unchanged {
                row: (*matched).clone(),
            },
            outcome,
        ));
    }
    let expires_at = post.resolve_expiry(poster, now)?;
    match poster.class {
        AuthorClass::Agent
            if poster.resident.is_some()
                && snapshot.poster_events.len() >= limits::RESIDENT_POSTS_PER_HOUR as usize =>
        {
            return Err(request::limit(
                "resident_posts_per_hour",
                limits::RESIDENT_POSTS_PER_HOUR,
            ));
        }
        AuthorClass::Agent
            if poster.resident.is_none()
                && snapshot.poster_events.len()
                    >= limits::AGENT_POSTS_PER_TASK_PER_DAY as usize =>
        {
            return Err(request::limit(
                "agent_posts_per_task_per_day",
                limits::AGENT_POSTS_PER_TASK_PER_DAY,
            ));
        }
        AuthorClass::Orchestrator
            if snapshot.poster_events.len() >= limits::ORCHESTRATOR_POSTS_PER_HOUR as usize =>
        {
            return Err(request::limit(
                "orchestrator_posts_per_hour",
                limits::ORCHESTRATOR_POSTS_PER_HOUR,
            ));
        }
        _ => {}
    }
    let event = |notice_id: &str, outcome: PostOutcome, revision: u32| NoticeEventRow {
        workspace_id: workspace.id.clone(),
        notice_id: notice_id.to_string(),
        at: now,
        action: outcome.as_str().to_string(),
        poster_class: poster.class.as_str().to_string(),
        poster_crew: poster.crew.clone(),
        poster_task_id: poster.task_id.clone(),
        detail_json: serde_json::json!({
            "revision": revision, "request_fingerprint": fingerprint,
        })
        .to_string(),
    };
    let Some(matched) = matched else {
        let scope = post.scope.unwrap_or(NoticeScope::Workspace);
        let home_open = open
            .iter()
            .filter(|row| row.workspace_id == workspace.id)
            .count();
        if home_open >= limits::MAX_OPEN_PER_WORKSPACE as usize {
            return Err(request::limit(
                "max_open_per_workspace",
                limits::MAX_OPEN_PER_WORKSPACE,
            ));
        }
        if scope == NoticeScope::Global
            && open_globals >= limits::MAX_OPEN_GLOBAL_PER_OWNER as usize
        {
            return Err(request::limit(
                "max_open_global_per_owner",
                limits::MAX_OPEN_GLOBAL_PER_OWNER,
            ));
        }
        // A resident's status line is exempt from the per-mandate open cap
        // both while live and when created or recreated after expiry, while
        // still subject to the workspace, global and hourly limits above.
        if poster.class == AuthorClass::Agent && !post.status_line {
            let status_key = poster.resident_status_key();
            let task_open = open
                .iter()
                .filter(|row| {
                    row.author_task_id.is_some()
                        && row.author_task_id == poster.task_id
                        && !status_key
                            .as_deref()
                            .is_some_and(|key| row.dedupe_key == key)
                })
                .count();
            let (name, cap) = match poster.resident {
                Some(_) => ("max_open_per_resident", limits::MAX_OPEN_PER_RESIDENT),
                None => ("max_open_per_agent_task", limits::MAX_OPEN_PER_AGENT_TASK),
            };
            if task_open >= cap as usize {
                return Err(request::limit(name, cap));
            }
        }
        let notice = Notice {
            id: new_notice_id()?,
            workspace: workspace.clone(),
            scope,
            kind: post.kind,
            title: post.title.clone(),
            body: post.body.clone(),
            dedupe_key: post.dedupe_key.clone(),
            author: poster.author(),
            linked_task: post.linked_task.clone(),
            close_on_linked_done: post.close_on_linked_done,
            evidence: post.evidence.clone(),
            hosts: post.hosts.clone(),
            expires_at: expires_at.unwrap_or_else(|| {
                now + Duration::hours(i64::from(poster.default_expiry_hours(post.kind)))
            }),
            opened_at: now,
            updated_at: now,
            revision: 1,
            reports: NoticeReports {
                count: 1,
                last_at: now,
            },
            closed: None,
            status: EffectiveStatus::Open,
        };
        let row = notice_row(&notice)?;
        let event = event(&notice.id, PostOutcome::Created, 1);
        return Ok((NoticePostPlan::Insert { row, event }, PostOutcome::Created));
    };
    let mut notice = parse_notice_row(matched)?;
    let expected_revision = matched.revision;
    notice.reports.count = notice.reports.count.saturating_add(1);
    notice.reports.last_at = now;
    if !poster.may_edit(&matched.author_class, matched.author_task_id.as_deref()) {
        let row = notice_row(&notice)?;
        let event = event(&notice.id, PostOutcome::Corroborated, notice.revision);
        return Ok((
            NoticePostPlan::Update {
                row,
                expected_revision,
                event,
            },
            PostOutcome::Corroborated,
        ));
    }
    let scope = post.scope.unwrap_or(notice.scope);
    if scope == NoticeScope::Global && notice.scope == NoticeScope::Workspace {
        if !poster.may_post_scope(NoticeScope::Global) {
            return Err(refuse(
                NoticeRefusalCode::ScopeRefused,
                "this poster may not widen a notice to global scope",
            ));
        }
        if open_globals >= limits::MAX_OPEN_GLOBAL_PER_OWNER as usize {
            return Err(request::limit(
                "max_open_global_per_owner",
                limits::MAX_OPEN_GLOBAL_PER_OWNER,
            ));
        }
    }
    if scope == NoticeScope::Workspace
        && notice.scope == NoticeScope::Global
        && notice.author.class != poster.class
    {
        return Err(refuse(
            NoticeRefusalCode::ScopeRefused,
            "a global notice another class authored cannot be narrowed",
        ));
    }
    let before = notice.clone();
    notice.scope = scope;
    notice.kind = post.kind;
    notice.title = post.title.clone();
    notice.body = post.body.clone();
    notice.evidence = post.evidence.clone();
    notice.hosts = post.hosts.clone();
    if post.linked_task.is_some() {
        notice.linked_task = post.linked_task.clone();
        notice.close_on_linked_done = post.close_on_linked_done;
    }
    if let Some(expires_at) = expires_at {
        notice.expires_at = expires_at;
    }
    let changed = {
        let mut compare = before.clone();
        compare.reports = notice.reports.clone();
        compare != notice
    };
    if changed {
        notice.revision = notice.revision.saturating_add(1);
        notice.updated_at = now;
    }
    let row = notice_row(&notice)?;
    let event = event(&notice.id, PostOutcome::Updated, notice.revision);
    Ok((
        NoticePostPlan::Update {
            row,
            expected_revision,
            event,
        },
        PostOutcome::Updated,
    ))
}
