//! Crew registry handlers.

use crate::state::{DashboardState, Ws};
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

pub(super) async fn list_crews(Ws(runtime): Ws) -> Response {
    Json(runtime.configured_crew_registry_projection()).into_response()
}

/// Read crew registries from the same pinned workspace generation.
pub(super) async fn list_all_crews(
    State(state): State<DashboardState>,
    RawQuery(query): RawQuery,
) -> Response {
    let requested: std::collections::HashSet<String> =
        url::form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
            .filter_map(|(key, value)| (key == "workspaces").then(|| value.into_owned()))
            .collect();
    match super::blocking("aggregate crew registries", move || {
        let pinned = state.pin();
        let entries = pinned
            .entries()
            .iter()
            .filter(|entry| requested.contains(&entry.id))
            .collect::<Vec<_>>();
        let registries = super::workspaces::fan_out::fan_out(entries.len(), |index| {
            let entry = entries[index];
            let runtime = pinned.runtime_for(&entry.id).ok()?;
            Some((
                entry.id.clone(),
                runtime.configured_crew_registry_projection(),
            ))
        })
        .into_iter()
        .flatten()
        .collect::<std::collections::BTreeMap<_, _>>();
        Ok(json!({ "workspaces": registries }))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}
