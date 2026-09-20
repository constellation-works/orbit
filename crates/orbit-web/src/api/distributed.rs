//! Distributed-drain claim provenance and the owner's handoff actions
//! [ORB-12516].
//!
//! # What this adapter is
//!
//! An HTTP projection of owner-domain lifecycle APIs that already exist. It
//! reuses the dashboard's task and run views rather than adding a console of
//! its own, and it holds no lifecycle state: every read is
//! [`OrbitRuntime::distributed_claim_console`] and every write ends in the same
//! canonical approve / revoke / recover transaction the owner landing consumer
//! uses.
//!
//! # Where authority comes from
//!
//! The same place every other dashboard action's does: a
//! [`GovernedOperation`](orbit_common::governance::authorization::GovernedOperation)
//! row resolved through the shared chokepoint. There is no second authority
//! model here, and no action is gated by a disabled button — the capability
//! projection in the read response exists so the UI can *explain* a refusal it
//! would get anyway, and each mutation re-resolves it server-side.
//!
//! Three more fences sit behind that one, and none of them is optional:
//!
//! - **Replica.** Coordination writes are refused in a replica checkout by
//!   Core's `ensure_coordination_task_write_permitted`. The read answers such a
//!   checkout honestly (owner machine named, no claims) instead of erroring.
//! - **Stale.** Every mutation carries the identity the operator was looking at
//!   — the candidate/base commit pair for a handoff, the phase for a claim. A
//!   mismatch is a 409 the client resolves by refreshing, decided against the
//!   owner's stored record rather than against the browser's copy of it.
//! - **Uncertain merge.** A merge intent whose reply was lost blocks revocation
//!   and recovery until it is reconciled against the provider. A database row
//!   cannot cancel a request GitHub may already have applied.
//!
//! # What it deliberately does not do
//!
//! No host registration, demotion, or fleet listing; no automatic reclamation;
//! no landing trigger. Recovery is here because there is no heartbeat, so the
//! decision that an attempt is over belongs to a human.

use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_CLAIM_RECOVER, DASHBOARD_HANDOFF_APPROVE, DASHBOARD_HANDOFF_REVOKE,
};
use orbit_core::application::review::{ExpectedCandidate, HandoffConsoleRefusal};
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use serde::Deserialize;
use serde_json::{Value, json};

use super::routines::{
    OperationsQuery, action_capability, authorization_denied, authorized_caller,
    explicit_workspace, record_operation_audit,
};
use super::{bad_request, blocking, server_error, validate_id};
use crate::state::{DashboardState, Ws};

/// Actor recorded for a dashboard-authored owner decision. The dashboard is a
/// human surface, so approval is attributed to a human even when the server
/// process itself runs inside a managed run whose ambient identity is a model.
const OPERATOR_ACTOR: &str = "human";

/// Longest accepted replay identity. Long enough for a UUID or a digest,
/// short enough that a client cannot use the mutation key as storage.
const MAX_REQUEST_ID: usize = 128;
/// Longest accepted operator reason, matching what the store records as a
/// status note.
const MAX_REASON: usize = 2000;

