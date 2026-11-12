//! The operator's one-shot effort backfills, never run on open: the dispatch
//! effort of invocations that completed before Orbit recorded a dispatch
//! identity, and a follower's late report to its owner of the effort of
//! leaves it settled before followers reported effort [ORB-24115].

use orbit_common::OrbitError;
use orbit_store::contracts::{LocalPullPhase, PROVIDER_DEFAULT_REPORTED_EFFORT};
use orbit_store::workflow::dispatch_effort;
use serde::Serialize;
use serde_json::{Value, json};

pub use orbit_store::workflow::dispatch_effort::DispatchEffortBackfill;

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Record a dispatch identity, with the effort its launch argv rendered,
    /// for every invocation of this workspace that has none, marked as
    /// backfilled. The crew is the run's resolved crew only for the run's
    /// executor invocation. Invocations with no matching launch, or whose
    /// matching launches disagree on effort, stay unknown. A run whose
    /// invocation never completed gets one launch record per audited launch
    /// that one step contains and that names a provider, and a launch a
    /// dispatch-time record already covers is left as that record. Running it
    /// again changes nothing.
    pub fn backfill_dispatch_effort(&self) -> Result<DispatchEffortBackfill, OrbitError> {
        let workspace_id = self.stores().jobs().workspace_id().to_string();
        dispatch_effort::backfill_dispatch_effort(
            &self.sqlite_store()?,
            &workspace_id,
            orbit_runtime::cost::IMPLEMENT_INVOCATION_IDS,
        )
    }
}

/// What a follower's late effort report for one settled claim came to
/// [ORB-24115].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimedEffortOutcome {
    /// The owner recorded the effort.
    Recorded,
    /// The owner already held this effort; nothing changed.
    Replayed,
    /// The owner holds a different effort for the claim, which stands; the
    /// report was refused.
    Conflict,
    /// This host holds no matching implementing invocation or launch record
    /// of the leaf, so nothing was sent and the attempt stays effort unknown.
    EffortUnknown,
    /// This host holds no record of the claim's leaf run, or the claim never
    /// bound one; nothing was sent.
    NoLeafRun,
    /// The owner's build does not know the report and refused it unread.
    OwnerUnsupported,
    /// The owner refused the report for another reason, named in the detail.
    Refused,
    /// The owner could not be reached or did not answer.
    OwnerUnreachable,
}

/// One settled claim's row in a [`ClaimedEffortBackfill`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimedEffortRow {
    pub task_id: String,
    pub claim_id: String,
    pub leaf_run_id: Option<String>,
    /// The effort sent (`provider_default` for the provider's default), when
    /// this host knew one.
    pub reasoning_effort: Option<String>,
    pub outcome: ClaimedEffortOutcome,
    /// Why, for every outcome but `recorded` and `replayed`.
    pub detail: Option<String>,
}

/// A follower's late effort report for the claims it settled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClaimedEffortBackfill {
    pub workspace: String,
    /// Settled claims this host holds a pull admission for.
    pub examined: usize,
    pub recorded: usize,
    pub replayed: usize,
    pub conflicting: usize,
    pub effort_unknown: usize,
    pub no_leaf_run: usize,
    pub owner_unsupported: usize,
    pub refused: usize,
    pub owner_unreachable: usize,
    pub rows: Vec<ClaimedEffortRow>,
}

impl ClaimedEffortBackfill {
    fn push(&mut self, row: ClaimedEffortRow) {
        let count = match row.outcome {
            ClaimedEffortOutcome::Recorded => &mut self.recorded,
            ClaimedEffortOutcome::Replayed => &mut self.replayed,
            ClaimedEffortOutcome::Conflict => &mut self.conflicting,
            ClaimedEffortOutcome::EffortUnknown => &mut self.effort_unknown,
            ClaimedEffortOutcome::NoLeafRun => &mut self.no_leaf_run,
            ClaimedEffortOutcome::OwnerUnsupported => &mut self.owner_unsupported,
            ClaimedEffortOutcome::Refused => &mut self.refused,
            ClaimedEffortOutcome::OwnerUnreachable => &mut self.owner_unreachable,
        };
        *count += 1;
        self.examined += 1;
        self.rows.push(row);
    }
}

/// The owner tool a follower reports a settled leaf's effort through.
const CLAIM_EFFORT_TOOL: &str = "orbit.drain.claim.effort";

/// The refusal an owner whose build predates the report answers on its
/// internal drain route for a tool it does not know.
const UNKNOWN_DRAIN_TOOL_REFUSAL: &str = "requires the internal runtime route";
/// The invalid-params response an older owner returns for an unknown
/// internal drain operation.
const UNKNOWN_DRAIN_OPERATION_REFUSAL: &str = "unknown internal drain operation";

