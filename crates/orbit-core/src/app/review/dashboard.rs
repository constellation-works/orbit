//! The dashboard's governed owner handoff and claim recovery actions. Each
//! calls the existing runtime method under web task-write provenance and
//! classifies owner console refusals before returning.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    AuthorizationDenial, DASHBOARD_CLAIM_RECOVER, DASHBOARD_HANDOFF_APPROVE,
    DASHBOARD_HANDOFF_RETRY, DASHBOARD_HANDOFF_REVOKE, GovernedOperation,
};
use orbit_types::task::{TaskWriteProvenance, TaskWriteSurface};
use orbit_types::workflow::handoff::LandingRetryRequest;
use serde_json::{Value, json};

use super::{DashboardAction, DashboardWrite, DashboardWritten};
use crate::Core;
use crate::app::exec::{self, DashboardCaller, DashboardRow};
use crate::application::operator_input;
use crate::application::task::with_task_write_provenance;

/// Actor recorded for a dashboard-authored owner decision. The dashboard is a
/// human surface, so approval is attributed to a human even when the server
/// process itself runs inside a managed run whose ambient identity is a model.
const OPERATOR_ACTOR: &str = "human";

enum PreparedWrite {
    Approve {
        handoff_id: String,
        expected: super::ExpectedCandidate,
        request_id: String,
    },
    Retry {
        request: LandingRetryRequest,
    },
    Revoke {
        handoff_id: String,
        expected: super::ExpectedCandidate,
        reason: String,
        request_id: String,
    },
    Recover {
        claim_id: String,
        expected_phase: String,
        status: orbit_types::task::TaskStatus,
        reason: String,
        request_id: String,
    },
}

/// Authorize one owner handoff or claim action, run it under web task-write
/// provenance, classify console refusals and record its one audit row.
pub(super) fn write(
    core: &Core,
    caller: DashboardCaller,
    action: DashboardAction,
) -> Result<Result<DashboardWritten, AuthorizationDenial>, OrbitError> {
    let DashboardAction {
        write_caller,
        write,
    } = action;
    let (governed, target, arguments, prepared) = prepare(write);
    let runtime = core.runtime();
    exec::run_dashboard_recorded(
        core,
        caller.operation(governed, &target, arguments),
        || {
            let outcome = with_task_write_provenance(
                TaskWriteProvenance::new(Some(TaskWriteSurface::Web), write_caller),
                || match prepared {
                    PreparedWrite::Approve {
                        handoff_id,
                        expected,
                        request_id,
                    } => runtime.approve_handoff_as_operator(
                        &handoff_id,
                        &expected,
                        OPERATOR_ACTOR,
                        &format!("dashboard-approve:{handoff_id}:{request_id}"),
                    ),
                    PreparedWrite::Retry { request } => {
                        let dispatch = runtime.retry_handoff_as_operator(
                            &runtime.workspace_id()?,
                            &request,
                            OPERATOR_ACTOR,
                        )?;
                        serde_json::to_value(dispatch)
                            .map_err(|error| OrbitError::Execution(error.to_string()))
                    }
                    PreparedWrite::Revoke {
                        handoff_id,
                        expected,
                        reason,
                        request_id,
                    } => runtime.revoke_handoff_as_operator(
                        &handoff_id,
                        &expected,
                        OPERATOR_ACTOR,
                        &reason,
                        "dashboard-revoke",
                        &request_id,
                    ),
                    PreparedWrite::Recover {
                        claim_id,
                        expected_phase,
                        status,
                        reason,
                        request_id,
                    } => runtime.recover_claim_as_operator(
                        &claim_id,
                        &expected_phase,
                        status,
                        OPERATOR_ACTOR,
                        &reason,
                        ("dashboard-recover", &request_id),
                    ),
                },
            );
            Ok(match outcome {
                Ok(value) => DashboardWritten::Applied(value),
                Err(error) => match runtime.handoff_console_refusal(&error) {
                    Some(refusal) => DashboardWritten::Refused {
                        refusal,
                        message: error.to_string(),
                    },
                    None => DashboardWritten::Failed(error),
                },
            })
        },
        |result, arguments| {
            let failure = match result {
                Ok(DashboardWritten::Applied(_)) => None,
                Ok(DashboardWritten::Refused { message, .. }) => Some(message.clone()),
                Ok(DashboardWritten::Failed(error)) | Err(error) => Some(error.to_string()),
            };
            Some(DashboardRow {
                failure,
                target: None,
                arguments,
            })
        },
    )
}

