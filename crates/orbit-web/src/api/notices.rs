//! Fleet notices for the dashboard: the banner read, the Notices view and
//! the operator post/close forms.
//!
//! Reads go through the same core effective-state read as `orbit.notice.list`
//! (`OrbitRuntime::list_notices`); writes go through the same core post/close
//! the tool uses, with the poster resolved as the operator when this server
//! holds an operator session. Notice title and body are plain text on the
//! wire; the dashboard renders them as text nodes.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::application::notice::{
    NoticeCaller, NoticeListFilter, NoticePostRawFields, NoticePostRequest, NoticeSelector,
    NoticeStatusFilter,
};
use orbit_types::record::{EffectiveStatus, Notice, NoticeKind};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{bad_request, blocking};
use crate::state::DashboardState;

/// `?workspace=<id|all>` selects the shown workspaces.
#[derive(Debug, Default, Deserialize)]
pub(super) struct NoticesQuery {
    #[serde(default)]
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct NoticePostBody {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    dedupe_key: Option<String>,
    #[serde(default)]
    linked_task: Option<String>,
    #[serde(default)]
    close_on_linked_done: Option<bool>,
    #[serde(default)]
    hosts: Vec<String>,
    #[serde(default)]
    expires_in_hours: Option<u64>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct NoticeCloseBody {
    #[serde(default)]
    reason: Option<String>,
}

fn ended_at(notice: &Notice) -> Option<DateTime<Utc>> {
    match notice.status {
        EffectiveStatus::Closed => notice.closed.as_ref().map(|closed| closed.at),
        EffectiveStatus::Expired => Some(notice.expires_at),
        EffectiveStatus::Open => None,
    }
}

fn ended_label(notice: &Notice) -> Option<String> {
    match notice.status {
        EffectiveStatus::Open => None,
        EffectiveStatus::Expired => Some(format!("expired at {}", notice.expires_at.to_rfc3339())),
        EffectiveStatus::Closed => {
            let closed = notice.closed.as_ref()?;
            match closed.cause {
                orbit_types::record::NoticeCloseCause::LinkedTaskDone => {
                    Some("linked task done".to_string())
                }
                orbit_types::record::NoticeCloseCause::Manual => {
                    let mut by = closed.by.class.as_str().to_string();
                    if let Some(crew) = &closed.by.crew {
                        by = format!("{by} {crew}");
                    }
                    if let Some(task) = &closed.by.task_id {
                        by = format!("{by} {task}");
                    }
                    let mut label = format!("closed by {by}");
                    if let Some(reason) = &closed.reason {
                        label = format!("{label}: {reason}");
                    }
                    Some(label)
                }
            }
        }
    }
}

fn notice_json(notice: &Notice) -> Value {
    let mut value = serde_json::to_value(notice)
        .unwrap_or_else(|_| json!({"id": notice.id, "title": notice.title}));
    if let Some(object) = value.as_object_mut() {
        object.insert("ended_label".into(), json!(ended_label(notice)));
        object.insert("ended_at".into(), json!(ended_at(notice)));
    }
    value
}

struct ResolvedNoticeRuntimes {
    runtimes: Vec<Arc<OrbitRuntime>>,
    degraded: Vec<String>,
}

fn resolve_runtimes(
    state: &DashboardState,
    workspace: Option<&str>,
) -> Result<ResolvedNoticeRuntimes, Box<Response>> {
    let pinned = state.pin();
    match workspace {
        Some("all") => {
            let mut runtimes = Vec::new();
            let mut degraded = Vec::new();
            // The cache is only an optimization: cold and config-invalidated
            // homes still belong to this aggregate's pinned workspace set.
            for entry in pinned.entries() {
                if !entry.active {
                    degraded.push(format!("workspace '{}' is unavailable", entry.id));
                    continue;
                }
                match pinned.runtime_for(&entry.id) {
                    Ok(runtime) => runtimes.push(runtime),
                    Err(_) => {
                        tracing::warn!(workspace = %entry.id, "notice list could not open workspace");
                        degraded.push(format!("failed to open workspace '{}'", entry.id));
                    }
                }
            }
            Ok(ResolvedNoticeRuntimes { runtimes, degraded })
        }
        other => {
            let id = match other.filter(|value| !value.is_empty()) {
                Some(id) => id.to_string(),
                None => pinned
                    .default_workspace()
                    .map(str::to_string)
                    .ok_or_else(|| {
                        Box::new(bad_request(
                            "no workspace selected and no default is configured; pass ?workspace=<id>"
                                .to_string(),
                        ))
                    })?,
            };
            let runtime = pinned
                .runtime_for(&id)
                .map_err(|error| Box::new(error.into_response()))?;
            Ok(ResolvedNoticeRuntimes {
                runtimes: vec![runtime],
                degraded: Vec::new(),
            })
        }
    }
}

fn resolve_runtime(
    state: &DashboardState,
    workspace: Option<&str>,
) -> Result<Arc<OrbitRuntime>, Box<Response>> {
    if workspace == Some("all") {
        return Err(Box::new(bad_request(
            "select one workspace to write a notice".to_string(),
        )));
    }
    let mut resolved = resolve_runtimes(state, workspace)?;
    resolved
        .runtimes
        .pop()
        .ok_or_else(|| Box::new(bad_request("no workspace selected".to_string())))
}

fn operator_caller(state: &DashboardState) -> Result<NoticeCaller, Box<Response>> {
    if !state.operator_session() {
        return Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "an operator session is required to write notices",
                    "code": "authorization_denied",
                })),
            )
                .into_response(),
        ));
    }
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(McpCapability::Agent);
    session
        .effective_capabilities
        .insert(McpCapability::Operator);
    Ok(NoticeCaller {
        session,
        scope_task_id: None,
        scope_run_id: None,
        orchestrator: None,
        model: None,
    })
}

