//! Decision request reads and surface projections.

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::TaskDecisionSnapshot;
use orbit_types::task::{
    DecisionGate, DecisionRequest, DecisionRequestState, Task, TaskComment, TaskHistoryEntry,
};
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

/// Which existing decision-list envelope a surface serves.
///
/// The records, count and warnings are identical; only the envelope
/// field names differ, because both are established consumer contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionListProfile {
    /// CLI / MCP tool envelope: `requests` / `count`.
    Tool,
    /// Dashboard HTTP envelope: `items` / `total`.
    Dashboard,
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

    /// The shared `decision.list` operation every surface serves.
    ///
    /// Owns gate validation, the optional exact `topic` filter (tool
    /// profile only; dashboard callers pass `None`), the warning-tolerant
    /// listing, request projection, counts and warnings. Surfaces keep
    /// only their input parser and renderer: they pass the raw gate
    /// name, task id, topic and closed flag they parsed, plus the
    /// [`DecisionListProfile`] whose envelope they serve, and render
    /// the returned JSON unchanged.
    ///
    /// An unknown `gate_name` is refused even when no requests exist.
    /// The Tool envelope is `{requests, count}` and the Dashboard
    /// envelope is `{items, total}`; a non-empty `warnings` list is
    /// added to either.
    pub fn list_decision_requests_projected(
        &self,
        gate_name: Option<&str>,
        task_id: Option<&str>,
        topic: Option<&str>,
        include_closed: bool,
        profile: DecisionListProfile,
    ) -> Result<Value, OrbitError> {
        let gate = gate_name
            .map(|name| {
                DecisionGate::from_name(name).ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "unknown decision gate `{name}`; expected agent_request, block_triage, model_answer_review, pilot_admission, proposed_approval, resident_authority, review_approval or script_grant"
                    ))
                })
            })
            .transpose()?;
        let mut listing =
            self.list_decision_requests_with_warnings(gate, task_id, include_closed)?;
        if let Some(topic) = topic {
            listing
                .requests
                .retain(|request| request.topic.as_deref() == Some(topic));
        }
        let requests = listing
            .requests
            .iter()
            .map(decision_request_json)
            .collect::<Vec<_>>();
        let count = requests.len();
        let mut value = match profile {
            DecisionListProfile::Tool => json!({ "requests": requests, "count": count }),
            DecisionListProfile::Dashboard => json!({ "items": requests, "total": count }),
        };
        if !listing.warnings.is_empty() {
            value["warnings"] = json!(listing.warnings);
        }
        Ok(value)
    }

    /// Projection inputs outside the bundle. A different tool registry,
    /// typed-decision boundary or native platform must not reuse a pilot
    /// request's closed state from another runtime. Bump the version when
    /// changing decision folding semantics across binary versions.
    pub(crate) fn decision_projection_key(&self) -> String {
        let mut tools = self.allowlist_known_tool_names();
        tools.sort();
        orbit_common::security::release::sha256_hex(
            json!([
                "decision-projection-v1",
                std::env::consts::OS,
                self.typed_decisions_since(),
                tools
            ])
            .to_string()
            .as_bytes(),
        )
    }

    pub(crate) fn record_decision_snapshots(&self, records: &[TaskDecisionSnapshot]) {
        if !records.is_empty()
            && let Err(error) = self
                .stores()
                .tasks()
                .record_task_decision_snapshots(records)
        {
            tracing::debug!(%error, "decision snapshots not recorded");
        }
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
            None => self.decision_scan_task_ids()?,
        };
        // Proofs are per partition and cover the whole bundle, including
        // terminal tasks. Only clean, closed projections can skip a read.
        let use_cache = task_id.is_none() && self.worker_invocation().is_none();
        let stamps = if use_cache {
            self.task_bundle_stamps(&ids)
        } else {
            Default::default()
        };
        let mut snapshots = if use_cache {
            self.stores()
                .tasks()
                .task_decision_snapshots()
                .unwrap_or_else(|error| {
                    tracing::debug!(%error, "decision snapshots unavailable");
                    Default::default()
                })
        } else {
            Default::default()
        };
        let projection_key = if use_cache {
            self.decision_projection_key()
        } else {
            String::new()
        };
        let now = chrono::Utc::now();
        let mut listing = DecisionRequestListing::default();
        let mut observed = Vec::new();
        for id in ids {
            let stamp = stamps.get(&id);
            let cached = snapshots.remove(&id).filter(|snapshot| {
                stamp == Some(&snapshot.stamp)
                    && snapshot.projection_key == projection_key
                    && now >= snapshot.observed_at
                    && snapshot
                        .requests
                        .iter()
                        .all(|request| request.state != DecisionRequestState::Open)
            });
            let requests = match cached {
                Some(snapshot) => snapshot.requests,
                None => {
                    let read = self.get_task_decision_requests_for_scan(&id);
                    let (requests, observed_at) = if tolerate_corrupt {
                        let Some(requests) = super::super::listing::task_bundle_scan_result(
                            read,
                            &mut listing.warnings,
                        )?
                        else {
                            continue;
                        };
                        requests
                    } else {
                        read?
                    };
                    if let Some(stamp) = stamp {
                        observed.push(TaskDecisionSnapshot {
                            task_id: id,
                            stamp: stamp.clone(),
                            projection_key: projection_key.clone(),
                            observed_at,
                            requests: requests.clone(),
                        });
                    }
                    requests
                }
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
        self.record_decision_snapshots(&observed);
        listing
            .requests
            .sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        Ok(listing)
    }

    /// A scan needs ids, not every envelope's fields. Keep the full task
    /// population even for open-only reads: terminal bundles must still warn
    /// when corrupt. Registrations provide the complete population; this
    /// scan's whole-bundle proofs and reads supply freshness and validation,
    /// without a second envelope-index freshness pass. Other backends fall
    /// back to metadata, as do workers whose reads must route through the owner.
    fn decision_scan_task_ids(&self) -> Result<Vec<String>, OrbitError> {
        if self.worker_invocation().is_none() {
            if !self.coordination_task_reads_visible() {
                return Ok(Vec::new());
            }
            if let Some(ids) = self.stores().tasks().registered_task_bundle_ids()? {
                return Ok(ids);
            }
        }
        Ok(self
            .list_task_metadata()?
            .into_iter()
            .map(|task| task.id)
            .collect())
    }

    /// One decision request by id, with its effective state, or `None`
    /// when no task in the workspace carries it.
    pub fn get_decision_request(
        &self,
        request_id: &str,
    ) -> Result<Option<DecisionRequest>, OrbitError> {
        if self.worker_invocation().is_none() && self.coordination_task_reads_visible() {
            let owner = self
                .stores()
                .tasks()
                .decision_request_task_id(request_id)
                .unwrap_or_else(|error| {
                    tracing::debug!(%error, "decision request owner unavailable");
                    None
                });
            if let Some(owner) = owner {
                match self.get_task_decision_requests(&owner) {
                    Ok(requests) => {
                        if let Some(request) = requests
                            .into_iter()
                            .find(|request| request.id == request_id)
                        {
                            return Ok(Some(request));
                        }
                    }
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        // IDs are opaque hashes. Older bundles or a lost opportunistic
        // cache write need discovery once; subsequent lookups read the owner.
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

    /// [`Self::open_decisions_json`] over records the caller already loaded.
    pub fn open_decisions_json_in(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
        comments: &[TaskComment],
    ) -> Result<Value, OrbitError> {
        Ok(Value::Array(
            self.task_decision_requests_in(task, history, comments, chrono::Utc::now())?
                .iter()
                .filter(|request| request.state == DecisionRequestState::Open)
                .map(open_decision_json)
                .collect(),
        ))
    }
}
