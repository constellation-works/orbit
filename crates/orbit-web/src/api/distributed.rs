//! Distributed-drain claim provenance and the owner's handoff actions
//! [ORB-12516].
//!
//! # What this adapter is
//!
//! An HTTP projection of owner-domain lifecycle APIs that already exist. It
//! reuses the dashboard's task and run views rather than adding a console of
//! its own, and it holds no lifecycle state: every read and every write runs
//! through [`orbit_core::app::review::exec`], ending in the same canonical
//! approve / revoke / recover transaction the owner landing consumer uses.
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
//! no arbitrary pipeline inputs. Recovery is here because there is no heartbeat, so the
//! decision that an attempt is over belongs to a human.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_CLAIM_RECOVER, DASHBOARD_HANDOFF_APPROVE, DASHBOARD_HANDOFF_RETRY,
    DASHBOARD_HANDOFF_REVOKE,
};
use orbit_core::app::review::{
    self, DashboardAction, DashboardWrite, DashboardWritten, DistributedClaimState,
    ExpectedCandidate, HandoffConsoleRefusal,
};
use orbit_core::{Core, OrbitError, TaskStatus};
use serde::Deserialize;
use serde_json::{Value, json};

use super::routines::{
    OperationsQuery, action_capability, authorization_denied, dashboard_caller, explicit_workspace,
    unexpected_outcome,
};
use super::{bad_request, blocking, server_error, validate_id};
use crate::state::{DashboardState, WsCore};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum ClaimsStateQuery {
    #[default]
    Active,
    Settled,
    All,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ClaimsQuery {
    task: Option<String>,
    #[serde(default)]
    state: ClaimsStateQuery,
    #[serde(default)]
    detail: bool,
}

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
    /// The target task status — the operator's choice, never inferred. The
    /// owner refuses a target the task lifecycle does not allow, and holds
    /// `done` to the ordinary completion gates.
    status: String,
    reason: String,
    request_id: String,
}

/// Retry a stopped or uncertain handoff, with a durable compare-and-reserve key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetryHandoffRequest {
    expected_candidate_commit: String,
    expected_base_commit: String,
    expected_latest_attempt: u32,
    expected_latest_state: orbit_types::workflow::handoff::LandingAttemptState,
    reason: String,
    request_key: String,
}

