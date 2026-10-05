//! What a follower's settle-only pass did with each pull admission
//! [ORB-13663], and the crews a pull drain's window can run [ORB-13941].
//!
//! Settlement no longer belongs to the drain that admitted a claim. The
//! admission record is the outbox: the leaf's own worker records and delivers
//! its settlement as it terminalizes, and any other follower process —
//! `orbit run cancel`, `orbit run auto --stop`, a new drain — delivers whatever
//! is still recorded. This is the report those surfaces print, one entry per
//! admission that still held a slot when the pass started.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionCrewCapability, ClaimMutation, LocalPullAdmission, LocalPullPhase,
};
use orbit_types::workflow::{CrewExclusion, CrewExclusionSource, PullCrewPreflight};
use serde::Serialize;

/// One admission carried by a settle-only pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullSettlementEntry {
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    pub request_id: String,
    pub task_id: Option<String>,
    pub leaf_run_id: Option<String>,
    /// Where the admission stands after the pass:
    ///
    /// - `settled` — the owner accepted its handoff, failure or release;
    /// - `closed_obsolete` — the owner had already ended the claim, so the
    ///   settlement was closed locally (ORB-13639);
    /// - `leaf_running` — its leaf is live and settles itself when it ends;
    /// - `pending_delivery` — a settlement is recorded but did not reach the
    ///   owner; any later pass retries it;
    /// - `settlement_refused` — the owner refused the recorded settlement while
    ///   it still holds the claim; delivery backs off until the owner accepts
    ///   it ([ORB-13979]);
    /// - `release_held` — a forced cancel released the claim but could not
    ///   confirm its leaf stopped; the owner keeps the claim until it does;
    /// - `owner_unreachable` — skipped after an earlier delivery to the same
    ///   owner failed in this pass;
    /// - `awaiting_drain` — not launched yet, and a live drain for its owner
    ///   will carry it;
    /// - `unanswered_request` — no live drain will carry it and the owner
    ///   holds no receipt for the request, so nothing is held on the owner;
    /// - `launch_uncertain` — a launch was never acknowledged; deliberate
    ///   recovery is required;
    /// - `idle` / `refused` — an unanswered request the owner answered with
    ///   nothing ready, or refused, so no claim exists;
    /// - `pending` — stopped by the error in `detail`; any later pass retries;
    /// - `no_owner_route` — this runtime has no federated route to the owner.
    pub outcome: String,
    /// The error that stopped the admission, when one did.
    pub detail: Option<String>,
}

impl PullSettlementEntry {
    /// One line for a terminal report.
    ///
    /// An outcome token alone does not tell an operator whether anything is
    /// left to do, so a line with no recorded error carries what the outcome
    /// means and the next step. `launch_uncertain` always carries it: it is
    /// the one outcome that must not be answered with a second attempt.
    #[must_use]
    pub fn describe(&self) -> String {
        let subject = match (&self.task_id, &self.leaf_run_id) {
            (Some(task), Some(leaf)) => format!("{task} (leaf {leaf})"),
            (Some(task), None) => task.clone(),
            (None, Some(leaf)) => format!("leaf {leaf}"),
            (None, None) => format!("request {}", self.request_id),
        };
        let guidance = outcome_guidance(&self.outcome);
        match (&self.detail, guidance) {
            (Some(detail), Some(guidance)) if self.outcome == "launch_uncertain" => {
                format!("{subject}: {} — {detail} ({guidance})", self.outcome)
            }
            (Some(detail), _) => format!("{subject}: {} — {detail}", self.outcome),
            (None, Some(guidance)) => format!("{subject}: {} — {guidance}", self.outcome),
            (None, None) => format!("{subject}: {}", self.outcome),
        }
    }
}

