//! Listener-only admission, authentication and coalesced audit persistence.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use chrono::Utc;
use orbit_auth::client_address::{LeaseClientAddressPolicy, resolve_lease_client_address};
use orbit_auth::limiter::{KeyedLimiter, LimitKey};
use orbit_auth::scope::{LeaseTokenRefusal, LeaseTokenVerb};
use orbit_auth::store::{LeaseTokenDecision, LeaseTokenStore};
use orbit_core::OrbitError;
use tokio::sync::Semaphore;

use super::{
    LeaseListenerLimits, bearer_secret, limits, listener_refusal, route_verb, unauthorized,
};
use crate::api::leases::{LeaseCaller, LeaseCallerToken};

pub(super) struct ListenerGate {
    pub(super) client_address: LeaseClientAddressPolicy,
    pub(super) store: Arc<Mutex<LeaseTokenStore>>,
    pub(super) limiter: Arc<Mutex<KeyedLimiter>>,
    pub(super) limits: LeaseListenerLimits,
    pub(super) failed_auth_limit: u32,
    pub(super) failed_auth_window: Duration,
    pub(super) in_flight: Arc<Semaphore>,
    pub(super) flush_lock: Arc<tokio::sync::Mutex<()>>,
    /// Set at shutdown so open notice streams close instead of holding the drain.
    pub(super) closing: tokio::sync::watch::Sender<bool>,
}

impl ListenerGate {
    pub(super) async fn audit(&self, key: &LimitKey, reason: &'static str, period: Duration) {
        if let Ok(mut limiter) = self.limiter.lock() {
            limiter.record_rejection(
                key,
                reason,
                period,
                tokio::time::Instant::now().into_std(),
                Utc::now(),
            );
        }
        self.flush_audits(false).await;
    }

    pub(super) async fn flush_audits(&self, force: bool) {
        // Refused requests never queue a task or a waiter per audit row.
        let Ok(flush) = Arc::clone(&self.flush_lock).try_lock_owned() else {
            return;
        };
        let rows = match self.limiter.lock() {
            Ok(mut limiter) => limiter.drain_audits(tokio::time::Instant::now().into_std(), force),
            Err(_) => return,
        };
        if rows.is_empty() {
            return;
        }
        let store = Arc::clone(&self.store);
        let limiter = Arc::clone(&self.limiter);
        let written = tokio::task::spawn_blocking(move || {
            // Keep the flush allowance in the blocking operation: cancelling
            // its HTTP caller must not admit another queued audit write.
            let _flush = flush;
            let written = store
                .lock()
                .map_err(|_| OrbitError::Execution("lease token store lock poisoned".into()))
                .and_then(|mut store| store.record_limit_audits(&rows));
            if let Err(error) = &written {
                if let Ok(mut limiter) = limiter.lock() {
                    limiter.retry_audits(rows);
                }
                tracing::error!(%error, "lease listener could not flush rejection audits");
            }
            written
        })
        .await;
        if let Err(error) = written {
            tracing::error!(%error, "lease listener audit task failed");
        }
    }