#[derive(Debug, Deserialize)]
pub(super) struct ApproveHandoffRequest {
    /// The candidate the operator was shown. Compared against the owner's
    /// accepted handoff and then discarded.
    expected_candidate_commit: String,
    expected_base_commit: String,
    /// Replay identity: retrying the same decision returns the stored result
    /// instead of recording a second one.
    request_id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RevokeHandoffRequest {
    expected_candidate_commit: String,
    expected_base_commit: String,
    reason: String,
    request_id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RecoverClaimRequest {
    /// The phase the operator was shown. A claim that moved on is refused.
    expected_phase: String,
    /// `blocked` for diagnosis or `backlog` for retry — the operator's choice,
    /// never inferred.
    status: String,
    reason: String,
    request_id: String,
}

/// `GET /api/distributed/claims` — read-only owner inspection.
///
/// Creates nothing: no receipt, no reservation, no claim, no task transition.
/// A replica checkout is answered rather than refused, so switching the
/// workspace selector renders an honest empty view instead of a fault.
pub(super) async fn list_claims(State(state): State<DashboardState>, Ws(runtime): Ws) -> Response {
    let operator_session = state.operator_session();
    match blocking("distributed claim console", move || {
        runtime.distributed_claim_console()
    })
    .await
    {
        Ok(mut console) => {
            if let Some(object) = console.as_object_mut() {
                object.insert("capabilities".to_string(), capabilities(operator_session));
            }
            Json(console).into_response()
        }
        Err(response) => *response,
    }
}

/// `POST /api/distributed/handoffs/:id/approve` — record completion authority
/// for one exact candidate.
///
/// Approving does not merge. It records an immutable, candidate-scoped
/// authorization and the durable landing-start request the owner landing job
/// consumes; the merge itself re-checks the authorization, the candidate and
/// the digest-pinned validation evidence in its own transaction.
pub(super) async fn approve_handoff_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Path(handoff_id): Path<String>,
    Json(body): Json<ApproveHandoffRequest>,
) -> Response {
    let handoff_id = match validate_id(&handoff_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let request_id = match validate_request_id(&body.request_id) {
        Ok(request_id) => request_id,
        Err(message) => return bad_request(message),
    };
    let expected = ExpectedCandidate {
        candidate_commit: body.expected_candidate_commit,
        base_commit: body.expected_base_commit,
    };
    let arguments = json!({
        "handoff_id": handoff_id,
        "candidate": expected.candidate_commit,
        "base": expected.base_commit,
    });
    let audited = handoff_id.clone();
    run_owner_action(
        state,
        query,
        runtime,
        &DASHBOARD_HANDOFF_APPROVE,
        "handoff.approve",
        audited,
        arguments,
        move |runtime| {
            runtime.approve_handoff_as_operator(
                &handoff_id,
                &expected,
                OPERATOR_ACTOR,
                &format!("dashboard-approve:{handoff_id}:{request_id}"),
            )
        },
    )
    .await
}

/// `POST /api/distributed/handoffs/:id/revoke` — withdraw completion authority.
///
/// The task stays in `review`: revocation cancels the pending landing request
/// and records its own immutable audit row. It does not decide what happens to
/// the work, and it is refused while an external merge intent is unresolved.
pub(super) async fn revoke_handoff_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Path(handoff_id): Path<String>,
    Json(body): Json<RevokeHandoffRequest>,
) -> Response {
    let handoff_id = match validate_id(&handoff_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let request_id = match validate_request_id(&body.request_id) {
        Ok(request_id) => request_id,
        Err(message) => return bad_request(message),
    };
    let reason = match validate_reason(&body.reason, "revoking completion authority") {
        Ok(reason) => reason,
        Err(message) => return bad_request(message),
    };
    let expected = ExpectedCandidate {
        candidate_commit: body.expected_candidate_commit,
        base_commit: body.expected_base_commit,
    };
    let arguments = json!({
        "handoff_id": handoff_id,
        "candidate": expected.candidate_commit,
        "base": expected.base_commit,
        "reason": reason,
    });
    let audited = handoff_id.clone();
    run_owner_action(
        state,
        query,
        runtime,
        &DASHBOARD_HANDOFF_REVOKE,
        "handoff.revoke",
        audited,
        arguments,
        move |runtime| {
            runtime.revoke_handoff_as_operator(
                &handoff_id,
                &expected,
                OPERATOR_ACTOR,
                &reason,
                &format!("dashboard-revoke:{handoff_id}:{request_id}"),
            )
        },
    )
    .await
}

/// `POST /api/distributed/claims/:id/recover` — deliberate recovery.
///
/// Fences the attempt, invalidates any pending landing authority, releases the
/// reservation and moves the task, all in one transaction. Nothing here infers
/// that an attempt died: age, an elapsed reservation and an absent owner-local
/// run are diagnostics the read surfaces, and this is the operator acting on
/// them.
pub(super) async fn recover_claim_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Path(claim_id): Path<String>,
    Json(body): Json<RecoverClaimRequest>,
) -> Response {
    let claim_id = match validate_id(&claim_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let request_id = match validate_request_id(&body.request_id) {
        Ok(request_id) => request_id,
        Err(message) => return bad_request(message),
    };
    let reason = match validate_reason(&body.reason, "claim recovery") {
        Ok(reason) => reason,
        Err(message) => return bad_request(message),
    };
    let status = match body.status.as_str() {
        "blocked" => TaskStatus::Blocked,
        "backlog" => TaskStatus::Backlog,
        other => {
            return bad_request(format!(
                "recovery moves a task to 'blocked' for diagnosis or 'backlog' for retry, not \
                 '{other}'"
            ));
        }
    };
    let expected_phase = body.expected_phase;
    let arguments = json!({
        "claim_id": claim_id,
        "expected_phase": expected_phase,
        "status": body.status,
        "reason": reason,
    });
    let audited = claim_id.clone();
    run_owner_action(
        state,
        query,
        runtime,
        &DASHBOARD_CLAIM_RECOVER,
        "claim.recover",
        audited,
        arguments,
        move |runtime| {
            runtime.recover_claim_as_operator(
                &claim_id,
                &expected_phase,
                status,
                OPERATOR_ACTOR,
                &reason,
                &format!("dashboard-recover:{claim_id}:{request_id}"),
            )
        },
    )
    .await
}

/// The shared body of all three mutations: authorize, require an explicit
/// workspace, run the domain transaction off the async pool, audit the outcome,
/// and translate a refusal into the code the client acts on.
#[allow(clippy::too_many_arguments)]
async fn run_owner_action<F>(
    state: DashboardState,
    query: OperationsQuery,
    runtime: std::sync::Arc<OrbitRuntime>,
    operation: &'static orbit_common::governance::authorization::GovernedOperation,
    audit_name: &'static str,
    target: String,
    arguments: Value,
    action: F,
) -> Response
where
    F: FnOnce(&OrbitRuntime) -> Result<Value, OrbitError> + Send + 'static,
{
    // Workspace selection first: an action aimed at the wrong workspace is a
    // malformed request, not a denial, and auditing it as one would be a lie.
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let started = Instant::now();
    let caller = match authorized_caller(operation, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                audit_name,
                &target,
                "",
                &arguments,
                None,
                Some(&denial),
                None,
                started,
            );
            return authorization_denied(denial);
        }
    };
    let acting = runtime.clone();
    let outcome = tokio::task::spawn_blocking(move || action(&acting)).await;
    match outcome {
        Ok(Ok(value)) => {
            record_operation_audit(
                &runtime,
                &workspace,
                audit_name,
                &target,
                "",
                &arguments,
                Some(&caller),
                None,
                None,
                started,
            );
            Json(json!({ "ok": true, "result": value })).into_response()
        }
        Ok(Err(error)) => {
            let response = refusal_response(&error);
            record_operation_audit(
                &runtime,
                &workspace,
                audit_name,
                &target,
                "",
                &arguments,
                Some(&caller),
                None,
                Some(&error.to_string()),
                started,
            );
            response
        }
        Err(join_err) => server_error(OrbitError::Execution(format!(
            "{audit_name} panicked: {join_err}"
        ))),
    }
}

