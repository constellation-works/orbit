//! `GET /api/resource-profiles`: Analytics › Resources. The selected
//! workspace's delivery resource profiles on this host, the same set admission
//! prices candidates with, beside the host's size and admit thresholds.
//! `?last=<n>` (or `last=all`) instead profiles each key's last `n`
//! retained deliveries, up to 1,000; `all` is a compatibility alias for that
//! maximum, without changing admission's set. `delivery_limit` discloses the
//! effective per-key bound and `max_last` the maximum explicit selection.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use crate::state::Ws;

/// The most deliveries an explicit `last` selection, including `all`, serves.
const MAX_LAST: usize = 1_000;

#[derive(Deserialize, Default)]
pub(super) struct ResourceProfilesQuery {
    /// A count of deliveries per key, or `all` for the maximum explicit
    /// selection; absent is `profile_window`, which may be larger.
    #[serde(default)]
    last: Option<String>,
}

/// Each profile is the object `orbit run profiles --json` emits. Profiles are
/// per host (each host keeps its own store), so this serving host's set is the
/// only one in the answer; another host's comes through its forward.
pub(super) async fn resource_profiles(
    Ws(runtime): Ws,
    Query(query): Query<ResourceProfilesQuery>,
) -> Response {
    let window = match query.last.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("all") => Some(MAX_LAST),
        Some(raw) => match raw.parse::<usize>() {
            Ok(count) if (1..=MAX_LAST).contains(&count) => Some(count),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!("last must be all or a count from 1 to {MAX_LAST}")
                    })),
                )
                    .into_response();
            }
        },
    };
    let selected = window.and(query.last.as_deref().map(|last| last.trim().to_string()));
    match blocking("resource profiles", move || {
        let workspace_id = runtime.workspace_id()?;
        let set = match window {
            Some(window) => runtime.resource_profiles_over(window)?,
            None => runtime.resource_profiles()?,
        };
        let profiles = set
            .profiles
            .iter()
            .filter(|profile| profile.key.workspace_id == workspace_id)
            .map(|profile| serde_json::to_value(profile.as_ref()))
            .collect::<Result<Vec<Value>, _>>()
            .map_err(|error| {
                orbit_core::OrbitError::Execution(format!("encode resource profiles: {error}"))
            })?;
        let settings = runtime.auto_concurrency_settings();
        Ok(json!({
            "workspace_id": workspace_id,
            "built_at": set.built_at,
            "host": runtime.host_capacity(),
            "admit_cpu_percent": settings.admit_cpu_percent,
            "admit_memory_percent": settings.admit_memory_percent,
            "profile_window": settings.profile_window,
            // Preserve the requested selection, including the legacy `all`
            // alias, and disclose its effective bound independently.
            "last": selected,
            "delivery_limit": window.unwrap_or(settings.profile_window as usize),
            "max_last": MAX_LAST,
            "min_profile_deliveries": settings.min_profile_deliveries,
            "horizon_cap_minutes": settings.horizon_cap_minutes,
            "profiles": profiles,
        }))
    })
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}