/// What an outcome means for the operator, when it is not self-evident.
fn outcome_guidance(outcome: &str) -> Option<&'static str> {
    match outcome {
        "closed_obsolete" => {
            Some("the owner had already ended this claim; decide the task's status on the owner")
        }
        "leaf_running" => Some("still running; it settles itself when it ends"),
        "pending_delivery" | "owner_unreachable" => Some(
            "recorded but not delivered to the owner; the clock sweep retries it, or rerun \
             `orbit run auto --stop` once it is reachable",
        ),
        "pending" => Some("stopped by an error; any later pass retries it"),
        "settlement_refused" => Some(SETTLEMENT_REFUSAL_REMEDY),
        "release_held" => Some(
            "its leaf may still be running, so the owner keeps the claim; the release is \
             delivered once the leaf is seen to stop",
        ),
        "awaiting_drain" => Some("a live drain for this owner will carry it"),
        "unanswered_request" => Some("the owner holds no receipt for it, so nothing is held there"),
        "launch_uncertain" => Some(
            "the launch was never acknowledged, so the leaf may still be running; recover the \
             claim on the owner's dashboard and do not start another attempt",
        ),
        "no_owner_route" => Some("add the owner to ~/.orbit/mcp-destinations.toml"),
        _ => None,
    }
}

/// The first wait after the owner refuses a pending settlement while it still
/// holds the claim [ORB-13979]. Each further refusal doubles it, up to
/// [`SETTLEMENT_REFUSAL_MAX_BACKOFF`].
pub(crate) const SETTLEMENT_REFUSAL_FIRST_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(60);

/// The longest an automatic pass waits between deliveries of a settlement
/// the owner keeps refusing, so a fixed owner still settles it within this.
pub(crate) const SETTLEMENT_REFUSAL_MAX_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// How long automatic passes wait after the owner refused a settlement
/// `refusals` times in a row.
pub(crate) fn settlement_refusal_backoff(refusals: u32) -> std::time::Duration {
    let doublings = refusals.saturating_sub(1).min(16);
    SETTLEMENT_REFUSAL_FIRST_BACKOFF
        .saturating_mul(1 << doublings)
        .min(SETTLEMENT_REFUSAL_MAX_BACKOFF)
}

/// What an operator does about a settlement the owner keeps refusing.
const SETTLEMENT_REFUSAL_REMEDY: &str = "the owner refuses this recorded outcome while it \
     holds the claim; fix the condition its reason names on the owner. Automatic passes \
     retry with backoff, at most every 15 minutes, and settle it once the owner accepts; \
     `orbit run auto --stop` retries it at once. Recovering the claim on the owner's \
     dashboard closes it instead";

/// A settlement the owner refused while it still holds the claim
/// [ORB-13979]: the record stays `settling`, its outcome kept, and delivery
/// backs off rather than asking again on every pass. `orbit run show` lists
/// these for the drain carrying them, and the drain's passes admit nothing new
/// while any is held for their owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusedPullSettlement {
    /// The owner's task the settlement is for.
    pub task_id: Option<String>,
    pub leaf_run_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    /// The owner's refusal, as it answered.
    pub reason: String,
    /// Consecutive refused deliveries.
    pub refusals: u32,
    pub first_refused_at: DateTime<Utc>,
    pub last_refused_at: DateTime<Utc>,
    /// No automatic pass delivers it before this.
    pub retry_after: DateTime<Utc>,
    /// What the operator does about it.
    pub remedy: String,
}