/// `GET /api/distributed/claims` — read-only owner inspection.
///
/// Creates nothing: no receipt, no reservation, no claim, no task transition.
/// A replica checkout is answered rather than refused, so switching the
/// workspace selector renders an honest empty view instead of a fault.
pub(super) async fn list_claims(
    State(state): State<DashboardState>,
    Query(query): Query<ClaimsQuery>,
    WsCore(core): WsCore,
) -> Response {
    if let Some(task_id) = query.task.as_deref()
        && let Err(message) = validate_id(task_id)
    {
        return bad_request(message);
    }
    let claim_state = match query.state {
        ClaimsStateQuery::Active => DistributedClaimState::Active,
        ClaimsStateQuery::Settled => DistributedClaimState::Settled,
        ClaimsStateQuery::All => DistributedClaimState::All,
    };
    let operator_session = state.operator_session();
    match blocking("distributed claim console", move || {
        review::exec(
            &core,
            review::Command::ClaimConsole {
                task_id: query.task,
                state: claim_state,
                detail: query.detail,
            },
        )
    })
    .await
    {
        Ok(review::Outcome::ClaimConsole(mut console)) => {
            if let Some(object) = console.as_object_mut() {
                object.insert("capabilities".to_string(), capabilities(operator_session));
            }
            Json(console).into_response()
        }
        Ok(_) => unexpected_outcome("distributed claim console"),
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
    headers: axum::http::HeaderMap,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Path(handoff_id): Path<String>,
    Json(body): Json<ApproveHandoffRequest>,
) -> Response {
    let handoff_id = match validate_id(&handoff_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let request_id = match review::replay_key(&body.request_id) {
        Ok(request_id) => request_id,
        Err(OrbitError::InvalidInput(message)) => return bad_request(message),
        Err(error) => return server_error(error),
    };
    let expected = ExpectedCandidate {
        candidate_commit: body.expected_candidate_commit,
        base_commit: body.expected_base_commit,
    };
    run_owner_action(
        state,
        headers,
        query,
        core,
        "handoff.approve",
        DashboardWrite::ApproveHandoff {
            handoff_id,
            expected,
            request_id,
        },
    )
    .await
}

/// `POST /api/distributed/handoffs/:id/retry` — normal owner landing only.
pub(super) async fn retry_handoff_action(
    State(state): State<DashboardState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Path(handoff_id): Path<String>,
    Json(body): Json<RetryHandoffRequest>,
) -> Response {
    let handoff_id = match validate_id(&handoff_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    run_owner_action(
        state,
        headers,
        query,
        core,
        "handoff.retry",
        DashboardWrite::RetryHandoff {
            handoff_id,
            expected_candidate_commit: body.expected_candidate_commit,
            expected_base_commit: body.expected_base_commit,
            expected_latest_attempt: body.expected_latest_attempt,
            expected_latest_state: body.expected_latest_state,
            reason: body.reason,
            request_key: body.request_key,
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
    headers: axum::http::HeaderMap,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Path(handoff_id): Path<String>,
    Json(body): Json<RevokeHandoffRequest>,
) -> Response {
    let handoff_id = match validate_id(&handoff_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let expected = ExpectedCandidate {
        candidate_commit: body.expected_candidate_commit,
        base_commit: body.expected_base_commit,
    };
    run_owner_action(
        state,
        headers,
        query,
        core,
        "handoff.revoke",
        DashboardWrite::RevokeHandoff {
            handoff_id,
            expected,
            reason: body.reason,
            request_id: body.request_id,
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
    headers: axum::http::HeaderMap,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Path(claim_id): Path<String>,
    Json(body): Json<RecoverClaimRequest>,
) -> Response {
    let claim_id = match validate_id(&claim_id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    // Any task status parses here; the owner's lifecycle and completion
    // gates decide which target this claim's task may take.
    let status = match body.status.parse::<TaskStatus>() {
        Ok(status) => status,
        Err(message) => return bad_request(message),
    };
    run_owner_action(
        state,
        headers,
        query,
        core,
        "claim.recover",
        DashboardWrite::RecoverClaim {
            claim_id,
            expected_phase: body.expected_phase,
            status,
            status_label: body.status,
            reason: body.reason,
            request_id: body.request_id,
        },
    )
    .await
}

/// The shared body of owner mutations: require an explicit workspace, hand the
/// action to `app::review::exec` off the async pool, and render its outcome.
async fn run_owner_action(
    state: DashboardState,
    headers: axum::http::HeaderMap,
    query: OperationsQuery,
    core: Arc<Core>,
    label: &'static str,
    write: DashboardWrite,
) -> Response {
    // Workspace selection first: an action aimed at the wrong workspace is a
    // malformed request, not a denial, and auditing it as one would be a lie.
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let caller = dashboard_caller(&state, Some(workspace), "");
    let write_caller = super::forward::web_write_caller(&state, &headers);
    let outcome = blocking(label, move || {
        review::exec(
            &core,
            review::Command::Dashboard(
                caller,
                DashboardAction {
                    write_caller,
                    write,
                },
            ),
        )
    })
    .await;
    match outcome {
        Ok(review::Outcome::Dashboard(Ok(DashboardWritten::Applied(value)))) => {
            Json(json!({ "ok": true, "result": value })).into_response()
        }
        Ok(review::Outcome::Dashboard(Ok(DashboardWritten::Refused { refusal, message }))) => {
            refusal_response(refusal, message)
        }
        Ok(review::Outcome::Dashboard(Ok(DashboardWritten::Failed(error)))) => match error {
            OrbitError::InvalidInput(message) => bad_request(message),
            other => server_error(OrbitError::Execution(other.to_string())),
        },
        Ok(review::Outcome::Dashboard(Err(denial))) => authorization_denied(denial),
        Ok(_) => unexpected_outcome(label),
        Err(response) => *response,
    }
}

/// Translate an owner-domain refusal into the status and code the dashboard
/// acts on.
///
/// The three conflict codes are distinct because the operator's next move
/// differs: refresh the view, reconcile the merge, or act on the owner machine.
/// Collapsing them into one 500 would hide exactly the state this surface
/// exists to show.
fn refusal_response(refusal: HandoffConsoleRefusal, message: String) -> Response {
    match refusal {
        HandoffConsoleRefusal::ReplicaCheckout => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": message, "code": refusal.code()})),
        )
            .into_response(),
        HandoffConsoleRefusal::UncertainMerge => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": refusal.code(),
                "remedy": "reconcile the recorded merge intent against the provider's actual \
                           state before revoking or recovering this claim",
            })),
        )
            .into_response(),
        // A handoff the owner no longer holds reads as stale too: the operator's
        // remedy is the same refresh, and a 404 would invite a client retry loop.
        HandoffConsoleRefusal::NotCurrent => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": refusal.code(),
                "remedy": "refresh the view: this handoff or claim is no longer current",
            })),
        )
            .into_response(),
        HandoffConsoleRefusal::Stale => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": message,
                "code": refusal.code(),
                "remedy": "refresh the view: the owner's claim or handoff state changed since \
                           this action was prepared",
            })),
        )
            .into_response(),
    }
}

/// The same authorization decision each mutation enforces, projected so the UI
/// can say *why* an action is unavailable instead of silently hiding it.
fn capabilities(operator_session: bool) -> Value {
    json!({
        "handoff_approve": action_capability(&DASHBOARD_HANDOFF_APPROVE, operator_session),
        "handoff_retry": action_capability(&DASHBOARD_HANDOFF_RETRY, operator_session),
        "handoff_revoke": action_capability(&DASHBOARD_HANDOFF_REVOKE, operator_session),
        "claim_recover": action_capability(&DASHBOARD_CLAIM_RECOVER, operator_session),
    })
}
