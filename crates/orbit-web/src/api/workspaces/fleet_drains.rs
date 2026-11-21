//! `GET /api/fleet/drains`: one readiness row per registered workspace.

use std::collections::{BTreeMap, HashSet};

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::FleetReadinessFacts;
use serde_json::{Value, json};

use super::super::blocking;
use super::fan_out::fan_out;
use crate::state::{DashboardState, Pinned};

/// `GET /api/fleet/drains` — one observational readiness row per registered
/// workspace on this host. Unavailable workspaces remain explicit rows. Remote
/// hosts expose the same endpoint through the existing single-hop forward.
pub(in crate::api) async fn list_fleet_drains(State(state): State<DashboardState>) -> Response {
    match blocking("fleet drains", move || {
        let pinned = state.pin();
        let entries = pinned.entries();
        // Every workspace here shares one task registry, so the first runtime
        // that opens reads its global facts for all of them.
        let facts = entries
            .iter()
            .filter(|entry| entry.active)
            .find_map(|entry| pinned.runtime_for(&entry.id).ok())
            .map(|runtime| runtime.fleet_readiness_facts())
            .unwrap_or_default();
        // One readiness snapshot per workspace, computed concurrently: the
        // row order stays the pinned entries' order.
        let items = fan_out(entries.len(), |index| {
            fleet_drain_row(&pinned, &entries[index], &facts)
        });
        Ok::<_, orbit_core::OrbitError>(json!({"items": items}))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn fleet_drain_row(
    pinned: &Pinned,
    entry: &crate::state::WsEntry,
    facts: &FleetReadinessFacts,
) -> Value {
    let mut row = json!({
        "workspace_id": entry.id,
        "workspace_name": entry.name,
        "phase": "unavailable",
        "slots_used": null,
        "slots_ceiling": null,
        "eligible_count": null,
        "top_wait_reason": null,
        "baseline_red_hold": null,
        "holds": [],
        "queued_drains": [],
        "task_pilot_warning": null,
        "drain_phase": null,
        "pull_active": null,
        "pull_admissions_stopped": null,
    });
    let readiness = if entry.active {
        pinned
            .runtime_for(&entry.id)
            .map_err(|error| orbit_core::OrbitError::Execution(format!("{error:?}")))
            .and_then(|runtime| {
                let mut payload = runtime.workspace_fleet_readiness(facts)?;
                payload["owner_machine_id"] = json!(runtime.replica_owner_machine());
                Ok(payload)
            })
    } else {
        row["error"] = json!("workspace is unavailable");
        return row;
    };
    match readiness {
        Ok(payload) => {
            let tasks = payload["tasks"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let eligible = tasks.iter().filter(|task| task["eligible"] == true).count();
            let mut waits = BTreeMap::<&str, usize>::new();
            for task in tasks.iter().filter(|task| task["eligible"] != true) {
                if let Some(reason) = task["reason"].as_str() {
                    *waits.entry(reason).or_default() += 1;
                }
            }
            // Stable alphabetical tie-break; never return task-specific
            // detail (which can contain internal task identifiers).
            let top = waits
                .iter()
                .max_by(|(a, ac), (b, bc)| ac.cmp(bc).then_with(|| b.cmp(a)))
                .map(|(reason, _)| *reason);
            let capacity = &payload["capacity"];
            let mut holds = Vec::new();
            if capacity["admissions_stopped"] == true
                || capacity["pull_drain_admissions_stopped"] == true
            {
                holds.push(json!("admissions stopped"));
            }
            if capacity["host_shutdown_holding"] == true {
                holds.push(json!("host shutdown scheduled"));
            }
            if let Some(resources) = capacity["resource_throttle"]["resources"].as_array() {
                for resource in resources {
                    if let Some(name) = resource["resource"].as_str() {
                        holds.push(json!(format!("{name} throttled")));
                    }
                }
            }
            // A crew that sets `env` is its own account, `provider/crew`.
            let account = |entry: &Value| {
                let provider = entry["provider"].as_str()?;
                Some(orbit_types::workflow::provider_account_label(
                    provider,
                    entry["account"].as_str(),
                ))
            };
            for limit in payload["provider_limits"].as_array().into_iter().flatten() {
                if limit["gated"] == true
                    && let Some(account) = account(limit)
                {
                    holds.push(json!(format!("{account} usage limit")));
                }
            }
            for failure in payload["provider_auth_failures"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(account) = account(failure) {
                    holds.push(json!(format!("{account} authentication failed")));
                }
            }
            let phase =
                if capacity["drain_phase"] == "idle" && capacity["pull_drain_run_id"].is_string() {
                    "pulling"
                } else {
                    capacity["drain_phase"].as_str().unwrap_or("idle")
                };
            row["phase"] = json!(phase);
            // Keep the independent drain and pull states for host diagnostics;
            // the fleet's combined `phase` alone loses a concurrent pull.
            row["drain_phase"] = json!(capacity["drain_phase"].as_str().unwrap_or(
                if capacity["drain_run_id"].is_string() {
                    "draining"
                } else {
                    "idle"
                }
            ));
            row["pull_active"] = json!(capacity["pull_drain_run_id"].is_string());
            row["pull_admissions_stopped"] =
                json!(capacity["pull_drain_admissions_stopped"] == true);
            // Keep concurrent owner and pull windows distinguishable in Fleet.
            row["owner_admissions_stopped"] = json!(capacity["admissions_stopped"] == true);
            row["drain_run_id"] = capacity["drain_run_id"].clone();
            row["pull_drain_run_id"] = capacity["pull_drain_run_id"].clone();
            row["owner_running_tasks"] = capacity["running_admitted_workers"].clone();
            row["pull_running_tasks"] = capacity["wrapper_leaf_runs"].clone();
            row["owner_machine_id"] = payload["owner_machine_id"].clone();
            // Pull coordinators execute the local workspace's claimed leaves,
            // rather than admitting owner workers into their own checkpoint.
            row["running_tasks"] = if phase == "pulling" {
                capacity["wrapper_leaf_runs"].clone()
            } else {
                capacity["running_admitted_workers"].clone()
            };
            row["slots_used"] = capacity["active_leaf_runs"].clone();
            row["slots_ceiling"] = capacity["max_active_leaf_runs"].clone();
            row["eligible_count"] = json!(eligible);
            // Only the held task ids leave the row, never task detail text.
            // `complete` is false when the bounded listing truncated the
            // backlog, so a consumer needing the observed-id set must fall
            // back to the full readiness read.
            let mut hold_seen = HashSet::new();
            let baseline_red_hold_ids: Vec<&str> = tasks
                .iter()
                .filter(|task| task["reason"] == "baseline_red_hold")
                .filter_map(|task| task["task_id"].as_str())
                .filter(|id| !id.is_empty() && hold_seen.insert(*id))
                .collect();
            row["baseline_red_hold"] = json!({
                "task_ids": baseline_red_hold_ids,
                "complete": payload["total"].as_u64().unwrap_or(0) <= tasks.len() as u64,
            });
            row["counts_truncated"] =
                json!(payload["total"].as_u64().unwrap_or(0) > tasks.len() as u64);
            row["top_wait_reason"] = json!(top);
            row["holds"] = json!(holds);
            // Windows waiting for the live one's slot, as readiness reports
            // them: run ids, policy and crew names, never env values.
            row["queued_drains"] = capacity["queued_drains"].clone();
            // The live drain's own record that no task pilot runs here.
            row["task_pilot_warning"] = capacity["task_pilot_warning"].clone();
        }
        Err(error) => {
            tracing::warn!(workspace = %entry.id, %error, "fleet readiness unavailable");
            row["error"] = json!("readiness unavailable");
        }
    }
    row
}