impl RefusedPullSettlement {
    /// The refusal `record` is held on, when it is a `settling` admission
    /// the owner refused.
    pub(crate) fn of(record: &LocalPullAdmission) -> Option<Self> {
        if record.phase != LocalPullPhase::Settling {
            return None;
        }
        let refusal = record.settlement_refusal.as_ref()?;
        Some(Self {
            task_id: record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
                .map(|claim| claim.task_id.clone()),
            leaf_run_id: record.leaf_run_id.clone(),
            owner: record.destination.selector.clone(),
            drain_run_id: record.request.run_context.run_id.clone(),
            reason: refusal.reason.clone(),
            refusals: refusal.refusals,
            first_refused_at: refusal.first_refused_at,
            last_refused_at: refusal.last_refused_at,
            retry_after: refusal.retry_after,
            remedy: SETTLEMENT_REFUSAL_REMEDY.to_string(),
        })
    }

    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}) owner {} refused it {} time(s) since {}: {}; next attempt after {} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id.as_deref().unwrap_or("-"),
            self.owner,
            self.refusals,
            self.first_refused_at.to_rfc3339(),
            self.reason,
            self.retry_after.to_rfc3339(),
            self.remedy
        )
    }
}

/// Settlements this follower recorded but never delivered to the owner.
///
/// A live or cancelling drain retries delivery on every pass, and a leaf's
/// own worker retries its settlement with a bounded backoff when no drain
/// carries its owner. What outlasts both is retried by the OS clock sweep,
/// or waits for the next drain or for an operator to run
/// `orbit run auto --stop` on a host without the clock. This is the
/// read-only summary `orbit doctor` reports so the wait is visible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PendingPullSettlements {
    /// Admissions whose outcome is recorded but not delivered.
    pub count: usize,
    /// When the oldest such admission's leaf ended, and so when its outcome
    /// was recorded. `None` when none is pending or no ended leaf run can be
    /// read for any of them.
    pub oldest_recorded_at: Option<DateTime<Utc>>,
}

impl PendingPullSettlements {
    /// How long the oldest pending settlement has waited, at `now`.
    #[must_use]
    pub fn oldest_age(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        self.oldest_recorded_at
            .map(|recorded| (now - recorded).max(chrono::Duration::zero()))
    }
}

/// The owner claim a local pull leaf run executes, as this follower recorded
/// it. `orbit run show <leaf-run>` reads this so a leaf's run page names the
/// task it works for, which owner holds the claim, and whether the outcome has
/// reached that owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullLeafClaim {
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// The owner's claim on that task.
    pub claim_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    /// Where the follower's record of this admission stands (`launched`,
    /// `settling`, `settled`, ...).
    pub settlement_phase: String,
    /// Why the owner refused the settlement, once it had already ended the
    /// claim.
    pub refusal: Option<String>,
    /// The owner's refusal of the pending settlement while it still holds
    /// the claim, and when delivery is next attempted.
    pub settlement_refusal: Option<RefusedPullSettlement>,
    /// What the phase means for the operator.
    pub guidance: String,
}

impl PullLeafClaim {
    /// One line for a run page.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "task {} claim {} owner {} drain {} settlement={} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.claim_id.as_deref().unwrap_or("-"),
            self.owner,
            self.drain_run_id,
            self.settlement_phase,
            self.guidance
        )
    }
}

/// A claimed leaf a pull drain is still carrying: launched, not yet
/// terminal. `orbit run cancel` (graceful) waits for these, `--force` stops
/// them, and `orbit run auto --stop` and `orbit run show` list them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainClaimedLeaf {
    pub leaf_run_id: String,
    /// The leaf's job: one of the claimed leaf definitions.
    pub job_id: String,
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim; an earlier drain's admission
    /// is carried by the live drain for the same owner.
    pub admitted_by: String,
    /// The leaf run's state (`running`, `pending`, ...).
    pub leaf_state: String,
    /// Where the follower's record of this admission stands.
    pub settlement_phase: String,
}

impl DrainClaimedLeaf {
    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}) {} settlement={}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id,
            self.leaf_state,
            self.settlement_phase
        )
    }
}

