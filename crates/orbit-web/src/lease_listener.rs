//! The lease listener (`docs/design/path-leases/2_design.md` §7.1–§7.3).
//!
//! `orbit lease listen` runs this as the lease listener user (`orbit-lease`
//! by convention), never as the operator, so agents running as the operator
//! can neither mint its tokens nor run code as it. It serves the lease
//! routes under `/api` and `/healthz`, and nothing else: no dashboard,
//! cookie, browser session or CORS. Every lease request needs a bearer lease
//! token, verified through `orbit-auth` against the token store in the
//! running user's Orbit home, and is forwarded with the token's principal,
//! id and scope and the resolved client address to Orbit Web over the lease
//! intake socket. The listener never opens Orbit's main store.
//!
//! Refusals that never reach Orbit (an unresolvable client address, a rate
//! limit, a missing, unknown, revoked or out-of-scope token, a request
//! rejected after authentication) are audited in the listener user's token
//! store with the remote address, route and reason, never a secret. Orbit
//! audits every operation it receives.

use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use chrono::Utc;
use orbit_auth::client_address::{
    LeaseClientAddressPolicy, TrustedProxies, resolve_lease_client_address,
};
use orbit_auth::environment::{LISTENER_USER, check_listener_execution_environment};
use orbit_auth::limiter::{FailedAuthAdmission, FailedAuthLimiter};
use orbit_auth::scope::{LeaseTokenRefusal, LeaseTokenVerb};
use orbit_auth::store::{
    LeaseListenerRefusal, LeaseRequestOrigin, LeaseTokenDecision, LeaseTokenStore,
};
use orbit_config::LeaseListenerSettings;
use orbit_core::OrbitError;
use serde_json::{Value, json};

use crate::api::leases::{
    LeaseBackend, LeaseBackendResult, LeaseCaller, LeaseCallerToken, lease_router,
};
use crate::lease_intake::{
    LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse,
};

/// How long the listener waits for each intake frame.
const INTAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Client addresses the failed-authentication limiter tracks at once.
const FAILED_AUTH_TRACKED_ADDRESSES: usize = 4096;

/// Longest route recorded in an audit row.
const AUDIT_ROUTE_MAX_BYTES: usize = 256;

/// The listener's validated configuration.
#[derive(Debug, Clone)]
pub struct LeaseListenerConfig {
    /// Address to listen on.
    pub bind: SocketAddr,
    /// How the client address is found.
    pub client_address: LeaseClientAddressPolicy,
    /// The operator's lease intake socket.
    pub intake_socket: PathBuf,
    /// Failed authentications from one address before it gets 429.
    pub failed_auth_limit: u32,
    /// The window those failures are counted over.
    pub failed_auth_window: Duration,
}

impl LeaseListenerConfig {
    /// Validate `lease.listener.*`, refusing an unsafe or incomplete
    /// combination with a message naming the setting.
    pub fn from_settings(settings: &LeaseListenerSettings) -> Result<Self, OrbitError> {
        if settings.tls_configured {
            return Err(refused(
                "lease.listener.tls is set, but native TLS on the lease listener arrives in a \
                 later phase. Unset it, and terminate TLS at a proxy with \
                 lease.listener.behind_proxy = true and lease.listener.trusted_proxy, or bind a \
                 loopback or private address",
            ));
        }
        let Some(raw_bind) = settings.bind.as_deref() else {
            return Err(refused(
                "lease.listener.bind is unset, so there is no listener. Set it in the listener \
                 user's ~/.orbit/config.toml, for example bind = \"100.64.0.5:7480\"",
            ));
        };
        let bind: SocketAddr = raw_bind.parse().map_err(|_| {
            refused(&format!(
                "lease.listener.bind = \"{raw_bind}\" is not an IP address and port, such as \
                 127.0.0.1:7480 or [::1]:7480"
            ))
        })?;
        let client_address = if settings.behind_proxy {
            if settings.trusted_proxy.is_empty() {
                return Err(refused(
                    "lease.listener.behind_proxy = true needs lease.listener.trusted_proxy: list \
                     the proxy addresses or CIDR ranges whose X-Forwarded-For may be read",
                ));
            }
            LeaseClientAddressPolicy::BehindProxy(TrustedProxies::parse(&settings.trusted_proxy)?)
        } else {
            LeaseClientAddressPolicy::Direct
        };
        if bind.ip().is_unspecified() && !settings.behind_proxy {
            return Err(refused(&format!(
                "lease.listener.bind = \"{raw_bind}\" listens on every interface without TLS. \
                 Bind a specific address, or put a terminating proxy in front and set \
                 lease.listener.behind_proxy = true"
            )));
        }
        let Some(intake_socket) = settings.intake_socket.as_deref() else {
            return Err(refused(
                "lease.listener.intake_socket is unset: name the operator's lease intake socket, \
                 such as /home/<operator>/.orbit/run/lease-intake.sock",
            ));
        };
        let intake_socket = PathBuf::from(intake_socket);
        if !intake_socket.is_absolute() {
            return Err(refused(&format!(
                "lease.listener.intake_socket = \"{}\" must be an absolute path",
                intake_socket.display()
            )));
        }
        Ok(Self {
            bind,
            client_address,
            intake_socket,
            failed_auth_limit: settings.failed_auth_limit,
            failed_auth_window: Duration::from_secs(u64::from(settings.failed_auth_window_seconds)),
        })
    }
}

