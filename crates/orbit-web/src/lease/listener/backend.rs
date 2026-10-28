//! Forwarding authenticated lease operations to the intake socket.

use super::admission::{AuditedByOrbit, ListenerRefusalReason};
use crate::api::leases::{LeaseBackend, LeaseBackendResult, LeaseCaller, LeaseNoticePages};
use crate::lease_intake::{
    LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitError;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

/// How long the listener waits for each intake frame.
pub(super) const INTAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an open notice stream waits for its next page. Orbit sends one
/// about every second, so silence this long means it has gone.
const INTAKE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Forwards each lease operation, with the authenticated token, to Orbit Web
/// over the intake socket.
pub(super) struct IntakeLeaseBackend {
    pub(super) intake_socket: PathBuf,
    pub(super) intake_in_flight: Arc<Semaphore>,
    pub(super) timeout: Duration,
    /// Set when the listener shuts down, so open notice streams end.
    pub(super) closing: watch::Receiver<bool>,
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