/// Translate an owner-domain refusal into the status and code the dashboard
/// acts on.
///
/// The three conflict codes are distinct because the operator's next move
/// differs: refresh the view, reconcile the merge, or act on the owner machine.
/// Collapsing them into one 500 would hide exactly the state this surface
/// exists to show.
fn refusal_response(error: &OrbitError) -> Response {
    let message = error.to_string();
    match HandoffConsoleRefusal::classify(error) {
        Some(refusal @ HandoffConsoleRefusal::ReplicaCheckout) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": message, "code": refusal.code()})),
        )
            .into_response(),
        Some(refusal @ HandoffConsoleRefusal::UncertainMerge) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": refusal.code(),
                "remedy": "reconcile the recorded merge intent against the provider's actual \
                           state before revoking or recovering this claim",
            })),
        )
            .into_response(),
        Some(refusal) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": refusal.code(),
                "remedy": "refresh the view: the owner's claim or handoff state changed since \
                           this action was prepared",
            })),
        )
            .into_response(),
        // A handoff the owner no longer holds reads as stale too: the operator's
        // remedy is the same refresh, and a 404 would invite a client retry loop.
        None if message.contains("is current on this owner") => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": HandoffConsoleRefusal::NotCurrent.code(),
                "remedy": "refresh the view: this handoff or claim is no longer current",
            })),
        )
            .into_response(),
        None => match error {
            OrbitError::InvalidInput(message) => bad_request(message.clone()),
            other => server_error(OrbitError::Execution(other.to_string())),
        },
    }
}

/// The same authorization decision each mutation enforces, projected so the UI
/// can say *why* an action is unavailable instead of silently hiding it.
fn capabilities(operator_session: bool) -> Value {
    json!({
        "handoff_approve": action_capability(&DASHBOARD_HANDOFF_APPROVE, operator_session),
        "handoff_revoke": action_capability(&DASHBOARD_HANDOFF_REVOKE, operator_session),
        "claim_recover": action_capability(&DASHBOARD_CLAIM_RECOVER, operator_session),
    })
}

fn validate_request_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(
            "request_id identifies this decision so a retry replays it instead of recording a \
             second one; send one"
                .to_string(),
        );
    }
    if trimmed.len() > MAX_REQUEST_ID {
        return Err(format!(
            "request_id must be at most {MAX_REQUEST_ID} characters"
        ));
    }
    validate_id(trimmed).map(ToString::to_string)
}

fn validate_reason(raw: &str, action: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{action} requires a reason"));
    }
    if trimmed.len() > MAX_REASON {
        return Err(format!("reason must be at most {MAX_REASON} characters"));
    }
    Ok(trimmed.to_string())
}
