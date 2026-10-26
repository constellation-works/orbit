//! Lease-only HTTP routes shared by the dashboard and the lease listener.
//!
//! The runtime operations own the JSON input/output contract. Backends return
//! those values unchanged, including structured refusals such as `granted: false`.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use orbit_common::governance::authorization::DASHBOARD_LEASE_WRITE;
use orbit_common::protocol::tool_input::optional_string_alias;
use orbit_core::runtime::task::locks::lease;
use orbit_core::{OrbitError, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::routines::{authorization_denied, authorized_caller};
use super::{bad_request, map_runtime_error, server_error};
use crate::state::DashboardState;

/// Caller attribution and workspace selection for a lease operation.
///
/// The dashboard accepts these in the query string; JSON operations may also
/// declare `principal` in their body. A listener must authenticate and bind
/// these declarations to its token before invoking its backend. As on SSH,
/// dashboard declarations are attribution, not authentication.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LeaseCaller {
    /// Declared person or agent name; required by the request operation.
    pub principal: Option<String>,
    /// Workspace selector, or the server's default when omitted.
    pub workspace: Option<String>,
}

/// A runtime-contract JSON answer, or an HTTP error using the web error mapping.
/// Structured operation refusals remain JSON answers, not transport errors.
pub type LeaseBackendResult = Result<Value, Box<Response>>;

/// Operations needed by the lease-only HTTP router.
///
/// Methods run on Tokio's blocking pool, so implementations may use the runtime
/// or blocking intake-socket I/O. Each method receives the caller separately
/// from the operation input. Implementations enforce their own write authority;
/// the dashboard backend requires its operator session. Authentication and
/// transport-specific Origin checks belong to the mounting server.
pub trait LeaseBackend: Send + Sync + 'static {
    /// Request a lease, returning the `orbit.lease.request` contract.
    fn request(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult;
    /// Renew the URL's lease, returning the `orbit.lease.renew` contract.
    fn renew(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult;
    /// Expand a lease; unavailable until the expansion phase.
    fn expand(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult;
    /// Release the URL's lease, returning the `orbit.lease.release` contract.
    fn release(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult;
    /// Show the URL's lease, returning the `orbit.lease.show` contract.
    fn show(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult;
    /// List leases, returning the `orbit.lease.list` contract.
    fn list(&self, caller: &LeaseCaller) -> LeaseBackendResult;
    /// Check paths, returning the `orbit.lease.check` contract.
    fn check(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult;
}

/// Build only the `/leases` routes over a caller-supplied backend.
///
/// Nest this router under `/api`. It contains no dashboard, health, notices or
/// event routes and applies no dashboard session or localhost middleware.
pub fn lease_router(backend: impl LeaseBackend) -> Router {
    let backend: Arc<dyn LeaseBackend> = Arc::new(backend);
    Router::new()
        .route("/leases", get(list_leases).post(request_lease))
        .route("/leases/check", post(check_leases))
        .route("/leases/{id}", get(show_lease).delete(release_lease))
        .route("/leases/{id}/renew", post(renew_lease))
        .route("/leases/{id}/expand", post(expand_lease))
        .with_state(backend)
}

/// The dashboard's runtime adapter; the listener supplies its own backend.
pub(super) struct RuntimeLeaseBackend {
    pub(super) state: DashboardState,
}

impl RuntimeLeaseBackend {
    fn lease_runtime(
        &self,
        caller: &LeaseCaller,
        write: bool,
    ) -> Result<Arc<OrbitRuntime>, Box<Response>> {
        if write {
            authorized_caller(&DASHBOARD_LEASE_WRITE, self.state.operator_session())
                .map_err(|denial| Box::new(authorization_denied(denial)))?;
        }
        let pinned = self.state.pin();
        let workspace = caller
            .workspace
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| pinned.default_workspace())
            .ok_or_else(|| {
                Box::new(bad_request(
                    "no workspace selected and no default is configured; pass ?workspace=<id>"
                        .to_string(),
                ))
            })?;
        pinned
            .runtime_for(workspace)
            .map_err(|error| Box::new(error.into_response()))
    }
}

impl LeaseBackend for RuntimeLeaseBackend {
    fn request(&self, caller: &LeaseCaller, mut input: Value) -> LeaseBackendResult {
        let runtime = self.lease_runtime(caller, true)?;
        if let Some(principal) = &caller.principal {
            input["principal"] = json!(principal);
        }
        lease::request(&runtime, input, None, None).map_err(|e| Box::new(map_runtime_error(e)))
    }

    fn renew(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        let runtime = self.lease_runtime(caller, true)?;
        lease::renew(&runtime, input).map_err(|e| Box::new(map_runtime_error(e)))
    }

    fn expand(&self, _caller: &LeaseCaller, _input: Value) -> LeaseBackendResult {
        authorized_caller(&DASHBOARD_LEASE_WRITE, self.state.operator_session())
            .map_err(|denial| Box::new(authorization_denied(denial)))?;
        Err(Box::new(
            (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "lease expansion is not available in this phase",
                    "code": "not_available_in_this_phase",
                })),
            )
                .into_response(),
        ))
    }

    fn release(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        let runtime = self.lease_runtime(caller, true)?;
        lease::release(&runtime, input, None, None).map_err(|e| Box::new(map_runtime_error(e)))
    }

    fn show(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        let runtime = self.lease_runtime(caller, false)?;
        lease::show(&runtime, input).map_err(|e| Box::new(map_runtime_error(e)))
    }

    fn list(&self, caller: &LeaseCaller) -> LeaseBackendResult {
        let runtime = self.lease_runtime(caller, false)?;
        lease::list(&runtime).map_err(|e| Box::new(map_runtime_error(e)))
    }

    fn check(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        let runtime = self.lease_runtime(caller, false)?;
        lease::check(&runtime, input).map_err(|e| Box::new(map_runtime_error(e)))
    }
}

fn lease_call_input(mut caller: LeaseCaller, input: &Value) -> Result<LeaseCaller, Box<Response>> {
    if !input.is_object() {
        return Err(Box::new(bad_request(
            "lease input must be a JSON object".to_string(),
        )));
    }
    let principal =
        optional_string_alias(input, &["principal"]).map_err(|e| Box::new(map_runtime_error(e)))?;
    if let Some(principal) = principal {
        if caller
            .principal
            .as_ref()
            .is_some_and(|declared| declared != &principal)
        {
            return Err(Box::new(bad_request(
                "query and body principal must match".to_string(),
            )));
        }
        caller.principal = Some(principal);
    }
    Ok(caller)
}

async fn lease_backend_call(
    backend: Arc<dyn LeaseBackend>,
    caller: LeaseCaller,
    input: Value,
    operation: fn(&dyn LeaseBackend, &LeaseCaller, Value) -> LeaseBackendResult,
) -> Response {
    let caller = match lease_call_input(caller, &input) {
        Ok(caller) => caller,
        Err(error) => return *error,
    };
    match tokio::task::spawn_blocking(move || operation(backend.as_ref(), &caller, input)).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => *error,
        Err(error) => server_error(OrbitError::Execution(format!(
            "lease operation panicked: {error}"
        ))),
    }
}