impl OrbitRuntime {
    /// Report to each owner the reasoning effort of every leaf this follower
    /// settled for it, read from this host's own records by the rule a
    /// settlement reports (the leaf's main implementing invocation, else its
    /// latest matching launch record), so the owner's scorecards count an
    /// attempt settled before followers reported effort under that effort.
    /// The owner records a report only for a claim this machine ran, and
    /// never overwrites an effort it holds: the same effort replays, a
    /// different one is refused and counted as a conflict. A leaf this host
    /// holds no matching record of is counted and nothing is sent. Changes
    /// nothing on this host; running it again records nothing new.
    pub fn backfill_claimed_leaf_effort(&self) -> Result<ClaimedEffortBackfill, OrbitError> {
        let transport = self.drain_owner_transport().cloned().ok_or_else(|| {
            OrbitError::InvalidInput(
                "this runtime has no federated owner route; register the owner with \
                 `orbit host add <ssh-target>`"
                    .into(),
            )
        })?;
        let jobs = self.stores().jobs();
        let mut report = ClaimedEffortBackfill {
            workspace: jobs.workspace_id().to_string(),
            ..ClaimedEffortBackfill::default()
        };
        let mut unreachable = std::collections::BTreeSet::new();
        for admission in jobs.local_pull_admissions()? {
            let Some(claim) = admission
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
            else {
                continue;
            };
            if admission.phase != LocalPullPhase::Settled || admission.settlement.is_none() {
                continue;
            }
            let mut row = ClaimedEffortRow {
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                leaf_run_id: admission.leaf_run_id.clone(),
                reasoning_effort: None,
                outcome: ClaimedEffortOutcome::NoLeafRun,
                detail: None,
            };
            let leaf = match admission.leaf_run_id.as_deref() {
                Some(run_id) => jobs.get_job_run(run_id)?,
                None => None,
            };
            let Some(leaf) = leaf else {
                row.detail = Some(if admission.leaf_run_id.is_some() {
                    "this host holds no record of the claim's leaf run".into()
                } else {
                    "the claim never bound a leaf run".into()
                });
                report.push(row);
                continue;
            };
            let Some(effort) = self.leaf_reported_effort(&leaf) else {
                row.outcome = ClaimedEffortOutcome::EffortUnknown;
                row.detail = Some(
                    "this host holds no implementing invocation or launch record of the leaf \
                     with a recorded effort"
                        .into(),
                );
                report.push(row);
                continue;
            };
            let name = effort.unwrap_or_else(|| PROVIDER_DEFAULT_REPORTED_EFFORT.to_string());
            row.reasoning_effort = Some(name.clone());
            let selector = &admission.destination.selector;
            if unreachable.contains(selector) {
                row.outcome = ClaimedEffortOutcome::OwnerUnreachable;
                row.detail = Some("the owner was unreachable earlier in this run".into());
                report.push(row);
                continue;
            }
            let answer = transport
                .call(
                    selector,
                    CLAIM_EFFORT_TOOL,
                    json!({
                        "claim_id": claim.claim_id,
                        "run_id": leaf.run_id,
                        "reasoning_effort": name,
                    }),
                )
                .map_err(crate::application::distributed::owner_protocol_error);
            (row.outcome, row.detail) = match answer {
                Ok(answer) if answer.get("outcome").and_then(Value::as_str) == Some("replayed") => {
                    (ClaimedEffortOutcome::Replayed, None)
                }
                Ok(_) => (ClaimedEffortOutcome::Recorded, None),
                Err(error) => {
                    let outcome = refused_outcome(&error);
                    if outcome == ClaimedEffortOutcome::OwnerUnreachable {
                        unreachable.insert(selector.clone());
                    }
                    (outcome, Some(error.to_string()))
                }
            };
            report.push(row);
        }
        Ok(report)
    }
}

/// How an owner's error answers a late effort report.
fn refused_outcome(error: &OrbitError) -> ClaimedEffortOutcome {
    let message = error.to_string();
    if message.contains(UNKNOWN_DRAIN_TOOL_REFUSAL)
        || matches!(
            error,
            OrbitError::UnreachableDestination(message)
                if message.contains("\"code\":-32602")
                    && message.contains(UNKNOWN_DRAIN_OPERATION_REFUSAL)
        )
        || matches!(
            error,
            OrbitError::NotFound {
                kind: orbit_common::NotFoundKind::Tool,
                ..
            }
        )
    {
        ClaimedEffortOutcome::OwnerUnsupported
    } else if message.contains("effort_conflict") {
        ClaimedEffortOutcome::Conflict
    } else if crate::application::distributed::is_owner_transport_failure(error) {
        ClaimedEffortOutcome::OwnerUnreachable
    } else {
        ClaimedEffortOutcome::Refused
    }
}
