//! `/api/on/<host>/<rest>`: another registered host's dashboard API through
//! this one [ORB-14679].
//!
//! A request is forwarded, method, `/api/<rest>`, query string and body
//! unchanged, to the host's own `orbit web serve` over the tunnel
//! [`crate::host_tunnels`] owns. That host stays authoritative and enforces
//! its own gates. Before any process starts, this side runs the router's
//! origin guard, refuses an unsafe method without the operator session
//! (`host.forward`), refuses multi-hop and host-file paths, and resolves the
//! host from the serving host's own host file. The serving host's own name is
//! answered by the local router, with no tunnel.

use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{HeaderName, Method, header};
use axum::response::{IntoResponse, Json, Response};
use futures_core::Stream;
use hyper_util::rt::TokioIo;
use orbit_common::governance::authorization::DASHBOARD_HOST_FORWARD;
use orbit_registry::hosts::ResolvedHost;
use serde::Serialize;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::host::{host_error_code, status_for};
use super::routines::{action_capability, authorization_denied, authorized_caller};
use crate::host_tunnels::{ForwardError, HostTarget, Lease, RemoteIdentity};
use crate::ssh_tunnel::TunnelOrigin;
use crate::state::DashboardState;

/// Bound on one forwarded request: the response head, and for anything but
/// an event stream, the whole body. A slow host holds only its own request.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// Request headers passed to the remote. Host and Origin are replaced with the
/// remote's own loopback authority; cookies and the rest stay here.
const FORWARDED_REQUEST_HEADERS: [HeaderName; 3] = [
    header::ACCEPT,
    header::CONTENT_TYPE,
    HeaderName::from_static("last-event-id"),
];

/// Response headers passed back. The remote's artifact sandbox CSP and
/// attachment disposition survive; hop-by-hop headers do not, and this
/// dashboard's own security and nosniff layers still apply.
const FORWARDED_RESPONSE_HEADERS: [HeaderName; 4] = [
    header::CONTENT_TYPE,
    header::CONTENT_DISPOSITION,
    header::CONTENT_SECURITY_POLICY,
    header::CACHE_CONTROL,
];

/// The host a request names: one of the serving host's registered remotes,
/// or the serving host itself.
enum Addressed {
    Local { name: String, machine_id: String },
    Remote(HostTarget),
}

/// `GET|POST|PUT|PATCH|DELETE /api/on/:host/*rest`.
pub(super) async fn forward(
    State(state): State<DashboardState>,
    UrlPath((host, rest)): UrlPath<(String, String)>,
    request: Request,
) -> Response {
    let unsafe_method = !matches!(*request.method(), Method::GET | Method::HEAD);
    let operator = authorized_caller(&DASHBOARD_HOST_FORWARD, state.operator_session());
    if unsafe_method && let Err(denial) = operator {
        return authorization_denied(denial);
    }
    if let Err(message) = validate_rest(&rest) {
        return failure(&host, "invalid_input", message);
    }
    let Some(raw_rest) = raw_rest(request.uri().path()) else {
        return failure(&host, "invalid_input", "missing forwarded path".to_string());
    };
    let query = request
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let target = match resolve(&state, &host).await {
        Ok(Addressed::Local { name, .. }) => {
            return answer_locally(state, request, &name, &raw_rest, &query).await;
        }
        Ok(Addressed::Remote(target)) => target,
        Err(error) => return failure(&host, &error.code, error.message),
    };
    let tunnels = std::sync::Arc::clone(state.host_tunnels());
    let operator = operator.is_ok();
    let leased = {
        let target = target.clone();
        tokio::task::spawn_blocking(move || tunnels.acquire(&target, operator)).await
    };
    let lease = match leased {
        Ok(Ok(lease)) => lease,
        Ok(Err(error)) => return failure(&target.name, &error.code, error.message),
        Err(join) => {
            return failure(
                &target.name,
                "internal_error",
                format!("host tunnel establish panicked: {join}"),
            );
        }
    };
    let remote_port = state.host_tunnels().remote_port();
    let upstream = match upstream_request(request, &raw_rest, &query, remote_port) {
        Ok(upstream) => upstream,
        Err(message) => return failure(&target.name, "invalid_input", message),
    };
    let started = tokio::time::Instant::now();
    let response =
        match tokio::time::timeout(UPSTREAM_TIMEOUT, send(lease.local_port, upstream)).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return failure(
                    &target.name,
                    "unreachable_destination",
                    format!("the forward to host '{}' failed: {error}", target.name),
                );
            }
            Err(_) => {
                return failure(
                    &target.name,
                    "process_timeout",
                    format!(
                        "host '{}' did not answer within {}s",
                        target.name,
                        UPSTREAM_TIMEOUT.as_secs()
                    ),
                );
            }
        };
    let deadline = started + UPSTREAM_TIMEOUT;
    let closing = state.host_tunnels().closing();
    forwarded_response(response, lease, deadline, closing)
}

