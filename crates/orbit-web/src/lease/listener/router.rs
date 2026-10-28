//! Lease listener routes and authentication response helpers.

use super::LeaseListenerConfig;
use super::admission::{ListenerGate, lease_listener_gate, listener_address_gate};
use super::backend::{INTAKE_TIMEOUT, IntakeLeaseBackend};
use crate::api::leases::lease_router;
use axum::Router;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use orbit_auth::limiter::KeyedLimiter;
use orbit_auth::scope::LeaseTokenVerb;
use orbit_auth::store::LeaseTokenStore;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Semaphore, watch};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

/// The listener's whole router, with address admission on every route and
/// token admission on lease routes. Supply [`ConnectInfo`](axum::extract::ConnectInfo).
/// Use [`super::serve_lease_listener_on`] for connection bounds and periodic audit flushes.
pub fn lease_listener_router(config: &LeaseListenerConfig, store: LeaseTokenStore) -> Router {
    build_listener_router(config, store).0
}

pub(super) fn build_listener_router(
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
pub(super) fn route_verb(method: &Method, path: &str) -> LeaseTokenVerb {
    if *method == Method::GET || (*method == Method::POST && path.ends_with("/leases/check")) {
        LeaseTokenVerb::Read
    } else {
        LeaseTokenVerb::Lease
    }
}

/// The secret of an `Authorization: Bearer <secret>` header, if any.
pub(super) fn bearer_secret(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, secret) = value.trim().split_once(' ')?;
    let secret = secret.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !secret.is_empty()).then(|| secret.to_string())
}

pub(super) fn listener_refusal(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

pub(super) fn unauthorized() -> Response {
    let mut response = listener_refusal(StatusCode::UNAUTHORIZED, "unauthorized");
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}