/// Normalize audit arguments before authorization so a denied or refused
/// request never persists an unbounded reason or an invalid replay key.
/// Invalid values are omitted from the audit projection; raw values still
/// reach the runtime method, which refuses them on the operator path.
fn prepare(write: DashboardWrite) -> (&'static GovernedOperation, String, Value, PreparedWrite) {
    match write {
        DashboardWrite::ApproveHandoff {
            handoff_id,
            expected,
            request_id,
        } => {
            let arguments = json!({
                "handoff_id": handoff_id,
                "candidate": expected.candidate_commit,
                "base": expected.base_commit,
            });
            let target = handoff_id.clone();
            (
                &DASHBOARD_HANDOFF_APPROVE,
                target,
                arguments,
                PreparedWrite::Approve {
                    handoff_id,
                    expected,
                    request_id,
                },
            )
        }
        DashboardWrite::RetryHandoff {
            handoff_id,
            expected_candidate_commit,
            expected_base_commit,
            expected_latest_attempt,
            expected_latest_state,
            reason,
            request_key,
        } => {
            let audit_reason = operator_input::reason(&reason, "retrying a handoff").ok();
            let audit_key = operator_input::replay_key(&request_key).ok();
            let request = LandingRetryRequest {
                handoff_id: handoff_id.clone(),
                expected_candidate_commit,
                expected_base_commit,
                expected_latest_attempt,
                expected_latest_state,
                reason: audit_reason.clone().unwrap_or(reason),
                request_key: audit_key.clone().unwrap_or(request_key),
            };
            let mut arguments = json!(request);
            if let Some(object) = arguments.as_object_mut() {
                if audit_reason.is_none() {
                    object.remove("reason");
                }
                if audit_key.is_none() {
                    object.remove("request_key");
                }
            }
            (
                &DASHBOARD_HANDOFF_RETRY,
                handoff_id,
                arguments,
                PreparedWrite::Retry { request },
            )
        }
        DashboardWrite::RevokeHandoff {
            handoff_id,
            expected,
            reason,
            request_id,
        } => {
            let audit_reason =
                operator_input::reason(&reason, "revoking completion authority").ok();
            let audit_key = operator_input::replay_key(&request_id).ok();
            let reason = audit_reason.clone().unwrap_or(reason);
            let request_id = audit_key.unwrap_or(request_id);
            let mut arguments = json!({
                "handoff_id": handoff_id,
                "candidate": expected.candidate_commit,
                "base": expected.base_commit,
                "reason": reason,
            });
            if audit_reason.is_none()
                && let Some(object) = arguments.as_object_mut()
            {
                object.remove("reason");
            }
            let target = handoff_id.clone();
            (
                &DASHBOARD_HANDOFF_REVOKE,
                target,
                arguments,
                PreparedWrite::Revoke {
                    handoff_id,
                    expected,
                    reason,
                    request_id,
                },
            )
        }
        DashboardWrite::RecoverClaim {
            claim_id,
            expected_phase,
            status,
            status_label,
            reason,
            request_id,
        } => {
            let audit_reason = operator_input::reason(&reason, "claim recovery").ok();
            let audit_key = operator_input::replay_key(&request_id).ok();
            let reason = audit_reason.clone().unwrap_or(reason);
            let request_id = audit_key.unwrap_or(request_id);
            let mut arguments = json!({
                "claim_id": claim_id,
                "expected_phase": expected_phase,
                "status": status_label,
                "reason": reason,
            });
            if audit_reason.is_none()
                && let Some(object) = arguments.as_object_mut()
            {
                object.remove("reason");
            }
            let target = claim_id.clone();
            (
                &DASHBOARD_CLAIM_RECOVER,
                target,
                arguments,
                PreparedWrite::Recover {
                    claim_id,
                    expected_phase,
                    status,
                    reason,
                    request_id,
                },
            )
        }
    }
}
