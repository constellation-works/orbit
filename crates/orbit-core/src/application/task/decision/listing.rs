//! Decision request reads and surface projections.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock, PoisonError};

use orbit_common::OrbitError;
use orbit_types::task::{
    DecisionGate, DecisionRequest, DecisionRequestState, Task, TaskComment, TaskHistoryEntry,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// Tasks a workspace decision scan found raising no request, by workspace
/// and task id, each with the whole-bundle stamp taken before the read that
/// showed it.
///
/// Requests derive only from `decision_requested` history events, so a task
/// whose history holds none lists no request in any state, whatever its
/// status, binding inputs or the clock. While its bundle stamp is unchanged
/// the store has not written the bundle since, so a read would still find
/// no event and no damage the store wrote, and the scan may skip it. Any
/// store write to the bundle changes the stamp, and the next scan reads that
/// task in full; a task the store cannot stamp is read every time. Only a
/// clean read is recorded, so a corrupt bundle keeps warning. The stamp's
/// limits are the store's (`task_bundle_stamps`): out-of-band edits its
/// stamp does not cover stay unseen until the bundle's next write.
///
/// The set lives for the process, which for the dashboard is the server:
/// the first scan after start reads every bundle, later ones only what
/// changed. Bounded at [`NoRequestTasks::CAPACITY`] entries; overflowing it
/// starts over, costing one full scan.
struct NoRequestTasks;

type NoRequestEntries = HashMap<String, HashMap<String, String>>;

impl NoRequestTasks {
    const CAPACITY: usize = 1 << 18;

    fn entries() -> std::sync::MutexGuard<'static, NoRequestEntries> {
        static ENTRIES: OnceLock<Mutex<NoRequestEntries>> = OnceLock::new();
        ENTRIES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The `ids` a scan must read, each with its current stamp when it has
    /// one: every id but those proven under the stamp they still carry.
    fn unproven(
        workspace: &str,
        ids: Vec<String>,
        mut stamps: BTreeMap<String, String>,
    ) -> Vec<(String, Option<String>)> {
        let entries = Self::entries();
        let proven = entries.get(workspace);
        ids.into_iter()
            .filter_map(|id| {
                let stamp = stamps.remove(&id);
                let holds = stamp.as_ref().is_some_and(|stamp| {
                    proven
                        .and_then(|proven| proven.get(&id))
                        .is_some_and(|recorded| recorded == stamp)
                });
                (!holds).then_some((id, stamp))
            })
            .collect()
    }

    fn record(workspace: &str, mut proven: Vec<(String, String)>) {
        if proven.is_empty() {
            return;
        }
        let mut entries = Self::entries();
        let held: usize = entries.values().map(HashMap::len).sum();
        if held + proven.len() > Self::CAPACITY {
            entries.clear();
        }
        // A single batch larger than the bound still fits after the clear,
        // so keep only the first CAPACITY entries. Anything not retained is
        // simply read in full on the next scan.
        proven.truncate(Self::CAPACITY);
        entries
            .entry(workspace.to_string())
            .or_default()
            .extend(proven);
    }
}

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
        // A workspace scan skips a task proven to raise no request: its whole
        // bundle still carries the stamp taken before an earlier clean read
        // found no `decision_requested` event. See [`NoRequestTasks`].
        let workspace = task_id
            .is_none()
            .then(|| self.workspace_id().ok())
            .flatten();
        let reads = match &workspace {
            Some(workspace) => {
                let stamps = self.task_bundle_stamps(&ids);
                NoRequestTasks::unproven(workspace, ids, stamps)
            }
            None => ids.into_iter().map(|id| (id, None)).collect(),
        };
        let mut listing = DecisionRequestListing::default();
        let mut proven = Vec::new();
        for (id, stamp) in reads {
            let read = self.get_task_decision_requests_with_events(&id);
            let (requests, raised_any) = if tolerate_corrupt {
                let Some(requests) =
                    super::super::listing::task_bundle_scan_result(read, &mut listing.warnings)?
                else {
                    continue;
                };
                requests
            } else {
                read?
            };
            if !raised_any && let Some(stamp) = stamp {
                proven.push((id, stamp));
            }
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
        if let Some(workspace) = &workspace {
            NoRequestTasks::record(workspace, proven);
        }
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