fn refused(detail: &str) -> OrbitError {
    OrbitError::InvalidInput(format!("refusing to start the lease listener: {detail}"))
}

/// `orbit lease listen`: check the execution environment and the intake
/// socket's owner, read `lease.listener.*` from the running user's Orbit
/// home, open its token store, and serve until SIGINT or SIGTERM.
pub fn run_lease_listener() -> Result<(), OrbitError> {
    let orbit_home = orbit_common::fs::path::global_orbit_dir()?;
    check_listener_execution_environment(&orbit_home)?;
    let settings = orbit_config::load_lease_listener_settings(&orbit_home)?;
    let config = LeaseListenerConfig::from_settings(&settings)?;
    refuse_intake_socket_owner(&config.intake_socket)?;
    let store = LeaseTokenStore::open_in_orbit_home(&orbit_home)?;
    serve_lease_listener(config, store)
}

/// Refuse to run as the user that owns the intake socket: that is the
/// operator, whose agents must not run the listener. When the socket is not
/// there yet, its directory's owner is checked instead.
fn refuse_intake_socket_owner(socket: &Path) -> Result<(), OrbitError> {
    let (inspected, metadata) = match std::fs::metadata(socket) {
        Ok(metadata) => (socket, metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let directory = socket.parent().unwrap_or(socket);
            let metadata = std::fs::metadata(directory).map_err(|error| {
                refused(&format!(
                    "cannot inspect the lease intake socket {} or its directory: {error}. Set \
                     lease.listener.intake_socket to the socket Orbit Web serves when the \
                     operator sets lease.intake.peer_user",
                    socket.display()
                ))
            })?;
            (directory, metadata)
        }
        Err(error) => {
            return Err(refused(&format!(
                "cannot inspect the lease intake socket {}: {error}",
                socket.display()
            )));
        }
    };
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() == euid {
        return Err(OrbitError::PolicyDenied(format!(
            "refusing to start the lease listener: {} is owned by the running user (uid {euid}), \
             so this is the operator's user. Run the listener as the listener user \
             ({LISTENER_USER} by convention), for example from its systemd unit",
            inspected.display()
        )));
    }
    Ok(())
}

/// Bind `config.bind` and serve the listener router until SIGINT or SIGTERM.
pub fn serve_lease_listener(
    config: LeaseListenerConfig,
    store: LeaseTokenStore,
) -> Result<(), OrbitError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| OrbitError::Execution(format!("lease listener runtime: {error}")))?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|error| {
                OrbitError::Io(format!(
                    "lease listener cannot bind {} (lease.listener.bind): {error}",
                    config.bind
                ))
            })?;
        let local = listener
            .local_addr()
            .map_err(|error| OrbitError::Io(format!("lease listener: {error}")))?;
        tracing::info!(address = %local, intake = %config.intake_socket.display(), "serving the lease listener");
        #[allow(clippy::print_stdout)]
        {
            println!("Lease listener on http://{local} (lease routes and /healthz only)");
        }
        let app = lease_listener_router(&config, store);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(crate::serve::shutdown_signal())
        .await
        .map_err(|error| OrbitError::Io(format!("lease listener: {error}")))
    })
}

