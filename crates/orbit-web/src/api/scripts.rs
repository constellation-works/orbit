//! The Automation › Scripts view (script steps §2, phase 4).
//!
//! * `GET /api/scripts` — each workspace script's grant state (granted,
//!   stale with the commit that changed it, ungranted, revoked), the diff
//!   against the last granted bytes, requested against granted capabilities
//!   and its open `script_grant` request. Safe for every session.
//! * `POST /api/scripts/<name>/grant` — the operator-session grant action,
//!   bound to the digest the operator reviewed. It answers the open request
//!   when one exists, so the grant and the decision record agree.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_SCRIPT_GRANT;
use serde::Deserialize;
use serde_json::{Value, json};

use super::routines::{
    action_capability, authorization_denied, authorized_caller, record_operation_audit,
};
use super::tasks::human_comment_author;
use super::{bad_request, blocking, non_empty_string, workspace_label};
use crate::state::{DashboardState, Ws};

/// Body of `POST /api/scripts/:name/grant`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ScriptGrantBody {
    /// The digest the operator reviewed; a script whose bytes moved on is
    /// refused rather than granted.
    #[serde(default)]
    digest: Option<String>,
    /// Requested human grantor label; sanitized like a comment author.
    #[serde(default)]
    author: Option<String>,
}

/// `GET /api/scripts`.
pub(super) async fn list_scripts(State(state): State<DashboardState>, Ws(runtime): Ws) -> Response {
    let operator = state.operator_session();
    match blocking("script list", move || runtime.script_grant_overview()).await {
        Ok(scripts) => Json(json!({
            "scripts": scripts,
            "capabilities": {"grant": action_capability(&DASHBOARD_SCRIPT_GRANT, operator)},
        }))
        .into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/scripts/:name/grant` — operator session only.
pub(super) async fn grant_script(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(name): Path<String>,
    Json(body): Json<ScriptGrantBody>,
) -> Response {
    let started = Instant::now();
    let workspace = workspace_label(&runtime).await;
    let arguments = json!({"digest": body.digest});
    let caller = match authorized_caller(&DASHBOARD_SCRIPT_GRANT, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                DASHBOARD_SCRIPT_GRANT.id,
                &name,
                "",
                &arguments,
                None,
                Some(&denial),
                None,
                started,
            )
            .await;
            return authorization_denied(denial);
        }
    };
    let Some(digest) = body.digest.as_deref().and_then(non_empty_string) else {
        return bad_request("digest must name the reviewed script digest".to_string());
    };
    let grantor = human_comment_author(body.author.as_deref());
    let writer = std::sync::Arc::clone(&runtime);
    let target = name.clone();
    let result = blocking("script grant", move || {
        writer.grant_script_from_dashboard(&target, &digest, grantor)
    })
    .await;
    let failure = result.as_ref().err().map(|_| "script grant refused");
    record_operation_audit(
        &runtime,
        &workspace,
        DASHBOARD_SCRIPT_GRANT.id,
        &name,
        "",
        &arguments,
        Some(&caller),
        None,
        failure,
        started,
    )
    .await;
    match result {
        Ok(grant) => Json(json!({"grant": serde_json::to_value(&grant).unwrap_or(Value::Null)}))
            .into_response(),
        Err(response) => *response,
    }
}
