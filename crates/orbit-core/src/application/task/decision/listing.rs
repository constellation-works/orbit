//! Decision request reads and surface projections.

use orbit_common::OrbitError;
use orbit_types::task::{DecisionGate, DecisionRequest, DecisionRequestState};
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// Decision requests readable in a workspace scan, with corrupt bundles
/// identified separately so a partial result never looks complete.
#[derive(Debug, Default)]
pub struct DecisionRequestListing {
    /// Matching requests, oldest first.
    pub requests: Vec<DecisionRequest>,
    /// Per-task bundle corruption encountered during the scan.
    pub warnings: Vec<String>,
}

/// JSON for one request as the decision surfaces serve it: the stored
/// record with its effective (derived) state.
pub(crate) fn decision_request_json(request: &DecisionRequest) -> Value {
    serde_json::to_value(request).unwrap_or_else(|_| {
        json!({
            "id": request.id,
            "task_id": request.task_id,
            "gate": request.gate.as_str(),
            "risk_tier": request.risk_tier,
            "category": request.category,
            "state": request.state.as_str(),
        })
    })
}

/// The compact `open_decisions[]` entry `orbit.task.show` carries: the
/// request id, its gate, the answer ids a surface may offer, expiry, and the
/// decision card: the short `summary`, the `human_action` of a request only a
/// person outside Orbit can resolve, and `options` (each answer's id, label,
/// consequence and whether it is the `recommended` one) so a surface renders
/// one button per option without a second read.
pub(crate) fn open_decision_json(request: &DecisionRequest) -> Value {
    let recommended = request.recommended.as_deref();
    let mut entry = json!({
        "id": request.id,
        "gate": request.gate.as_str(),
        "risk_tier": request.risk_tier,
        "category": request.category,
        "answers": request
            .answers
            .iter()
            .map(|answer| answer.id.clone())
            .collect::<Vec<_>>(),
        "options": request
            .answers
            .iter()
            .map(|answer| {
                json!({
                    "id": answer.id,
                    "label": answer.label,
                    "consequence": answer.consequence,
                    "risk_tier": answer.risk_tier,
                    "recommended": recommended == Some(answer.id.as_str()),
                })
            })
            .collect::<Vec<_>>(),
        "expires_at": request.expires_at.to_rfc3339(),
    });
    if let Some(summary) = &request.summary {
        entry["summary"] = json!(summary);
    }
    if let Some(action) = &request.human_action {
        entry["human_action"] = json!(action);
    }
    if let Some(topic) = &request.topic {
        entry["topic"] = json!(topic);
    }
    entry
}

impl OrbitRuntime {
    /// Decision requests across the workspace, oldest first. Open requests
    /// only unless `include_closed`; `gate` and `task_id` narrow the list.
    /// Strict: any unreadable bundle fails the call. Listing surfaces use
    /// [`Self::list_decision_requests_with_warnings`] for partial results.
    pub fn list_decision_requests(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
    ) -> Result<Vec<DecisionRequest>, OrbitError> {
        Ok(self
            .scan_decision_requests(gate, task_id, include_closed, false)?
            .requests)
    }

    /// List readable requests and warn for corrupt bundles in a workspace
    /// scan. An explicit `task_id` stays strict, as do non-corruption errors.
    pub fn list_decision_requests_with_warnings(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
    ) -> Result<DecisionRequestListing, OrbitError> {
        self.scan_decision_requests(gate, task_id, include_closed, task_id.is_none())
    }

    fn scan_decision_requests(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
        tolerate_corrupt: bool,
    ) -> Result<DecisionRequestListing, OrbitError> {
        let ids: Vec<String> = match task_id {
            Some(task_id) => vec![task_id.to_string()],
            None => self
                .list_task_metadata()?
                .into_iter()
                .map(|task| task.id)
                .collect(),
        };
        let mut listing = DecisionRequestListing::default();
        for id in &ids {
            let read = self.get_task_decision_requests(id);
            let requests = if tolerate_corrupt {
                let Some(requests) =
                    super::super::listing::task_bundle_scan_result(read, &mut listing.warnings)?
                else {
                    continue;
                };
                requests
            } else {
                read?
            };
            for request in requests {
                if gate.is_some_and(|gate| gate != request.gate) {
                    continue;
                }
                if !include_closed && request.state != DecisionRequestState::Open {
                    continue;
                }
                listing.requests.push(request);
            }
        }
        listing
            .requests
            .sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        Ok(listing)
    }

    /// One decision request by id, with its effective state, or `None`
    /// when no task in the workspace carries it.
    pub fn get_decision_request(
        &self,
        request_id: &str,
    ) -> Result<Option<DecisionRequest>, OrbitError> {
        Ok(self
            .list_decision_requests(None, None, true)?
            .into_iter()
            .find(|request| request.id == request_id))
    }

    /// The task's open decision requests in the compact show form: id,
    /// gate, answer ids and expiry per request.
    pub fn open_decisions_json(&self, task_id: &str) -> Result<Value, OrbitError> {
        Ok(Value::Array(
            self.get_task_decision_requests(task_id)?
                .iter()
                .filter(|request| request.state == DecisionRequestState::Open)
                .map(open_decision_json)
                .collect(),
        ))
    }
}