    async fn verify(
        &self,
        secret: String,
        workspace: Option<String>,
        verb: LeaseTokenVerb,
    ) -> Result<LeaseTokenDecision, OrbitError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut store = store
                .lock()
                .map_err(|_| OrbitError::Execution("lease token store lock poisoned".into()))?;
            store.verify_listener_coalesced(&secret, workspace.as_deref(), verb, Utc::now())
        })
        .await
        .map_err(|error| OrbitError::Execution(format!("lease token check failed: {error}")))?
    }

    fn check(
        &self,
        key: &LimitKey,
        name: &'static str,
        quota: u32,
        period: Duration,
        consume: bool,
    ) -> Result<(), Duration> {
        self.limiter.lock().map_err(|_| period)?.check(
            key,
            name,
            quota,
            period,
            tokio::time::Instant::now().into_std(),
            consume,
        )
    }

    async fn rate_refusal(
        &self,
        key: &LimitKey,
        name: &'static str,
        period: Duration,
        retry: Duration,
    ) -> Response {
        self.audit(key, name, period).await;
        let mut response = listener_refusal(StatusCode::TOO_MANY_REQUESTS, name);
        let seconds = retry
            .as_secs()
            .saturating_add(u64::from(retry.subsec_nanos() > 0))
            .max(1);
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

#[derive(Clone)]
struct LimitedClient {
    address: IpAddr,
    key: LimitKey,
}

pub(super) const REQUEST_WINDOW: Duration = Duration::from_secs(60);

/// Resolve once, then bound every request before authentication, including
/// health, unknown routes, malformed forwarding chains and method errors.
pub(super) async fn listener_address_gate(
    State(gate): State<Arc<ListenerGate>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(peer) = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
    else {
        return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    };
    let forwarded: Vec<&[u8]> = request
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .map(HeaderValue::as_bytes)
        .collect();
    let resolved = resolve_lease_client_address(&gate.client_address, peer, &forwarded);
    let address = resolved.as_ref().copied().unwrap_or(peer).to_canonical();
    let key = match gate.limiter.lock() {
        Ok(mut limiter) => {
            let key = limiter.address_key(address);
            limiter.bounded_key(key)
        }
        Err(_) => return listener_refusal(StatusCode::SERVICE_UNAVAILABLE, "limiter unavailable"),
    };
    if let Err(retry) = gate.check(
        &key,
        "address_requests",
        gate.limits.address_per_minute,
        REQUEST_WINDOW,
        true,
    ) {
        return gate
            .rate_refusal(&key, "address_requests", REQUEST_WINDOW, retry)
            .await;
    }
    if let Err(retry) = gate.check(
        &key,
        "failed_auth",
        gate.failed_auth_limit,
        gate.failed_auth_window,
        false,
    ) {
        return gate
            .rate_refusal(&key, "failed_auth", gate.failed_auth_window, retry)
            .await;
    }
    if let Err(refusal) = resolved {
        gate.audit(&key, refusal.as_str(), REQUEST_WINDOW).await;
        return listener_refusal(StatusCode::BAD_REQUEST, "bad request");
    }
    let Ok(permit) = Arc::clone(&gate.in_flight).try_acquire_owned() else {
        gate.audit(&key, "in_flight_requests", REQUEST_WINDOW).await;
        return listener_refusal(StatusCode::SERVICE_UNAVAILABLE, "in_flight_requests");
    };
    request.extensions_mut().insert(LimitedClient {
        address,
        key: key.clone(),
    });
    let response = next.run(request).await;
    let reason = match response.status() {
        StatusCode::PAYLOAD_TOO_LARGE => Some("body_bytes"),
        StatusCode::REQUEST_TIMEOUT => Some("request_timeout"),
        _ => None,
    };
    if let Some(reason) = reason {
        gate.audit(&key, reason, REQUEST_WINDOW).await;
    }
    limits::retain_response_allowance(response, permit)
}

/// Marks a response that Orbit produced and audited itself.
#[derive(Clone, Copy)]
pub(super) struct AuditedByOrbit;

/// The audit reason for a refusal the backend produced without reaching
/// Orbit.
#[derive(Clone, Copy)]
pub(super) struct ListenerRefusalReason(pub(super) &'static str);

/// Authenticate and apply address-independent read/write and concurrency budgets.
pub(super) async fn lease_listener_gate(
    State(gate): State<Arc<ListenerGate>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(client) = request.extensions().get::<LimitedClient>().cloned() else {
        return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    };
    let verb = route_verb(request.method(), request.uri().path());
    let workspace = Query::<LeaseCaller>::try_from_uri(request.uri())
        .ok()
        .and_then(|Query(caller)| caller.workspace)
        .filter(|ws| !ws.is_empty());
    let decision = match bearer_secret(request.headers()) {
        Some(secret) => match gate.verify(secret, workspace.clone(), verb).await {
            Ok(decision) => decision,
            Err(error) => {
                tracing::error!(%error, "lease listener could not check a token");
                return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        },
        None => {
            let _ = gate.check(
                &client.key,
                "failed_auth",
                gate.failed_auth_limit,
                gate.failed_auth_window,
                true,
            );
            gate.audit(&client.key, "missing_token", gate.failed_auth_window)
                .await;
            return unauthorized();
        }
    };
    let record = match decision {
        LeaseTokenDecision::Allow(record) => record,
        LeaseTokenDecision::Deny(refusal) => {
            if refusal != LeaseTokenRefusal::WorkspaceUnnamed {
                let _ = gate.check(
                    &client.key,
                    "failed_auth",
                    gate.failed_auth_limit,
                    gate.failed_auth_window,
                    true,
                );
            }
            gate.audit(&client.key, refusal.as_str(), gate.failed_auth_window)
                .await;
            return match refusal {
                LeaseTokenRefusal::Unknown
                | LeaseTokenRefusal::Revoked
                | LeaseTokenRefusal::Expired => unauthorized(),
                LeaseTokenRefusal::WrongWorkspace | LeaseTokenRefusal::VerbNotGranted => {
                    listener_refusal(StatusCode::FORBIDDEN, "forbidden")
                }
                LeaseTokenRefusal::WorkspaceUnnamed => listener_refusal(
                    StatusCode::BAD_REQUEST,
                    "this token names several workspaces; choose one with ?workspace=<id>",
                ),
            };
        }
    };
    let key = LimitKey::Token(record.token_id.clone());
    let (name, quota) = match verb {
        LeaseTokenVerb::Read => ("token_reads", gate.limits.token_reads_per_minute),
        LeaseTokenVerb::Lease => ("token_writes", gate.limits.token_writes_per_minute),
    };
    if let Err(retry) = gate.check(&key, name, quota, REQUEST_WINDOW, true) {
        return gate.rate_refusal(&key, name, REQUEST_WINDOW, retry).await;
    }
    let allowance = gate.limiter.lock().ok().map(|mut limiter| {
        limiter.token_allowance(
            &key,
            gate.limits.max_in_flight_per_token as usize,
            gate.limits.max_streams_per_token as usize,
            request.uri().path().ends_with("/leases/events"),
        )
    });
    let allowance = match allowance {
        Some(Ok(allowance)) => allowance,
        Some(Err(reason)) => {
            gate.audit(&key, reason, REQUEST_WINDOW).await;
            return listener_refusal(StatusCode::SERVICE_UNAVAILABLE, reason);
        }
        None => return listener_refusal(StatusCode::SERVICE_UNAVAILABLE, "limiter unavailable"),
    };
    let Some(workspace) = workspace.or_else(|| record.scope.workspaces.first().cloned()) else {
        return listener_refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    };
    request.extensions_mut().insert(LeaseCallerToken {
        token_id: record.token_id.clone(),
        scope: record.scope.clone(),
        workspace,
        client_address: client.address.to_string(),
    });
    let response = next.run(request).await;
    if !response.status().is_success() && response.extensions().get::<AuditedByOrbit>().is_none() {
        let reason = response
            .extensions()
            .get::<ListenerRefusalReason>()
            .map_or("request_rejected", |reason| reason.0);
        gate.audit(&key, reason, REQUEST_WINDOW).await;
    }
    limits::retain_response_allowance(response, allowance)
}