/// `GET /api/notices` — open notices plus the last 24h of closed/expired.
pub(super) async fn list_notices(
    State(state): State<DashboardState>,
    Query(query): Query<NoticesQuery>,
) -> Response {
    let can_write = state.operator_session();
    // Registry refresh and cold/config-invalidated runtime construction do
    // filesystem and store I/O, so keep resolution on the blocking pool.
    let resolved = blocking("resolve notice workspaces", move || {
        Ok(resolve_runtimes(&state, query.workspace.as_deref()))
    })
    .await
    .and_then(|resolved| resolved);
    let ResolvedNoticeRuntimes {
        runtimes,
        mut degraded,
    } = match resolved {
        Ok(resolved) => resolved,
        Err(response) => return *response,
    };
    match blocking("list notices", move || {
        let mut by_id: BTreeMap<String, Notice> = BTreeMap::new();
        for runtime in &runtimes {
            // Read the full effective state before choosing what to show:
            // the core listing sorts by kind and truncates at `limit`, so a
            // bounded `All` read lets retained closed/expired rows consume
            // the cap and silently hide an open notice of a later kind.
            // `limit: None` returns every retained match; the open/recent
            // selection below is the only bounding this endpoint applies.
            let listing = runtime.list_notices(&NoticeListFilter {
                status: NoticeStatusFilter::All,
                limit: None,
                ..Default::default()
            })?;
            degraded.extend(listing.degraded);
            for notice in listing.notices {
                by_id.entry(notice.id.clone()).or_insert(notice);
            }
        }
        Ok::<_, orbit_core::OrbitError>((by_id.into_values().collect::<Vec<_>>(), degraded))
    })
    .await
    {
        Ok((notices, degraded)) => {
            let now = Utc::now();
            let cutoff = now - Duration::hours(24);
            let mut open: Vec<&Notice> = notices
                .iter()
                .filter(|notice| notice.status == EffectiveStatus::Open)
                .collect();
            open.sort_by(|a, b| a.cmp_list_order(b));
            let mut recent: Vec<&Notice> = notices
                .iter()
                .filter(|notice| {
                    notice.status != EffectiveStatus::Open
                        && ended_at(notice).is_some_and(|at| at >= cutoff)
                })
                .collect();
            recent.sort_by_key(|notice| std::cmp::Reverse(ended_at(notice)));
            let banner: Vec<Value> = open
                .iter()
                .filter(|notice| {
                    matches!(notice.kind, NoticeKind::Incident | NoticeKind::Maintenance)
                })
                .map(|notice| notice_json(notice))
                .collect();
            Json(json!({
                "open": open.iter().map(|notice| notice_json(notice)).collect::<Vec<_>>(),
                "recent": recent.iter().map(|notice| notice_json(notice)).collect::<Vec<_>>(),
                "banner": banner,
                "can_write": can_write,
                "degraded": degraded,
            }))
            .into_response()
        }
        Err(response) => *response,
    }
}

/// `POST /api/notices` — operator post, through the core notice post.
pub(super) async fn post_notice(
    State(state): State<DashboardState>,
    Query(query): Query<NoticesQuery>,
    Json(body): Json<NoticePostBody>,
) -> Response {
    let caller = match operator_caller(&state) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let runtime = match resolve_runtime(&state, query.workspace.as_deref()) {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    match blocking("post notice", move || {
        let request = NoticePostRequest::parse_raw(NoticePostRawFields {
            kind: body.kind,
            title: body.title.unwrap_or_default(),
            body: body.body,
            scope: body.scope,
            dedupe_key: body.dedupe_key,
            linked_task: body.linked_task,
            close_on_linked_done: body.close_on_linked_done,
            evidence: Vec::new(),
            hosts: body.hosts,
            expires_in_hours: body.expires_in_hours,
            expires_at: body.expires_at,
            blank_means_absent: true,
        })?;
        runtime.post_notice(&caller, request)
    })
    .await
    {
        Ok(result) => Json(json!({
            "notice": notice_json(&result.notice),
            "outcome": result.outcome,
        }))
        .into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/notices/{id}/close` — operator close, through the core close.
pub(super) async fn close_notice(
    State(state): State<DashboardState>,
    Query(query): Query<NoticesQuery>,
    Path(id): Path<String>,
    Json(body): Json<NoticeCloseBody>,
) -> Response {
    let caller = match operator_caller(&state) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let runtime = match resolve_runtime(&state, query.workspace.as_deref()) {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    let reason = body.reason.unwrap_or_default();
    match blocking("close notice", move || {
        runtime.close_notice(&caller, NoticeSelector::Id(id), &reason)
    })
    .await
    {
        Ok(result) => Json(json!({
            "notice": notice_json(&result.notice),
            "already_inactive": result.already_inactive,
        }))
        .into_response(),
        Err(response) => *response,
    }
}