/// The crews a pull drain's window can run, and those it will not [ORB-13941].
///
/// The window's provider preflight, taken once when it opened, minus every
/// crew a claimed leaf of this drain found unusable since — a provider that
/// refused to authenticate, say. Derived from the drain's own admission
/// records, so it survives a follower restart and ends with the drain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullCrewWindow {
    /// When the preflight ran; `None` while the drain has not taken one.
    pub checked_at: Option<DateTime<Utc>>,
    /// Crews the window can run. `None` without a preflight: then every crew
    /// not excluded is offered to the owner.
    pub runnable: Option<Vec<String>>,
    /// The crew a task naming none runs as on this host.
    pub default_crew: Option<String>,
    /// Crews excluded for the rest of the window, with why.
    pub excluded: Vec<CrewExclusion>,
}

impl PullCrewWindow {
    /// What the drain declares to the owner on each request.
    #[must_use]
    pub fn capability(&self) -> AdmissionCrewCapability {
        AdmissionCrewCapability {
            runnable: self.runnable.clone(),
            default_crew: self.default_crew.clone(),
            excluded: self.excluded.clone(),
        }
    }

    /// Whether the window can run nothing at all: every configured crew is
    /// excluded, so requesting work would only collect idle receipts.
    #[must_use]
    pub fn runs_nothing(&self) -> bool {
        self.runnable.as_ref().is_some_and(Vec::is_empty)
    }

    /// One line per excluded crew, for a terminal report.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.excluded.len() + 1);
        if let Some(runnable) = &self.runnable {
            lines.push(if runnable.is_empty() {
                "runnable: none".to_string()
            } else {
                format!("runnable: {}", runnable.join(", "))
            });
        }
        lines.extend(self.excluded.iter().map(|exclusion| {
            let source = match exclusion.source {
                CrewExclusionSource::Preflight => "preflight",
                CrewExclusionSource::ProviderUnavailable => "provider_unavailable",
            };
            format!(
                "excluded {} ({source}): {}",
                exclusion.crew, exclusion.reason
            )
        }));
        lines
    }
}

fn phase_guidance(
    phase: LocalPullPhase,
    refusal: Option<&str>,
    refused: Option<&RefusedPullSettlement>,
) -> String {
    use LocalPullPhase as Phase;
    if let Some(refused) = refused {
        return format!(
            "refused by the owner ({}); {}",
            refused.reason, refused.remedy
        );
    }
    match (phase, refusal) {
        (Phase::Settled, Some(refusal)) => format!(
            "the owner had already ended this claim ({refusal}); decide the task's status on \
             the owner"
        ),
        (Phase::Settled, None) => "the outcome was delivered to the owner".to_string(),
        (Phase::Settling, _) => "the outcome is recorded but not delivered to the owner; \
             the clock sweep retries delivery, as does `orbit run auto --stop`"
            .to_string(),
        (Phase::Launched, _) => {
            "the leaf settles itself when it ends; nothing to do while it runs".to_string()
        }
        (Phase::Launching, _) => "the launch was never acknowledged; recover the claim on the \
             owner's dashboard rather than starting another attempt"
            .to_string(),
        _ => "admitted, not launched yet".to_string(),
    }
}

