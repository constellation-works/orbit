use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};

use super::blocking;
use crate::state::DashboardState;

/// Always reports the serving host, without a workspace extractor or remote routing.
pub(super) async fn resources(State(state): State<DashboardState>) -> Response {
    match blocking("host resource sampling", move || {
        state.host_resource_status()
    })
    .await
    {
        Ok(status) => Json(status).into_response(),
        Err(response) => *response,
    }
}
