//! Strict proposed-task capture. Legacy task creation remains independent.

use axum::{
    extract::{RawQuery, State, rejection::JsonRejection},
    response::{IntoResponse, Json, Response},
};
use orbit_common::{
    OrbitError,
    governance::authorization::{
        AuthorizationDenial, CallerCapabilities, CallerEnvelope, DESKTOP_TASK_EDIT, authorize,
    },
    security::release::sha256_hex,
};
use orbit_core::application::task::ProposedTaskInput;
use orbit_types::{
    tool::{McpCapability, ToolSessionContext},
    workspace::WorkspaceCheckoutRole,
};
use serde_json::{Value, json};

use super::{bad_request, blocking};
use crate::state::{DashboardState, WsEntry};

/// Content identity of the exact registered destination, stable across server
/// restarts. Snapshot generation is checked separately within each request.
pub(super) fn binding_token(entry: &WsEntry) -> Option<String> {
    let binding = entry.binding.as_ref()?;
    let bytes = serde_json::to_vec(&json!({
        "workspace": binding.logical_workspace_id,
        "partition": binding.task_partition_id,
        "owner": binding.owner_machine_id,
        "role": binding.checkout_role,
        "repo_root": binding.repo_root,
        "orbit_dir": entry.orbit_dir,
        "ship_mode": binding.ship_mode,
        "base_branch": binding.base_branch,
    }))
    .ok()?;
    Some(sha256_hex(&bytes))
}

fn authorize_creation(state: &DashboardState) -> Result<(), AuthorizationDenial> {
    let mut session = ToolSessionContext::default();
    if state.operator_session() {
        session
            .effective_capabilities
            .insert(McpCapability::Operator);
    }
    // HTTP callers never inherit the hosting process's managed-agent identity,
    // TTY, override or run capabilities.
    authorize(
        &DESKTOP_TASK_EDIT,
        &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&session)),
    )
}

pub(super) async fn create_proposal(
    State(state): State<DashboardState>,
    RawQuery(query): RawQuery,
    body: Result<Json<ProposedTaskInput>, JsonRejection>,
) -> Response {
    if let Err(denial) = authorize_creation(&state) {
        return super::routines::authorization_denied(denial);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let selections: Vec<String> =
        url::form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
            .filter(|(key, _)| key == "workspace")
            .map(|(_, value)| value.into_owned())
            .collect();
    let [workspace] = selections.as_slice() else {
        return bad_request("select exactly one explicit workspace".into());
    };
    if workspace.trim().is_empty() {
        return bad_request("select exactly one explicit workspace".into());
    }
    let workspace = workspace.clone();
    match blocking("create proposed task", move || {
        Ok((|| {
            authorize_creation(&state)
                .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))?;
            let pinned = state.pin_for_write()?;
            let entry = pinned
                .entries()
                .iter()
                .find(|entry| entry.id == workspace && entry.active)
                .ok_or_else(|| {
                    OrbitError::InvalidInput("select an active registered workspace".into())
                })?;
            if binding_token(entry).as_deref() != Some(body.binding.as_str()) {
                return Err(OrbitError::PolicyDenied(
                    "workspace binding changed; the frozen submission cannot be retargeted".into(),
                ));
            }
            let runtime = pinned
                .runtime_for(&workspace)
                .map_err(|_| OrbitError::PolicyDenied("workspace unavailable".into()))?;
            let binding = entry
                .binding
                .as_ref()
                .ok_or_else(|| OrbitError::PolicyDenied("workspace binding unavailable".into()))?;
            if let Some(refusal) = runtime.coordination_task_write_refusal() {
                return Err(refusal);
            }
            let machine =
                orbit_registry::machine_identity::load_machine_identity(state.global_root())?;
            if binding.checkout_role != Some(WorkspaceCheckoutRole::Owner)
                || binding.owner_machine_id.is_none()
                || binding.owner_machine_id.as_deref() != runtime.automation_machine_identity()
                || binding.owner_machine_id.as_deref() != Some(machine.id.as_str())
            {
                return Err(OrbitError::CapabilityRefused(
                    "task creation requires the workspace owner host".into(),
                ));
            }
            // A runtime build may have waited while the registry changed. Recheck
            // current access and generation before any key lookup or allocation.
            authorize_creation(&state)
                .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))?;
            if !pinned.same_generation(&state.pin_for_write()?) {
                return Err(OrbitError::PolicyDenied(
                    "workspace generation changed; retry the frozen submission".into(),
                ));
            }
            let request_id = body.request_id.clone();
            let id = runtime.create_proposed_task(body)?;
            Ok(json!({"id": id, "workspace_id": workspace, "request_id": request_id}))
        })())
    })
    .await
    {
        Ok(Ok(receipt)) => Json::<Value>(receipt).into_response(),
        Ok(Err(
            error @ (OrbitError::CapabilityDenied(_)
            | OrbitError::CapabilityRefused(_)
            | OrbitError::PolicyDenied(_)),
        )) => (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
        Ok(Err(error)) => super::map_runtime_error(error),
        Err(response) => *response,
    }
}
