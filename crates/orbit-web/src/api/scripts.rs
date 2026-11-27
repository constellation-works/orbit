//! The Automation › Scripts view (script steps §2, phase 4).
//!
//! * `GET /api/scripts` — each workspace script's grant state (granted,
//!   stale with the commit that changed it, ungranted, revoked), the diff
//!   against the last granted bytes, requested against granted capabilities
//!   and its open `script_grant` request. Safe for every session.
//! * `POST /api/scripts/<name>/grant` — the operator-session grant action,
//!   bound to the digest the operator reviewed. It answers the open request
//!   when one exists, so the grant and the decision record agree.

use crate::state::run_read;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_SCRIPT_GRANT;
use orbit_core::app::run::{Command as RunCommand, DashboardScriptGrant};
use serde::Deserialize;
use serde_json::{Value, json};

use super::routines::{action_capability, dashboard_caller};
use super::tasks::human_comment_author;
use super::{bad_request, blocking, map_runtime_error, non_empty_string};
use crate::state::{DashboardState, Ws, WsCore};

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
    match blocking("script list", move || {
        run_read(&runtime, RunCommand::script_grant_overview())
    })
    .await
    {
        Ok(scripts) => Json(json!({
            "scripts": scripts,
            "capabilities": {"grant": action_capability(&DASHBOARD_SCRIPT_GRANT, operator)},
        }))
        .into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/scripts/:name/grant` — operator session only. The run app
/// authorizes the session, grants and writes the audit row.
pub(super) async fn grant_script(
    State(state): State<DashboardState>,
    WsCore(core): WsCore,
    Path(name): Path<String>,
    Json(body): Json<ScriptGrantBody>,
) -> Response {
    let origin = dashboard_caller(&state, None, "");
    let reviewed = body.digest.as_deref().and_then(non_empty_string);
    let grantor = human_comment_author(body.author.as_deref());
    let result = blocking("script grant", move || {
        run_read(
            &core,
            RunCommand::dashboard_script_grant(origin, &name, body.digest, reviewed, grantor),
        )
    })
    .await;
    match result {
        Ok(DashboardScriptGrant::Granted(grant)) => {
            Json(json!({"grant": serde_json::to_value(&grant).unwrap_or(Value::Null)}))
                .into_response()
        }
        Ok(DashboardScriptGrant::DigestMissing) => {
            bad_request("digest must name the reviewed script digest".to_string())
        }
        Ok(DashboardScriptGrant::Refused(error)) => map_runtime_error(error),
        Err(response) => *response,
    }
}
