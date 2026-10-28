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
//! store as bounded aggregates by canonical key, limit/reason and window,
//! without secrets, principals, routes or queries. Orbit audits every
//! operation it receives.

use std::net::SocketAddr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use orbit_auth::client_address::{LeaseClientAddressPolicy, TrustedProxies};
use orbit_auth::environment::{LISTENER_USER, check_listener_execution_environment};
use orbit_auth::limiter::KeyedLimiter;
use orbit_auth::scope::LeaseTokenVerb;
use orbit_auth::store::LeaseTokenStore;
use orbit_config::LeaseListenerSettings;
use orbit_core::OrbitError;
use serde_json::{Value, json};

use crate::api::leases::{
    LeaseBackend, LeaseBackendResult, LeaseCaller, LeaseNoticePages, lease_router,
};
use crate::lease_intake::{
    LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse,
};

mod admission;
mod limits;
use admission::{
    AuditedByOrbit, ListenerGate, ListenerRefusalReason, lease_listener_gate, listener_address_gate,
};
mod tls;
mod transport;

pub use limits::LeaseListenerLimits;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

/// How long the listener waits for each intake frame.
const INTAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an open notice stream waits for its next page. Orbit sends one
/// about every second, so silence this long means it has gone.
const INTAKE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The listener's validated configuration.
#[derive(Debug, Clone)]
pub struct LeaseListenerConfig {
    /// Address to listen on.
    pub bind: SocketAddr,
    /// Native TLS, validated and loaded before binding; `None` serves HTTP.
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// How the client address is found.
    pub client_address: LeaseClientAddressPolicy,
    /// The operator's lease intake socket.
    pub intake_socket: PathBuf,
    /// Failed authentications from one address before it gets 429.
    pub failed_auth_limit: u32,
    /// The window those failures are counted over.
    pub failed_auth_window: Duration,
    /// Request, transport and concurrency bounds.
    pub limits: LeaseListenerLimits,
}

impl LeaseListenerConfig {
    /// Validate `lease.listener.*`, refusing an unsafe or incomplete
    /// combination with a message naming the setting.
    pub fn from_settings(settings: &LeaseListenerSettings) -> Result<Self, OrbitError> {
        let tls = settings
            .tls
            .as_ref()
            .map(tls::load_listener_tls)
            .transpose()?;
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
        if bind.ip().is_unspecified() && tls.is_none() && !settings.behind_proxy {
            return Err(refused(&format!(
                "lease.listener.bind = \"{raw_bind}\" listens on every interface without TLS. \
                 Set lease.listener.tls, bind a specific address, or put a terminating proxy in front and set \
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
            tls,
            client_address,
            intake_socket,
            failed_auth_limit: settings.failed_auth_limit,
            failed_auth_window: Duration::from_secs(u64::from(settings.failed_auth_window_seconds)),
            limits: LeaseListenerLimits::from_settings(settings),
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
        let shutdown = crate::serve::shutdown_signal()?;
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
            let scheme = if config.tls.is_some() { "https" } else { "http" };
            println!("Lease listener on {scheme}://{local} (lease routes and /healthz only)");
        }
        serve_lease_listener_on(listener, config, store, shutdown).await
    })
}

/// Serve an already bound socket with the same transport, router and peer
/// addresses as [`serve_lease_listener`], until `shutdown` completes.
pub async fn serve_lease_listener_on(
    listener: tokio::net::TcpListener,
    config: LeaseListenerConfig,
    store: LeaseTokenStore,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), OrbitError> {
    let (app, gate) = build_listener_router(&config, store);
    transport::serve_bounded_listener(listener, config, app, gate, shutdown)
        .await
        .map_err(|error| OrbitError::Io(format!("lease listener: {error}")))
}

/// The listener's whole router, with address admission on every route and
/// token admission on lease routes. Supply [`ConnectInfo`](axum::extract::ConnectInfo).
/// Use [`serve_lease_listener_on`] for connection bounds and periodic audit flushes.
pub fn lease_listener_router(config: &LeaseListenerConfig, store: LeaseTokenStore) -> Router {
    build_listener_router(config, store).0
}

fn build_listener_router(
    config: &LeaseListenerConfig,
    store: LeaseTokenStore,
) -> (Router, Arc<ListenerGate>) {
    let in_flight = Arc::new(Semaphore::new(
        config.limits.max_in_flight_requests as usize,
    ));
    let intake_in_flight = Arc::new(Semaphore::new(
        config.limits.max_in_flight_requests as usize,
    ));
    let (closing, closing_streams) = watch::channel(false);
    let gate = Arc::new(ListenerGate {
        client_address: config.client_address.clone(),
        store: Arc::new(Mutex::new(store)),
        limiter: Arc::new(Mutex::new(KeyedLimiter::new(
            config.limits.max_tracked_keys as usize,
            config.limits.ipv6_prefix_len as u8,
            tokio::time::Instant::now().into_std(),
        ))),
        limits: config.limits.clone(),
        failed_auth_limit: config.failed_auth_limit,
        failed_auth_window: config.failed_auth_window,
        in_flight,
        flush_lock: Arc::new(tokio::sync::Mutex::new(())),
        closing,
    });
    let backend = IntakeLeaseBackend {
        intake_socket: config.intake_socket.clone(),
        intake_in_flight,
        timeout: Duration::from_secs(u64::from(config.limits.request_timeout_seconds))
            .min(INTAKE_TIMEOUT),
        closing: closing_streams,
    };
    let app = Router::new()
        .route("/healthz", get(listener_healthz))
        .nest(
            "/api",
            lease_router(backend).route_layer(middleware::from_fn_with_state(
                Arc::clone(&gate),
                lease_listener_gate,
            )),
        )
        .layer(RequestBodyLimitLayer::new(
            config.limits.max_body_bytes as usize,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(u64::from(config.limits.request_timeout_seconds)),
        ))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&gate),
            listener_address_gate,
        ));
    (app, gate)
}

