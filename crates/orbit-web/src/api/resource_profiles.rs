//! `GET /api/resource-profiles`: Analytics › Resources. The selected
//! workspace's delivery resource profiles on this host, the same set admission
//! prices candidates with, beside the host's size and admit thresholds.
//! `?last=<n>` (or `last=all`) instead profiles each key's last `n`
//! retained deliveries, up to 1,000; `all` is a compatibility alias for that
//! maximum, without changing admission's set. `delivery_limit` discloses the
//! effective per-key bound and `max_last` the maximum explicit selection.

use axum::extract::Query;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::runtime::host_resource::forecast::HostCapacity;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::RawValue;

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
        // Encoded once per cached set and workspace, not on every read.
        let profiles = set.workspace_profiles_json(&workspace_id)?;
        let settings = runtime.auto_concurrency_settings();
        serde_json::to_vec(&ResourceProfilesBody {
            workspace_id: &workspace_id,
            built_at: set.built_at,
            host: runtime.host_capacity(),
            admit_cpu_percent: settings.admit_cpu_percent,
            admit_memory_percent: settings.admit_memory_percent,
            profile_window: settings.profile_window,
            last: selected,
            delivery_limit: window.unwrap_or(settings.profile_window as usize),
            max_last: MAX_LAST,
            min_profile_deliveries: settings.min_profile_deliveries,
            horizon_cap_minutes: settings.horizon_cap_minutes,
            profiles: &profiles,
        })
        .map_err(|error| {
            orbit_core::OrbitError::Execution(format!("encode resource profiles: {error}"))
        })
    })
    .await
    {
        Ok(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Err(response) => *response,
    }
}

/// The answer, in the order its keys are written.
#[derive(Serialize)]
struct ResourceProfilesBody<'a> {
    workspace_id: &'a str,
    built_at: DateTime<Utc>,
    host: Option<HostCapacity>,
    admit_cpu_percent: u8,
    admit_memory_percent: u8,
    profile_window: u32,
    /// The requested selection, including the legacy `all` alias; its
    /// effective bound is `delivery_limit`.
    last: Option<String>,
    delivery_limit: usize,
    max_last: usize,
    min_profile_deliveries: u32,
    horizon_cap_minutes: u32,
    profiles: &'a RawValue,
}