/// `GET /api/hosts/:host/connection`: the selected host's tunnel, opened on
/// demand, as the identity read through it reported. Never a second probe.
pub(super) async fn connection(
    State(state): State<DashboardState>,
    UrlPath(host): UrlPath<String>,
) -> Response {
    let operator = authorized_caller(&DASHBOARD_HOST_FORWARD, state.operator_session()).is_ok();
    let forward_writes = action_capability(&DASHBOARD_HOST_FORWARD, state.operator_session());
    let addressed = match resolve(&state, &host).await {
        Ok(addressed) => addressed,
        Err(error) => return failure(&host, &error.code, error.message),
    };
    let global_root = state.global_root().to_path_buf();
    let tunnels = std::sync::Arc::clone(state.host_tunnels());
    let report = tokio::task::spawn_blocking(move || {
        let local = orbit_cmd::hosts::local_host_facts(&global_root, "");
        let local_identity = RemoteIdentity {
            machine_id: String::new(),
            binary_version: local.binary_version,
            protocol_fingerprint: local.protocol_fingerprint,
        };
        match addressed {
            Addressed::Local { name, machine_id } => ConnectionState::reached(
                name,
                RemoteIdentity {
                    machine_id,
                    ..local_identity.clone()
                },
                true,
                "local",
                &local_identity,
            ),
            Addressed::Remote(target) => match tunnels.acquire(&target, operator) {
                Ok(lease) => ConnectionState::reached(
                    target.name,
                    lease.identity.clone(),
                    false,
                    origin_label(lease.origin),
                    &local_identity,
                ),
                Err(error) => ConnectionState::failed(target, error),
            },
        }
    })
    .await;
    match report {
        Ok(mut report) => {
            report.forward_writes = forward_writes;
            Json(report).into_response()
        }
        Err(join) => failure(
            &host,
            "internal_error",
            format!("host connection report panicked: {join}"),
        ),
    }
}

/// What the dashboard knows about one host's connection.
#[derive(Debug, Serialize)]
struct ConnectionState {
    host: String,
    machine_id: String,
    local: bool,
    reachable: bool,
    /// `local`, `attached` (a dashboard already running there) or `spawned`
    /// (started by this dashboard, stopped with its tunnel).
    origin: Option<&'static str>,
    binary_version: Option<String>,
    protocol_fingerprint: Option<String>,
    /// Version or protocol differs from the serving dashboard. Reported,
    /// never refused.
    skew: bool,
    skew_fields: Vec<&'static str>,
    error: Option<ForwardError>,
    /// Whether this session may forward unsafe methods (`host.forward`), as
    /// `{authorized, reason}`, so a dashboard disables writes it would refuse.
    forward_writes: Value,
}

