//! `GET /api/resource-profiles`: Analytics › Resources. The selected
//! workspace's delivery resource profiles on this host, the same set admission
//! prices candidates with, beside the host's size and admit thresholds.

use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};

use super::blocking;
use crate::state::Ws;

/// Each profile is the object `orbit run profiles --json` emits. Profiles are
/// per host (each host keeps its own store), so this serving host's set is the
/// only one in the answer; another host's comes through its forward.
pub(super) async fn resource_profiles(Ws(runtime): Ws) -> Response {
    match blocking("resource profiles", move || {
        let workspace_id = runtime.workspace_id()?;
        let set = runtime.resource_profiles()?;
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
