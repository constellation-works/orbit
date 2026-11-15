//! Compact all-status task matches for the shared dashboard search.

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::task::TaskListFilter;
use serde_json::json;

use super::{bad_request, blocking_join, server_error, workspaces};
use crate::state::DashboardState;

const SEARCH_RESULT_LIMIT: usize = 8;

/// `GET /api/tasks/search?q=…`: ID/title substring matches from every status,
/// ranked by ID prefix, then update time descending (ID breaks ties).
/// `workspace` selects one workspace; otherwise owned workspaces are searched,
/// narrowed by optional repeated `workspaces`. The response includes the exact
/// match count and explicitly names any unavailable aggregate sources.
pub(super) async fn search_dashboard_tasks(
    State(state): State<DashboardState>,
    RawQuery(raw): RawQuery,
) -> Response {
    let mut query = String::new();
    let mut workspace = None;
    let mut selected_workspaces = Vec::new();
    for (key, value) in url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "q" => query = value.trim().to_lowercase(),
            "workspace" => workspace = Some(value.into_owned()),
            "workspaces" => selected_workspaces.push(value.into_owned()),
            _ => {}
        }
    }
    if query.is_empty() {
        return Json(json!({ "items": [], "total": 0, "unavailable": [] })).into_response();
    }
    if query.len() > 512 {
        return bad_request("Search must be at most 512 bytes".to_string());
    }
    match blocking_join("dashboard task search", move || {
        let pinned = state.pin();
        if let Some(id) = &workspace
            && !pinned.entries().iter().any(|entry| entry.id == *id)
        {
            return bad_request(format!("Unknown workspace: {id}"));
        }
        let entries = pinned.entries().iter().filter(|entry| {
            if let Some(id) = &workspace {
                entry.id == *id
            } else {
                workspaces::entry_is_owned(entry)
                    && (selected_workspaces.is_empty() || selected_workspaces.contains(&entry.id))
            }
        });
        let filter = TaskListFilter {
            search: Some(query.clone()),
            ..Default::default()
        };
        let mut matches = Vec::new();
        let mut total = 0;
        let mut unavailable = Vec::new();
        for entry in entries {
            let runtime = match pinned.runtime_for(&entry.id) {
                Ok(runtime) => runtime,
                Err(error) => {
                    if workspace.is_some() {
                        return error.into_response();
                    }
                    unavailable.push(json!({
                        "workspace_name": entry.name, "error": "Workspace unavailable",
                    }));
                    continue;
                }
            };
            let mut candidates = match runtime.observe_task_candidates(&filter, usize::MAX) {
                Ok(page) => page.items,
                Err(error) => {
                    if workspace.is_some() {
                        return server_error(error);
                    }
                    tracing::warn!(workspace = %entry.id, %error, "dashboard search skipped a workspace");
                    unavailable.push(json!({
                        "workspace_name": entry.name, "error": "Could not read tasks",
                    }));
                    continue;
                }
            };
            total += candidates.len();
            // Rank before bounding: a recently edited old task and an ID-prefix
            // match must remain reachable even beyond the board's first page.
            candidates.sort_by_key(|task| {
                (
                    !task.id.to_lowercase().starts_with(&query),
                    std::cmp::Reverse(task.updated_at),
                    task.id.clone(),
                )
            });
            matches.extend(candidates.into_iter().take(SEARCH_RESULT_LIMIT).map(|task| {
                (
                    !task.id.to_lowercase().starts_with(&query),
                    std::cmp::Reverse(task.updated_at),
                    task.id,
                    task.title,
                    task.status,
                    entry.id.clone(),
                    entry.name.clone(),
                )
            }));
        }
        matches.sort_by(|a, b| (&a.0, &a.1, &a.2, &a.5).cmp(&(&b.0, &b.1, &b.2, &b.5)));
        let items = matches.into_iter().take(SEARCH_RESULT_LIMIT).map(
            |(_, _, id, title, status, workspace_id, workspace_name)| {
                json!({ "id": id, "title": title, "status": status.cli_name(),
                    "workspace_id": workspace_id, "workspace_name": workspace_name })
            },
        ).collect::<Vec<_>>();
        Json(json!({ "items": items, "total": total, "unavailable": unavailable })).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(response) => *response,
    }
}