impl ConnectionState {
    fn reached(
        host: String,
        identity: RemoteIdentity,
        local: bool,
        origin: &'static str,
        serving: &RemoteIdentity,
    ) -> Self {
        let mut skew_fields = Vec::new();
        if identity.binary_version.is_some() && identity.binary_version != serving.binary_version {
            skew_fields.push("binary_version");
        }
        if identity.protocol_fingerprint.is_some()
            && identity.protocol_fingerprint != serving.protocol_fingerprint
        {
            skew_fields.push("protocol_fingerprint");
        }
        Self {
            host,
            machine_id: identity.machine_id,
            local,
            reachable: true,
            origin: Some(origin),
            binary_version: identity.binary_version,
            protocol_fingerprint: identity.protocol_fingerprint,
            skew: !skew_fields.is_empty(),
            skew_fields,
            error: None,
            forward_writes: Value::Null,
        }
    }

    fn failed(target: HostTarget, error: ForwardError) -> Self {
        Self {
            host: target.name,
            machine_id: target.machine_id,
            local: false,
            reachable: false,
            origin: None,
            binary_version: None,
            protocol_fingerprint: None,
            skew: false,
            skew_fields: Vec::new(),
            error: Some(error),
            forward_writes: Value::Null,
        }
    }
}

fn origin_label(origin: TunnelOrigin) -> &'static str {
    match origin {
        TunnelOrigin::Attached => "attached",
        TunnelOrigin::Spawned => "spawned",
    }
}

/// Refuse what is never forwarded: an empty path, a dot segment, another
/// `on/<host>` hop, and the host-file routes, which stay on the serving host.
/// `rest` is the percent-decoded capture, so an encoded spelling is refused
/// too.
fn validate_rest(rest: &str) -> Result<(), String> {
    let mut segments = rest.split('/');
    let first = segments.next().unwrap_or_default();
    if first.is_empty() {
        return Err("name the API path to forward: /api/on/<host>/<path>".to_string());
    }
    if first.eq_ignore_ascii_case("on") {
        return Err("forwards do not chain: /api/on/<host>/on/… is refused".to_string());
    }
    if first.eq_ignore_ascii_case("hosts") {
        return Err(
            "the host file is managed on the serving host: use /api/hosts, not /api/on/<host>/hosts"
                .to_string(),
        );
    }
    if std::iter::once(first)
        .chain(segments)
        .any(|segment| segment == "." || segment == "..")
    {
        return Err("forwarded paths may not contain '.' or '..' segments".to_string());
    }
    Ok(())
}

/// The still-encoded path after `/on/<host>/`, as the client sent it.
fn raw_rest(path: &str) -> Option<String> {
    let path = path.strip_prefix("/api").unwrap_or(path);
    let after_on = path.strip_prefix("/on/")?;
    let (_, rest) = after_on.split_once('/')?;
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Resolve `:host` like CLI `<host>` against the last valid host file.
async fn resolve(state: &DashboardState, host: &str) -> Result<Addressed, ForwardError> {
    let state = state.clone();
    let host = host.to_string();
    let resolved = tokio::task::spawn_blocking(move || {
        let pinned = state.hosts();
        let Some(snapshot) = pinned.snapshot else {
            let (code, message) = pinned.load_error.map_or_else(
                || {
                    (
                        "internal_error".to_string(),
                        "host file not loaded".to_string(),
                    )
                },
                |error| (error.code, error.message),
            );
            return Err((code, message));
        };
        match snapshot.registry.resolve(&host) {
            Ok(ResolvedHost::Local(identity)) => Ok(Addressed::Local {
                name: identity.name.clone(),
                machine_id: identity.id.clone(),
            }),
            Ok(ResolvedHost::Entry(entry)) => Ok(Addressed::Remote(HostTarget {
                name: entry.name.clone(),
                machine_id: entry.machine_id.clone(),
                ssh: entry.ssh.clone(),
            })),
            Ok(ResolvedHost::Legacy(row)) => Ok(Addressed::Remote(HostTarget {
                name: row.ssh.clone(),
                machine_id: row.machine_id.clone(),
                ssh: row.ssh.clone(),
            })),
            Err(error) => Err((host_error_code(&error).to_string(), error.to_string())),
        }
    })
    .await;
    match resolved {
        Ok(Ok(addressed)) => Ok(addressed),
        Ok(Err((code, message))) => Err(ForwardError { code, message }),
        Err(join) => Err(ForwardError {
            code: "internal_error".to_string(),
            message: format!("host resolution panicked: {join}"),
        }),
    }
}

/// The serving host itself: `/api/<rest>` through this dashboard's own
/// router, as if the client had asked for it directly.
async fn answer_locally(
    state: DashboardState,
    mut request: Request,
    host: &str,
    raw_rest: &str,
    query: &str,
) -> Response {
    match format!("/{raw_rest}{query}").parse() {
        Ok(uri) => *request.uri_mut() = uri,
        Err(error) => {
            return failure(host, "invalid_input", format!("invalid path: {error}"));
        }
    }
    match super::router().with_state(state).oneshot(request).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    }
}