/// The listener's whole router: `/healthz`, and the lease routes under
/// `/api` behind token authentication. Every other path is 404. Serve it
/// with [`ConnectInfo<SocketAddr>`](ConnectInfo); without a peer address a
/// lease request is refused.
pub fn lease_listener_router(config: &LeaseListenerConfig, store: LeaseTokenStore) -> Router {
    let gate = Arc::new(ListenerGate {
        client_address: config.client_address.clone(),
        store: Arc::new(Mutex::new(store)),
        limiter: Mutex::new(FailedAuthLimiter::new(
            config.failed_auth_limit,
            config.failed_auth_window,
            FAILED_AUTH_TRACKED_ADDRESSES,
        )),
    });
    let backend = IntakeLeaseBackend {
        intake_socket: config.intake_socket.clone(),
    };
    Router::new().route("/healthz", get(listener_healthz)).nest(
        "/api",
        lease_router(backend)
            .route_layer(middleware::from_fn_with_state(gate, lease_listener_gate)),
    )
}

/// Liveness only: no version, workspace or token detail.
async fn listener_healthz() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

struct ListenerGate {
    client_address: LeaseClientAddressPolicy,
    store: Arc<Mutex<LeaseTokenStore>>,
    limiter: Mutex<FailedAuthLimiter>,
}

/// A refusal to audit, owned so it can move to the blocking pool.
struct PendingRefusal {
    origin: LeaseRequestOrigin,
    reason: &'static str,
    token_id: Option<String>,
    principal: Option<String>,
    workspace: Option<String>,
    verb: Option<LeaseTokenVerb>,
}

impl PendingRefusal {
    fn anonymous(origin: LeaseRequestOrigin, reason: &'static str) -> Self {
        Self {
            origin,
            reason,
            token_id: None,
            principal: None,
            workspace: None,
            verb: None,
        }
    }
}

impl ListenerGate {
    async fn audit(&self, pending: PendingRefusal) {
        let store = Arc::clone(&self.store);
        let written = tokio::task::spawn_blocking(move || {
            let mut store = store
                .lock()
                .map_err(|_| OrbitError::Execution("lease token store lock poisoned".into()))?;
            store.record_listener_refusal(&LeaseListenerRefusal {
                at: Utc::now(),
                origin: &pending.origin,
                reason: pending.reason,
                token_id: pending.token_id.as_deref(),
                principal: pending.principal.as_deref(),
                workspace: pending.workspace.as_deref(),
                verb: pending.verb,
            })
        })
        .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(%error, "lease listener could not audit a refusal"),
            Err(error) => tracing::error!(%error, "lease listener audit task failed"),
        }
    }

    async fn verify(
        &self,
        secret: String,
        workspace: Option<String>,
        verb: LeaseTokenVerb,
        origin: LeaseRequestOrigin,
    ) -> Result<LeaseTokenDecision, OrbitError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut store = store
                .lock()
                .map_err(|_| OrbitError::Execution("lease token store lock poisoned".into()))?;
            store.verify_listener_request(&secret, workspace.as_deref(), verb, Utc::now(), &origin)
        })
        .await
        .map_err(|error| OrbitError::Execution(format!("lease token check failed: {error}")))?
    }

    fn admission(&self, client: IpAddr) -> FailedAuthAdmission {
        match self.limiter.lock() {
            Ok(mut limiter) => limiter.check(client, Instant::now()),
            Err(_) => FailedAuthAdmission::Admit,
        }
    }

    fn record_failure(&self, client: IpAddr) {
        if let Ok(mut limiter) = self.limiter.lock() {
            limiter.record_failure(client, Instant::now());
        }
    }
}

/// Marks a response that Orbit produced and audited itself.
#[derive(Clone, Copy)]
struct AuditedByOrbit;

