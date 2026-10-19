//! Cumulative owner reservation for a provider base update, shared by causes.
use crate::task::TaskHistoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Persisted before the authenticated expected-head update, including lost replies.
pub const PR_BASE_UPDATE_RESERVED_EVENT: &str = "pr_base_update_reserved";

/// Exact provider/Git identity and original absolute recovery deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrBaseUpdateReservation {
    pub execution: String,
    pub repository: String,
    pub pr_number: String,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub base_sha: String,
    pub deadline_ms: u64,
    /// Adapter-owned emission proof or inherited-failure classification.
    pub evidence: Value,
}

/// Only a new reservation permits the single provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrBaseUpdateAdmission {
    Reserved,
    Observe(Box<PrBaseUpdateReservation>),
    Refused(String),
}

/// Malformed history refuses instead of granting a fresh budget.
pub fn pr_base_update_reservations(
    history: &[TaskHistoryEntry],
) -> Result<Vec<PrBaseUpdateReservation>, String> {
    history
        .iter()
        .filter(|row| row.event == PR_BASE_UPDATE_RESERVED_EVENT)
        .map(|row| {
            let reservation: PrBaseUpdateReservation = serde_json::from_str(
                row.note
                    .as_deref()
                    .ok_or("base update reservation lacks evidence")?,
            )
            .map_err(|error| format!("unreadable base update reservation: {error}"))?;
            validate(&reservation)?;
            Ok(reservation)
        })
        .collect()
}

/// One update per repository/PR across heads, causes, siblings and restarts.
pub fn decide_pr_base_update(
    history: &[TaskHistoryEntry],
    request: &PrBaseUpdateReservation,
) -> PrBaseUpdateAdmission {
    if let Err(reason) = validate(request) {
        return PrBaseUpdateAdmission::Refused(reason);
    }
    let reservations = match pr_base_update_reservations(history) {
        Ok(reservations) => reservations,
        Err(reason) => return PrBaseUpdateAdmission::Refused(reason),
    };
    if let Some(previous) = reservations
        .into_iter()
        .find(|row| row.repository == request.repository && row.pr_number == request.pr_number)
    {
        if previous.head_sha == request.head_sha
            && previous.head_branch == request.head_branch
            && previous.base_branch == request.base_branch
            && previous.base_sha == request.base_sha
            && previous.evidence == request.evidence
        {
            return PrBaseUpdateAdmission::Observe(Box::new(previous));
        }
        return PrBaseUpdateAdmission::Refused("the cumulative base update budget is spent".into());
    }
    PrBaseUpdateAdmission::Reserved
}

fn validate(request: &PrBaseUpdateReservation) -> Result<(), String> {
    let sha =
        |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|byte| byte.is_ascii_hexdigit());
    if [
        &request.execution,
        &request.repository,
        &request.pr_number,
        &request.head_branch,
        &request.base_branch,
    ]
    .iter()
    .any(|s| s.trim().is_empty())
        || !request.pr_number.bytes().all(|byte| byte.is_ascii_digit())
        || !sha(&request.head_sha)
        || !sha(&request.base_sha)
        || request.deadline_ms == 0
        || !request
            .evidence
            .as_object()
            .is_some_and(|value| !value.is_empty())
    {
        return Err("base update reservation has incomplete identity or evidence".into());
    }
    Ok(())
}