/// The request as the remote receives it: its own loopback Host and Origin,
/// so its origin guard sees a same-origin call, and only the allowlisted
/// request headers.
fn upstream_request(
    request: Request,
    raw_rest: &str,
    query: &str,
    remote_port: u16,
) -> Result<Request, String> {
    let (parts, body) = request.into_parts();
    let authority = format!("localhost:{remote_port}");
    let mut builder = Request::builder()
        .method(parts.method)
        .uri(format!("/api/{raw_rest}{query}"))
        .header(header::HOST, &authority)
        .header(header::ORIGIN, format!("http://{authority}"));
    for name in FORWARDED_REQUEST_HEADERS {
        for value in parts.headers.get_all(&name) {
            builder = builder.header(&name, value);
        }
    }
    builder
        .body(body)
        .map_err(|error| format!("invalid forwarded request: {error}"))
}

/// One HTTP/1.1 exchange over a fresh loopback connection to the forward.
async fn send(
    local_port: u16,
    request: Request,
) -> Result<axum::http::Response<hyper::body::Incoming>, String> {
    let stream = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, local_port))
        .await
        .map_err(|error| format!("connect to the local forward: {error}"))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|error| format!("HTTP handshake: {error}"))?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(%error, "forwarded host connection closed with an error");
        }
    });
    sender
        .send_request(request)
        .await
        .map_err(|error| error.to_string())
}

/// The remote's status, allowlisted headers and streamed body. The body holds
/// the lease, so the tunnel counts as in use until the client has it all or
/// goes away. An event stream has no deadline but ends when shutdown begins;
/// any other body must finish by `deadline`.
fn forwarded_response(
    response: axum::http::Response<hyper::body::Incoming>,
    lease: Lease,
    deadline: tokio::time::Instant,
    mut closing: tokio::sync::watch::Receiver<bool>,
) -> Response {
    let (parts, incoming) = response.into_parts();
    let event_stream = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let body = ForwardedBody {
        inner: Body::new(incoming).into_data_stream(),
        deadline: (!event_stream).then(|| Box::pin(tokio::time::sleep_until(deadline))),
        closing: event_stream.then(|| {
            let closed: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
                let _ = closing.wait_for(|closing| *closing).await;
            });
            closed
        }),
        _lease: lease,
    };
    let mut forwarded = Response::new(Body::from_stream(body));
    *forwarded.status_mut() = parts.status;
    for name in FORWARDED_RESPONSE_HEADERS {
        for value in parts.headers.get_all(&name) {
            forwarded.headers_mut().append(&name, value.clone());
        }
    }
    forwarded
}

struct ForwardedBody {
    inner: axum::body::BodyDataStream,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    closing: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    _lease: Lease,
}

impl Stream for ForwardedBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(closing) = this.closing.as_mut()
            && closing.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(None);
        }
        if let Some(deadline) = this.deadline.as_mut()
            && deadline.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the forwarded response did not finish in time",
            ))));
        }
        Pin::new(&mut this.inner)
            .poll_next(cx)
            .map(|item| item.map(|chunk| chunk.map_err(std::io::Error::other)))
    }
}

/// `{error, code, host}` with the status the host registry gives `code`.
fn failure(host: &str, code: &str, message: String) -> Response {
    (
        status_for(code),
        Json(json!({ "error": message, "code": code, "host": host })),
    )
        .into_response()
}