/// The audit reason for a refusal the backend produced without reaching
/// Orbit.
#[derive(Clone, Copy)]
struct ListenerRefusalReason(&'static str);

/// Resolve the client address, apply the failed-authentication limit,
/// authenticate the bearer token for the route's verb and workspace, and
/// hand the authenticated caller to the lease router.
async fn lease_listener_gate(
    State(gate): State<Arc<ListenerGate>>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = audit_route(&request);
    let Some(peer) = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
    else {
        tracing::error!("lease listener request without a peer address");
        return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    };
    let resolved = {
        let forwarded: Vec<&[u8]> = request
            .headers()
            .get_all("x-forwarded-for")
            .iter()
            .map(HeaderValue::as_bytes)
            .collect();
        resolve_lease_client_address(&gate.client_address, peer, &forwarded)
    };
    let client = match resolved {
        Ok(client) => client,
        Err(refusal) => {
            tracing::warn!(%peer, %refusal, "lease listener refused an unresolvable client address");
            let origin = LeaseRequestOrigin {
                remote_address: peer.to_canonical().to_string(),
                route,
            };
            gate.audit(PendingRefusal::anonymous(origin, refusal.as_str()))
                .await;
            return listener_refusal(StatusCode::BAD_REQUEST, "bad request");
        }
    };
    let origin = LeaseRequestOrigin {
        remote_address: client.to_string(),
        route,
    };
    if let FailedAuthAdmission::Limited {
        retry_after,
        first_in_window,
    } = gate.admission(client)
    {
        if first_in_window {
            gate.audit(PendingRefusal::anonymous(origin, "rate_limited"))
                .await;
        }
        let mut response = listener_refusal(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed authentication attempts; retry later",
        );
        let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
        if let Ok(value) = HeaderValue::from_str(&seconds.max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    let verb = route_verb(request.method(), request.uri().path());
    let workspace = Query::<LeaseCaller>::try_from_uri(request.uri())
        .ok()
        .and_then(|Query(caller)| caller.workspace)
        .filter(|workspace| !workspace.is_empty());
    let Some(secret) = bearer_secret(request.headers()) else {
        gate.record_failure(client);
        let mut pending = PendingRefusal::anonymous(origin, "missing_token");
        pending.workspace = workspace;
        pending.verb = Some(verb);
        gate.audit(pending).await;
        return unauthorized();
    };
    let record = match gate
        .verify(secret, workspace.clone(), verb, origin.clone())
        .await
    {
        Ok(LeaseTokenDecision::Allow(record)) => record,
        Ok(LeaseTokenDecision::Deny(refusal)) => {
            return match refusal {
                LeaseTokenRefusal::Unknown
                | LeaseTokenRefusal::Revoked
                | LeaseTokenRefusal::Expired => {
                    gate.record_failure(client);
                    unauthorized()
                }
                LeaseTokenRefusal::WrongWorkspace | LeaseTokenRefusal::VerbNotGranted => {
                    gate.record_failure(client);
                    listener_refusal(StatusCode::FORBIDDEN, "forbidden")
                }
                LeaseTokenRefusal::WorkspaceUnnamed => listener_refusal(
                    StatusCode::BAD_REQUEST,
                    "this token names several workspaces; choose one with ?workspace=<id>",
                ),
            };
        }
        Err(error) => {
            tracing::error!(%error, "lease listener could not check a token");
            return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };
    let Some(workspace) = workspace.or_else(|| record.scope.workspaces.first().cloned()) else {
        return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    };
    request.extensions_mut().insert(LeaseCallerToken {
        token_id: record.token_id.clone(),
        scope: record.scope.clone(),
        workspace: workspace.clone(),
        client_address: client.to_string(),
    });
    let response = next.run(request).await;
    if !response.status().is_success() && response.extensions().get::<AuditedByOrbit>().is_none() {
        let reason = response
            .extensions()
            .get::<ListenerRefusalReason>()
            .map_or("request_rejected", |reason| reason.0);
        gate.audit(PendingRefusal {
            origin,
            reason,
            token_id: Some(record.token_id),
            principal: Some(record.scope.principal),
            workspace: Some(workspace),
            verb: Some(verb),
        })
        .await;
    }
    response
}

/// `METHOD /api/…` as received, bounded for the audit row. The gate sits
/// on the router nested at `/api`, which sees the path without that prefix.
fn audit_route(request: &Request) -> String {
    let path = request.uri().path();
    let mut route = if path.starts_with("/api/") {
        format!("{} {path}", request.method())
    } else {
        format!("{} /api{path}", request.method())
    };
    if route.len() > AUDIT_ROUTE_MAX_BYTES {
        let mut end = AUDIT_ROUTE_MAX_BYTES;
        while !route.is_char_boundary(end) {
            end -= 1;
        }
        route.truncate(end);
    }
    route
}

/// The verb a lease route needs: reads (`GET`, and `POST …/check`, which
/// changes nothing) need `read`; everything else needs `lease`.
fn route_verb(method: &Method, path: &str) -> LeaseTokenVerb {
    if *method == Method::GET || (*method == Method::POST && path.ends_with("/leases/check")) {
        LeaseTokenVerb::Read
    } else {
        LeaseTokenVerb::Lease
    }
}

/// The secret of an `Authorization: Bearer <secret>` header, if any.
fn bearer_secret(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, secret) = value.trim().split_once(' ')?;
    let secret = secret.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !secret.is_empty()).then(|| secret.to_string())
}

fn listener_refusal(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn unauthorized() -> Response {
    let mut response = listener_refusal(StatusCode::UNAUTHORIZED, "unauthorized");
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

/// Forwards each lease operation, with the authenticated token, to Orbit Web
/// over the intake socket.
struct IntakeLeaseBackend {
    intake_socket: PathBuf,
}

impl IntakeLeaseBackend {
    fn forward(
        &self,
        caller: &LeaseCaller,
        operation: LeaseIntakeOperation,
        mut input: Value,
    ) -> LeaseBackendResult {
        let Some(token) = &caller.token else {
            tracing::error!("lease listener backend called without an authenticated token");
            return Err(Box::new(backend_refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "internal error"}),
                "unauthenticated",
            )));
        };
        // The principal is the token's; a declared one is ignored.
        if let Value::Object(map) = &mut input {
            map.remove("principal");
        }
        let request = LeaseIntakeRequest {
            operation,
            workspace: token.workspace.clone(),
            authority: LeaseIntakeAuthority {
                token_id: token.token_id.clone(),
                scope: token.scope.clone(),
                client_address: token.client_address.clone(),
            },
            input,
        };
        let answer = LeaseIntakeClient::connect(&self.intake_socket, INTAKE_TIMEOUT)
            .and_then(|mut client| client.call(&request));
        match answer {
            Ok(LeaseIntakeResponse::Ok { result }) => Ok(result),
            Ok(LeaseIntakeResponse::Error(error)) => Err(Box::new(intake_error_response(error))),
            Err(error) => {
                tracing::warn!(%error, "lease listener could not reach the intake socket");
                Err(Box::new(backend_refusal(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": "the lease service is unavailable; retry later"}),
                    "intake_unavailable",
                )))
            }
        }
    }
}

fn backend_refusal(status: StatusCode, body: Value, reason: &'static str) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .extensions_mut()
        .insert(ListenerRefusalReason(reason));
    response
}

/// Map an intake refusal to HTTP. Codes from Orbit's operation dispatch were
/// audited there; anything else stopped at the socket and is audited here.
fn intake_error_response(error: LeaseIntakeError) -> Response {
    let status = match error.code.as_str() {
        "scope_refused" => StatusCode::FORBIDDEN,
        "lease_not_held" => StatusCode::NOT_FOUND,
        // Without a workspace runtime Orbit writes no audit row, so the
        // listener audits this refusal itself.
        "unknown_workspace" => {
            return backend_refusal(
                StatusCode::NOT_FOUND,
                json!({"error": error.message, "code": error.code}),
                "unknown_workspace",
            );
        }
        "invalid_input" | "invalid_authority" => StatusCode::BAD_REQUEST,
        "not_available_in_this_phase" => StatusCode::NOT_IMPLEMENTED,
        "operation_failed" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => {
            tracing::warn!(code = %error.code, message = %error.message, "lease intake refused a request");
            return backend_refusal(
                StatusCode::BAD_GATEWAY,
                json!({"error": "the lease service refused the request", "code": error.code}),
                "intake_refused",
            );
        }
    };
    let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::warn!(message = %error.message, "lease operation failed on the owner");
        "the lease operation failed on the owner".to_string()
    } else {
        error.message
    };
    let mut response =
        (status, Json(json!({"error": message, "code": error.code}))).into_response();
    response.extensions_mut().insert(AuditedByOrbit);
    response
}

impl LeaseBackend for IntakeLeaseBackend {
    fn request(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Request, input)
    }

    fn renew(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Renew, input)
    }

    fn expand(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Expand, input)
    }

    fn release(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Release, input)
    }

    fn show(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Show, input)
    }

    fn list(&self, caller: &LeaseCaller) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::List, json!({}))
    }

    fn check(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Check, input)
    }
}

#[cfg(test)]
mod tests;
