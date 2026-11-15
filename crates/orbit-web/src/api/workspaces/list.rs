//! `GET /api/workspaces` and the workspace ownership and path helpers the
//! aggregate endpoints share.

use std::path::Path;

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde_json::{Value, json};

use super::super::blocking;
use super::fan_out::fan_out;
use crate::state::DashboardState;

/// `GET /api/workspaces` — list every workspace the dashboard can serve, with
/// the currently-selected default flagged.
///
/// Filesystem paths (`root`, `orbit_dir`) are home-abbreviated to `~` so the
/// frontend can render the selected workspace's location directly, without
/// needing to know the server's home directory (ORB-00037).
pub(in crate::api) async fn list_workspaces(State(state): State<DashboardState>) -> Response {
    match blocking("list workspaces", move || Ok(list_workspaces_json(&state))).await {
        Ok(values) => Json(Value::Array(values)).into_response(),
        Err(response) => *response,
    }
}

/// Whether this host owns the checkout for `entry`. A binding with no
/// recorded role (single-host installs, older registrations) counts as owned.
pub(crate) fn entry_is_owned(entry: &crate::state::WsEntry) -> bool {
    entry
        .binding
        .as_ref()
        .and_then(|binding| binding.checkout_role)
        != Some(WorkspaceCheckoutRole::Replica)
}

fn list_workspaces_json(state: &DashboardState) -> Vec<Value> {
    // Refresh and pin one snapshot so the listing and its `is_default` flags all
    // reflect the same generation (add/remove/rebind observed atomically).
    let pinned = state.pin();
    let default = pinned.default_workspace();
    let home = orbit_common::fs::path::home_dir().ok();
    let entries = pinned.entries();
    // `lease_enabled` lets the dashboard hide its Leases destination where
    // leases are off; null when the workspace's runtime can't be opened, so an
    // unknown setting reads as today's behaviour (shown).
    let lease_enabled = fan_out(entries.len(), |index| {
        let entry = &entries[index];
        if !entry.active {
            return None;
        }
        let runtime = pinned.runtime_for(&entry.id).ok()?;
        Some(runtime.leases_enabled())
    });
    entries
        .iter()
        .zip(lease_enabled)
        .map(|(entry, lease_enabled)| {
            json!({
                "id": entry.id,
                "creation_binding": super::super::tasks::create::binding_token(entry),
                "name": entry.name,
                "root": abbreviate_home(&entry.repo_root, home.as_deref()),
                "orbit_dir": abbreviate_home(&entry.orbit_dir, home.as_deref()),
                "status": if entry.active { "active" } else { "invalid" },
                "is_default": default == Some(entry.id.as_str()),
                "lease_enabled": lease_enabled,
                "checkout_role": entry.binding.as_ref().and_then(|binding| binding.checkout_role).map(|role| role.as_str()),
                "owner_machine_id": entry.binding.as_ref().and_then(|binding| binding.owner_machine_id.clone()),
            })
        })
        .collect()
}

/// Render a filesystem path for display, collapsing the user's home directory
/// to `~` (e.g. `/home/dan/ws/orbit` → `~/ws/orbit`). Paths outside `home`, an
/// unset `home`, or an empty path (the single-mode synthetic entry) are rendered
/// verbatim. ORB-00037.
pub(in crate::api) fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}
