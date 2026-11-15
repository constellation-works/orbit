//! Combined transport for the dashboard's simultaneous advisory reads.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Query, State};
use axum::http::Request;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{audit, jobs, notices, routines};
use crate::state::DashboardState;

/// Only the scheduled reads that are due are included; notices are always due.
#[derive(Default, Deserialize)]
pub(super) struct DashboardChromeQuery {
    workspace: Option<String>,
    since: Option<String>,
    #[serde(default)]
    routines: bool,
    #[serde(default)]
    failures: bool,
}

// Dispatch through the original handlers and extractors, retaining their JSON
// bytes and status independently. This router admits only four fixed GETs;
// the caller cannot supply paths, methods, refresh flags or write operations.
async fn chrome_reply(router: Router, path: &str, workspace: Option<&str>) -> Value {
    let mut uri = path.to_string();
    if let Some(workspace) = workspace {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("workspace", workspace)
            .finish();
        uri.push(if path.contains('?') { '&' } else { '?' });
        uri.push_str(&query);
    }
    let response = match Request::get(uri).body(Body::empty()) {
        Ok(request) => match router.oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        },
        Err(error) => {
            return json!({"status": 500, "body": json!({"error": error.to_string()}).to_string()});
        }
    };
    let status = response.status().as_u16();
    match to_bytes(response.into_body(), usize::MAX).await {
        Ok(body) => json!({"status": status, "body": String::from_utf8_lossy(&body)}),
        Err(error) => {
            json!({"status": 500, "body": json!({"error": error.to_string()}).to_string()})
        }
    }
}

/// `GET /api/dashboard/chrome`: one transport, independent component outcomes.
pub(super) async fn dashboard_chrome(
    State(state): State<DashboardState>,
    Query(query): Query<DashboardChromeQuery>,
) -> Response {
    let router = Router::new()
        .route("/notices", get(notices::list_notices))
        .route("/summary", get(audit::audit_summary))
        .route("/routines", get(routines::list_routine_health))
        .route("/failures", get(jobs::list_job_failure_streaks))
        .with_state(state);
    let workspace = query.workspace.as_deref();
    let summary_query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("since", query.since.as_deref().unwrap_or("24h"))
        .finish();
    let summary_path = format!("/summary?{summary_query}");
    let (notices, summary, routines, failures) = tokio::join!(
        chrome_reply(router.clone(), "/notices", workspace),
        chrome_reply(router.clone(), &summary_path, workspace),
        async {
            if query.routines {
                chrome_reply(router.clone(), "/routines", workspace).await
            } else {
                Value::Null
            }
        },
        async {
            if query.failures {
                chrome_reply(router.clone(), "/failures", workspace).await
            } else {
                Value::Null
            }
        },
    );
    Json(
        json!({"notices": notices, "summary": summary, "routines": routines, "failures": failures}),
    )
    .into_response()
}