impl crate::OrbitRuntime {
    /// The admissions a pull drain carries that still hold a slot: every one
    /// it made itself and, while it is live, those for its owner whose own
    /// drain has ended — it took them over. Never one another live drain
    /// made, so cancelling one drain cannot touch another's work. Empty for
    /// every other run.
    pub(crate) fn pull_drain_admissions(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<orbit_store::contracts::LocalPullAdmission>, OrbitError> {
        let jobs = self.stores().jobs();
        let Some(run) = jobs.get_job_run(drain_run_id)? else {
            return Ok(Vec::new());
        };
        if run.job_id != super::PULL_DRAIN_JOB {
            return Ok(Vec::new());
        }
        let inherits = (!run.state.is_terminal())
            .then(|| {
                run.input
                    .as_ref()
                    .and_then(|input| input.get("destination"))
                    .cloned()
                    .and_then(|value| {
                        serde_json::from_value::<orbit_store::contracts::PullDestination>(value)
                            .ok()
                    })
            })
            .flatten();
        let mut carried = Vec::new();
        for record in jobs.unsettled_local_pull_admissions()? {
            let admitted_by = &record.request.run_context.run_id;
            let own = admitted_by == drain_run_id;
            if own
                || (inherits.as_ref() == Some(&record.destination)
                    && jobs
                        .get_job_run(admitted_by)?
                        .is_none_or(|admitter| admitter.state.is_terminal()))
            {
                carried.push(record);
            }
        }
        Ok(carried)
    }

    /// The claimed leaves a pull drain is still carrying: launched and not
    /// yet terminal. Empty for every other run.
    pub fn pull_drain_claimed_leaves(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<DrainClaimedLeaf>, OrbitError> {
        use LocalPullPhase as Phase;
        let jobs = self.stores().jobs();
        let mut leaves = Vec::new();
        for record in self.pull_drain_admissions(drain_run_id)? {
            if !matches!(
                record.phase,
                Phase::Launching | Phase::Launched | Phase::Settling
            ) {
                continue;
            }
            let Some(leaf) = record.leaf_run_id.as_deref() else {
                continue;
            };
            let Some(run) = jobs.get_job_run(leaf)? else {
                continue;
            };
            if run.state.is_terminal() {
                continue;
            }
            leaves.push(DrainClaimedLeaf {
                leaf_run_id: run.run_id.clone(),
                job_id: run.job_id.clone(),
                task_id: record
                    .receipt
                    .as_ref()
                    .and_then(|receipt| receipt.claim.as_ref())
                    .map(|claim| claim.task_id.clone()),
                owner: record.destination.selector.clone(),
                admitted_by: record.request.run_context.run_id.clone(),
                leaf_state: run.state.to_string(),
                settlement_phase: phase_name(record.phase),
            });
        }
        Ok(leaves)
    }

    /// The settlements a pull drain carries that its owner refused while
    /// still holding their claims [ORB-13979]. Empty for every other run.
    pub fn pull_drain_refused_settlements(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<RefusedPullSettlement>, OrbitError> {
        Ok(self
            .pull_drain_admissions(drain_run_id)?
            .iter()
            .filter_map(RefusedPullSettlement::of)
            .collect())
    }

    /// The crew window of a pull drain run; `None` for every other run.
    pub fn pull_drain_crew_window(
        &self,
        drain_run_id: &str,
    ) -> Result<Option<PullCrewWindow>, OrbitError> {
        let jobs = self.stores().jobs();
        match jobs.get_job_run(drain_run_id)? {
            Some(run) if run.job_id == super::PULL_DRAIN_JOB => {}
            _ => return Ok(None),
        }
        let preflight = self
            .read_run_state(drain_run_id)?
            .and_then(|state| state.pull_crew_preflight);
        self.crew_window_from(drain_run_id, preflight).map(Some)
    }

    /// [`Self::pull_drain_crew_window`] over a preflight the caller holds,
    /// for a pass that could not persist the one it just took.
    pub(crate) fn crew_window_from(
        &self,
        drain_run_id: &str,
        preflight: Option<PullCrewPreflight>,
    ) -> Result<PullCrewWindow, OrbitError> {
        let mut excluded = preflight
            .as_ref()
            .map(|preflight| preflight.excluded.clone())
            .unwrap_or_default();
        for record in self
            .stores()
            .jobs()
            .local_pull_claims_admitted_by(drain_run_id)?
        {
            let Some(ClaimMutation::Release(evidence)) = &record.settlement else {
                continue;
            };
            let Some(unavailable) = &evidence.provider_unavailable else {
                continue;
            };
            let Some(crew) = unavailable.crew.as_deref() else {
                continue;
            };
            if excluded.iter().any(|exclusion| exclusion.crew == crew) {
                continue;
            }
            let task = record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
                .map(|claim| claim.task_id.as_str())
                .unwrap_or("a claimed task");
            excluded.push(CrewExclusion {
                crew: crew.to_string(),
                source: CrewExclusionSource::ProviderUnavailable,
                reason: format!("{task} failed: {}", unavailable.reason),
            });
        }
        let runnable = preflight.as_ref().map(|preflight| {
            preflight
                .runnable
                .iter()
                .filter(|crew| !excluded.iter().any(|exclusion| &exclusion.crew == *crew))
                .cloned()
                .collect()
        });
        Ok(PullCrewWindow {
            checked_at: preflight.as_ref().map(|preflight| preflight.checked_at),
            runnable,
            default_crew: match preflight {
                Some(preflight) => preflight.default_crew,
                None => self
                    .context
                    .settings()
                    .default_crew()
                    .map(ToOwned::to_owned),
            },
            excluded,
        })
    }

    /// The pull admission a local run executes, when it is a claimed leaf.
    /// `None` for every other run, including runs on an owner checkout.
    pub fn pull_leaf_claim(
        &self,
        run_id: &str,
    ) -> Result<Option<PullLeafClaim>, orbit_common::OrbitError> {
        let Some(admission) = self.claimed_leaf_admission(run_id)? else {
            return Ok(None);
        };
        let claim = admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref());
        let settlement_phase = phase_name(admission.phase);
        let settlement_refusal = RefusedPullSettlement::of(&admission);
        Ok(Some(PullLeafClaim {
            task_id: claim.map(|claim| claim.task_id.clone()),
            claim_id: claim.map(|claim| claim.claim_id.clone()),
            owner: admission.destination.selector.clone(),
            drain_run_id: admission.request.run_context.run_id.clone(),
            settlement_phase,
            refusal: admission.refusal.clone(),
            guidance: phase_guidance(
                admission.phase,
                admission.refusal.as_deref(),
                settlement_refusal.as_ref(),
            ),
            settlement_refusal,
        }))
    }
}

fn phase_name(phase: LocalPullPhase) -> String {
    serde_json::to_value(phase)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Whether the owner answered a pull with a refusal, as opposed to a lost or
/// uncertain delivery.
///
/// Only an answer counts. A refusal from the owner's pre-admission ladder —
/// selector, capability, shape, version, ship mode, review policy, a stale
/// ship contract — commits nothing, so it is safe to close the request once
/// the owner also confirms it holds no receipt for it. A delivery miss, a lost
/// answer or a store failure says nothing about whether an earlier send of the
/// same request committed, so the request stays pending and is retried.
pub(crate) fn is_owner_refusal(error: &OrbitError) -> bool {
    match error {
        OrbitError::RemoteTool { code, .. } => matches!(
            code.as_str(),
            "invalid_input" | "capability_refused" | "capability_denied" | "policy_denied"
        ),
        OrbitError::InvalidInput(_)
        | OrbitError::CapabilityRefused(_)
        | OrbitError::CapabilityDenied(_)
        | OrbitError::PolicyDenied(_) => true,
        _ => false,
    }
}

/// Whether an error says the owner could not be reached or did not answer
/// cleanly: an unreachable or stale route, a lost or unknown outcome, an owner
/// that is unavailable, or an owner-side failure that is not a refusal.
///
/// Only these mean the next call to the same owner is likely to fail or hang
/// the same way, so a pass stops calling that owner after the first one. A
/// refusal is an answer, and a local error (a store read, a missing binding)
/// says nothing about the owner at all.
pub(crate) fn is_owner_transport_failure(error: &OrbitError) -> bool {
    match error {
        OrbitError::RemoteTool { .. } => !is_owner_refusal(error),
        OrbitError::UnreachableDestination(_)
        | OrbitError::OutcomeUnknown { .. }
        | OrbitError::OwnerUnavailable(_)
        | OrbitError::OwnerNegotiation(_)
        | OrbitError::StaleRoute(_)
        | OrbitError::UnhealthyCheckout(_) => true,
        _ => false,
    }
}