/// Liveness only: no version, workspace or token detail.
async fn listener_healthz() -> Json<Value> {
    Json(json!({"status": "ok"}))
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
    intake_in_flight: Arc<Semaphore>,
    timeout: Duration,
    /// Set when the listener shuts down, so open notice streams end.
    closing: watch::Receiver<bool>,
}

impl IntakeLeaseBackend {
    fn forward(
        &self,
        caller: &LeaseCaller,
        operation: LeaseIntakeOperation,
        input: Value,
    ) -> LeaseBackendResult {
        let (request, _permit) = self.intake_request(caller, operation, input)?;
        let answer = LeaseIntakeClient::connect(&self.intake_socket, self.timeout)
            .and_then(|mut client| client.call(&request));
        match answer {
            Ok(LeaseIntakeResponse::Ok { result }) => Ok(result),
            Ok(LeaseIntakeResponse::Error(error)) => Err(Box::new(intake_error_response(error))),
            Err(error) => Err(Box::new(intake_unavailable(&error))),
        }
    }

    /// Open an `events` stream: its first page is read here, and the rest by
    /// a blocking reader that holds this operation's intake allowance until
    /// the client goes, Orbit closes the stream or the listener shuts down.
    fn open_notice_stream(
        &self,
        caller: &LeaseCaller,
        input: Value,
    ) -> Result<LeaseNoticePages, Box<Response>> {
        let (request, permit) = self.intake_request(caller, LeaseIntakeOperation::Events, input)?;
        let mut client = LeaseIntakeClient::connect(&self.intake_socket, self.timeout)
            .map_err(|error| Box::new(intake_unavailable(&error)))?;
        let first = match client.call(&request) {
            Ok(LeaseIntakeResponse::Ok { result }) => result,
            Ok(LeaseIntakeResponse::Error(error)) => {
                return Err(Box::new(intake_error_response(error)));
            }
            Err(error) => return Err(Box::new(intake_unavailable(&error))),
        };
        client
            .set_read_timeout(INTAKE_STREAM_IDLE_TIMEOUT)
            .map_err(|error| Box::new(intake_unavailable(&error)))?;
        let (pages, receiver) = mpsc::channel(4);
        let _ = pages.try_send(Ok(first));
        let closing = self.closing.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            while !*closing.borrow() && !pages.is_closed() {
                let page = match client.next_response() {
                    Ok(LeaseIntakeResponse::Ok { result }) => Ok(result),
                    Ok(LeaseIntakeResponse::Error(error)) => {
                        Err(Box::new(intake_error_response(error)))
                    }
                    Err(error) => {
                        tracing::debug!(%error, "lease notice stream from the intake socket ended");
                        return;
                    }
                };
                let last = page.is_err();
                if pages.blocking_send(page).is_err() || last {
                    return;
                }
            }
        });
        Ok(receiver)
    }

    /// The intake request for `operation`, carrying the authenticated token,
    /// and the allowance its blocking socket I/O holds.
    fn intake_request(
        &self,
        caller: &LeaseCaller,
        operation: LeaseIntakeOperation,
        mut input: Value,
    ) -> Result<(LeaseIntakeRequest, OwnedSemaphorePermit), Box<Response>> {
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
        // A handler timeout cannot cancel blocking socket I/O. Keep a separate
        // allowance in this blocking operation until its socket really closes.
        let Ok(permit) = Arc::clone(&self.intake_in_flight).try_acquire_owned() else {
            return Err(Box::new(backend_refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "in_flight_requests"}),
                "in_flight_requests",
            )));
        };
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
        Ok((request, permit))
    }
}

/// An unreachable intake socket, or a refused handshake, such as from an
/// Orbit Web of another protocol version; the reason is logged, not sent.
fn intake_unavailable(error: &OrbitError) -> Response {
    tracing::warn!(%error, "lease listener could not reach the intake socket");
    backend_refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": "the lease service is unavailable; retry later"}),
        "intake_unavailable",
    )
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
fn intake_error_response(mut error: LeaseIntakeError) -> Response {
    error.message = orbit_common::security::redaction::redact_all(&error.message);
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

    fn notices(&self, caller: &LeaseCaller, input: Value) -> LeaseBackendResult {
        self.forward(caller, LeaseIntakeOperation::Notices, input)
    }

    fn notice_stream(
        &self,
        caller: &LeaseCaller,
        input: Value,
    ) -> Result<LeaseNoticePages, Box<Response>> {
        self.open_notice_stream(caller, input)
    }
}