async fn request_lease(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
    Json(input): Json<Value>,
) -> Response {
    lease_backend_call(backend, caller, input, |b, c, i| b.request(c, i)).await
}

async fn renew_lease(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let mut input = body.map_or_else(|| json!({}), |Json(input)| input);
    if !input.is_object() {
        return bad_request("lease input must be a JSON object".to_string());
    }
    input["lease_id"] = json!(id);
    lease_backend_call(backend, caller, input, |b, c, i| b.renew(c, i)).await
}

async fn expand_lease(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let mut input = body.map_or_else(|| json!({}), |Json(input)| input);
    if !input.is_object() {
        return bad_request("lease input must be a JSON object".to_string());
    }
    input["lease_id"] = json!(id);
    lease_backend_call(backend, caller, input, |b, c, i| b.expand(c, i)).await
}

async fn release_lease(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
    Path(id): Path<String>,
) -> Response {
    lease_backend_call(backend, caller, json!({"lease_id": id}), |b, c, i| {
        b.release(c, i)
    })
    .await
}

async fn show_lease(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
    Path(id): Path<String>,
) -> Response {
    lease_backend_call(backend, caller, json!({"lease_id": id}), |b, c, i| {
        b.show(c, i)
    })
    .await
}

async fn list_leases(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
) -> Response {
    lease_backend_call(backend, caller, json!({}), |b, c, _| b.list(c)).await
}

async fn check_leases(
    State(backend): State<Arc<dyn LeaseBackend>>,
    Query(caller): Query<LeaseCaller>,
    Json(input): Json<Value>,
) -> Response {
    lease_backend_call(backend, caller, input, |b, c, i| b.check(c, i)).await
}
