//! Settlement owned by the admission record, not by the admitting drain
//! [ORB-13663].
//!
//! Only the drain that admitted a claim used to settle it, from its refill
//! loop. Cancelling that drain — `orbit run cancel` or the dashboard's cancel
//! — took the only settlement path with it: leaves finished, handoffs stayed
//! recorded but undelivered, failed leaves recorded nothing, and the owner kept
//! every claim `running`.
//!
//! Now the admission record is the outbox, and settlement is carried by
//! whichever follower process gets there first:
//!
//! - **The leaf's own worker.** Run terminalization records the failure a
//!   terminal leaf implies (success already recorded its typed handoff), and
//!   the bound worker then delivers its own settlement. When the owner cannot
//!   be reached and no live drain carries its owner, the worker retries with
//!   a bounded backoff before it exits. The worker is an unsandboxed Orbit
//!   process with the host's federated owner route; only the agent subprocess
//!   inside a leaf runs under the sandbox, which is why the agent itself never
//!   talks to the owner (ORB-13642).
//! - **The drain's own passes**, live or cancelling, which also reconcile a
//!   leaf whose worker died, so its failure is recorded and delivered in the
//!   same pass.
//! - **The OS clock sweep** ([`OrbitRuntime::deliver_recorded_pull_settlements`]),
//!   every tick, over owner and replica checkouts alike: what is recorded is
//!   delivered once neither the leaf's worker nor a drain is left to, but
//!   nothing unlaunched is ever ended there.
//! - **Any settle-only pass** ([`OrbitRuntime::settle_pending_pulls`]):
//!   `orbit run cancel` and the dashboard's cancel, `orbit run auto --stop`
//!   and the dashboard's stop. A pass delivers recorded settlements for every
//!   owner, and ends the unlaunched admissions no live drain will carry (see
//!   [`SettleScope::Abandon`]).
//! - **A new drain** for the same owner, whose refill carries every earlier
//!   admission for that owner forward, whichever drain made it.
//!
//! Once settled, a leaf's `target/` build output is reclaimed by the drain's
//! next pass ([`OrbitRuntime::reclaim_settled_leaf_build_output`]); its
//! checkout is left to worktree GC.
//!
//! Delivery stays the idempotent owner mutation it always was — one mutation
//! ID per claim, receipt reconciliation of a refusal, `stale_claim` closing a
//! settlement the owner already ended — so two processes settling the same
//! admission deliver it once.
//!
//! A settlement the owner refuses while it still holds the claim is recorded
//! as refused and backs off [ORB-13979]: the drain's passes, the clock sweep
//! and the leaf's worker deliver it again only once its backoff has elapsed
//! (doubling to at most 15 minutes), while an operator's settle-only pass
//! delivers it at once ([`RefusedDelivery::Now`]).

use std::collections::BTreeSet;
use std::time::Duration;

use super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::drain::{
    PullDrain, RefusedDelivery, SettleScope, leaf_failure_settlement, release_settlement,
};
use crate::OrbitRuntime;
use crate::application::distributed::{
    PULL_DRAIN_JOB, PendingPullSettlements, PullSettlementEntry, is_owner_transport_failure,
};
use orbit_common::OrbitError;
use orbit_engine::run_worktree_has_build_output;
use orbit_store::contracts::{
    JobRunQuery, LocalPullAdmission, LocalPullMutation, LocalPullPhase, PullDestination,
};

/// Why a settle-only pass releases unlaunched work no live drain carries.
const DRAIN_ENDED_CAUSE: &str = "the drain ended before launching this task";

/// How long a leaf's own worker waits between delivery attempts when its
/// settlement did not reach the owner and no drain will retry it. Bounded: a
/// worker does not outlive its leaf for long, and whatever is still pending
/// afterwards waits in the outbox for the next pass.
const LEAF_SETTLEMENT_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(240),
];

impl OrbitRuntime {
    /// Record a terminal claimed leaf's settlement in its admission. Local
    /// only: nothing is sent to the owner.
    ///
    /// Called from run terminalization in whichever process terminalizes the
    /// leaf — its worker, a cancel, orphan reconciliation — so a failed leaf
    /// has its failure recorded whether or not any drain is watching. A
    /// `Created` admission is left alone: its bind may not have reached the
    /// owner, and the pass that settles it binds first.
    pub(crate) fn record_claimed_leaf_settlement(
        &self,
        run_id: &str,
        diagnostic: Option<(&str, &str)>,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        let jobs = self.stores().jobs();
        let Some(record) = jobs.local_pull_for_run(run_id)? else {
            return Ok(None);
        };
        if record.settlement.is_some()
            || !matches!(
                record.phase,
                LocalPullPhase::Bound | LocalPullPhase::Launching | LocalPullPhase::Launched
            )
        {
            return Ok(Some(record));
        }
        let Some(run) = jobs.get_job_run(run_id)? else {
            return Ok(Some(record));
        };
        if !run.state.is_terminal() {
            return Ok(Some(record));
        }
        let state = jobs.read_run_state(run_id)?;
        let final_recovery = state
            .as_ref()
            .and_then(|state| state.final_recovery.as_ref());
        let cancellation_policy = state
            .as_ref()
            .and_then(|state| state.task_cancellation_policy.as_ref());
        let settlement = leaf_failure_settlement(
            &record,
            &run,
            diagnostic,
            final_recovery,
            cancellation_policy,
        );
        match jobs.mutate_local_pull(
            &record.destination,
            &record.request.request_id,
            &LocalPullMutation::Settle(Box::new(settlement)),
        ) {
            Ok(settling) => Ok(Some(settling)),
            // Another process recorded this claim's settlement first; that one
            // is delivered.
            Err(error) => match jobs.local_pull_for_run(run_id)? {
                Some(current) if current.settlement.is_some() => Ok(Some(current)),
                _ => Err(error),
            },
        }
    }

    /// Run terminalization's settlement hook: record the leaf's settlement,
    /// and deliver it when this process is the leaf's own bound worker.
    /// Best-effort — a failure is logged and the record stays in the outbox
    /// for the next settle-only pass, so a run always reaches its terminal
    /// state.
    ///
    /// Other terminalizing processes only record: orphan reconciliation runs
    /// while a workspace opens and must not wait on the owner, and a cancel
    /// delivers from its own entry point.
    pub(crate) fn best_effort_settle_terminal_claimed_leaf(
        &self,
        run_id: &str,
        diagnostic: Option<(&str, &str)>,
    ) {
        match self.record_claimed_leaf_settlement(run_id, diagnostic) {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.pull",
                    run_id,
                    %error,
                    "could not record the terminal claimed leaf's settlement; a settle-only pass \
                     or the next drain records it",
                );
                return;
            }
        }
        if self
            .worker_invocation()
            .is_some_and(|binding| binding.bound_run_id == run_id)
        {
            self.deliver_claimed_leaf_settlement(run_id);
        }
    }

    /// The leaf's own worker, once its run is final: when the settlement it
    /// delivered at terminalization is still undelivered (the owner was down
    /// or did not answer), retry with a bounded backoff while no live drain
    /// carries its owner — a drain retries on every pass. Runs after
    /// finalization, so the wait never delays the run's terminal state.
    /// Whatever is still pending afterwards stays in the outbox. A settlement
    /// the owner refused is not the worker's to wait on: the owner answered,
    /// and its backoff belongs to the drain and the clock sweep.
    pub(crate) fn retry_own_claimed_leaf_settlement(&self, run_id: &str) {
        if self
            .worker_invocation()
            .is_none_or(|binding| binding.bound_run_id != run_id)
        {
            return;
        }
        for delay in LEAF_SETTLEMENT_RETRY_DELAYS {
            let undelivered = matches!(
                self.stores().jobs().local_pull_for_run(run_id),
                Ok(Some(record)) if record.phase == LocalPullPhase::Settling
                    && record.settlement_refusal.is_none()
            );
            if !undelivered || self.drain_carries_leaf(run_id) {
                return;
            }
            tracing::info!(
                target: "orbit.core.pull",
                run_id,
                delay_seconds = delay.as_secs(),
                "claimed leaf settlement not delivered; retrying",
            );
            std::thread::sleep(delay);
            self.deliver_claimed_leaf_settlement(run_id);
        }
    }

    /// Whether a live drain will carry this leaf's admission: one pulls from
    /// its owner. Unreadable drains count as none, so the worker retries.
    fn drain_carries_leaf(&self, run_id: &str) -> bool {
        let Ok(Some(record)) = self.stores().jobs().local_pull_for_run(run_id) else {
            return false;
        };
        self.live_drain_destinations()
            .is_some_and(|live| live.contains(&record.destination))
    }

    /// Record a forced release of a live claimed leaf's claim, before the
    /// leaf is stopped: its terminalization then finds the claim's settlement
    /// already decided, and the owner's task returns to the backlog rather
    /// than being failed for a stop the operator asked for. A settlement the
    /// leaf recorded first — its handoff — wins, and is returned instead.
    pub(crate) fn record_forced_leaf_release(
        &self,
        record: &LocalPullAdmission,
        why: &str,
    ) -> Result<LocalPullAdmission, OrbitError> {
        let jobs = self.stores().jobs();
        match jobs.mutate_local_pull(
            &record.destination,
            &record.request.request_id,
            &LocalPullMutation::Settle(Box::new(release_settlement(record, why))),
        ) {
            Ok(settling) => Ok(settling),
            Err(error) => {
                let current = match record.leaf_run_id.as_deref() {
                    Some(leaf) => jobs.local_pull_for_run(leaf)?,
                    None => None,
                };
                match current {
                    Some(current) if current.settlement.is_some() => Ok(current),
                    _ => Err(error),
                }
            }
        }
    }

    /// Reconcile the launched leaves `destination`'s admissions are waiting
    /// on whose worker died, so a drain pass records — and then delivers —
    /// their failure instead of waiting on a run nothing will finish. A leaf
    /// whose owner is alive, or cannot be judged, is left alone.
    pub(crate) fn reconcile_orphaned_claimed_leaves(&self, destination: &PullDestination) {
        // Before any admission read, so a cancel persisted on this call is
        // visible to the refill's post-reconciliation read.
        self.run_orphan_reconcile_hook();
        let jobs = self.stores().jobs();
        let records = match jobs.unsettled_local_pull_admissions() {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(target: "orbit.core.pull", %error, "pull admissions unreadable; no leaf reconciled");
                return;
            }
        };
        for record in records {
            if record.destination != *destination
                || !matches!(
                    record.phase,
                    LocalPullPhase::Launching | LocalPullPhase::Launched
                )
            {
                continue;
            }
            let Some(leaf) = record.leaf_run_id.as_deref() else {
                continue;
            };
            let run = match jobs.get_job_run(leaf) {
                Ok(Some(run)) if !run.state.is_terminal() => run,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(target: "orbit.core.pull", leaf, %error, "claimed leaf unreadable");
                    continue;
                }
            };
            if let Err(error) = self.reconcile_stale_job_run(&run) {
                tracing::warn!(target: "orbit.core.pull", leaf, %error, "could not reconcile a claimed leaf");
            }
        }
    }

    /// Deliver one leaf's recorded settlement. `None` when the run is not a
    /// claimed leaf or its admission no longer holds a slot.
    pub(crate) fn deliver_claimed_leaf_settlement(
        &self,
        run_id: &str,
    ) -> Option<PullSettlementEntry> {
        let record = match self.stores().jobs().local_pull_for_run(run_id) {
            Ok(Some(record)) if record.holds_capacity() => record,
            Ok(_) => return None,
            Err(error) => {
                tracing::warn!(target: "orbit.core.pull", run_id, %error, "claimed leaf admission unreadable");
                return None;
            }
        };
        self.carry_settlements_scoped(
            vec![record],
            DRAIN_ENDED_CAUSE,
            true,
            RefusedDelivery::WhenDue,
        )
        .pop()
    }

    /// One settle-only pass over every admission in this workspace that still
    /// holds a slot, for every owner.
    ///
    /// Delivers recorded settlements, records the failure of any terminal
    /// leaf, and — for an admission no live drain will carry (its own drain
    /// ended and no live drain pulls from its owner) — ends what was never
    /// launched ([`SettleScope::Abandon`]). It never
    /// requests new work and never launches a leaf, so it is safe beside a
    /// live drain. A workspace that never pulled reads nothing and does
    /// nothing.
    ///
    /// Only an operator runs this pass, so a settlement the owner refused is
    /// delivered again at once rather than when its backoff elapses.
    pub(crate) fn settle_pending_pulls(&self) -> Vec<PullSettlementEntry> {
        match self.stores().jobs().unsettled_local_pull_admissions() {
            Ok(records) => self.carry_settlements_for(records, DRAIN_ENDED_CAUSE),
            Err(error) => {
                tracing::warn!(target: "orbit.core.pull", %error, "pull admissions unreadable; nothing settled");
                Vec::new()
            }
        }
    }

    /// Admissions whose outcome is recorded locally but not delivered to the
    /// owner (`Settling`), with when the oldest was recorded.
    ///
    /// Read-only: it reads through the same schema-free path as
    /// [`Self::settle_pending_pulls`], so a workspace that never pulled
    /// reports nothing and keeps no pull tables. It contacts no owner.
    pub fn pending_pull_settlements(&self) -> Result<PendingPullSettlements, OrbitError> {
        let jobs = self.stores().jobs();
        let mut summary = PendingPullSettlements::default();
        for record in jobs.unsettled_local_pull_admissions()? {
            if record.phase != LocalPullPhase::Settling {
                continue;
            }
            summary.count += 1;
            let recorded_at = record
                .leaf_run_id
                .as_deref()
                .map(|leaf| jobs.get_job_run(leaf))
                .transpose()?
                .flatten()
                .and_then(|run| run.finished_at);
            summary.oldest_recorded_at = match (summary.oldest_recorded_at, recorded_at) {
                (Some(oldest), Some(recorded)) => Some(oldest.min(recorded)),
                (oldest, recorded) => oldest.or(recorded),
            };
        }
        Ok(summary)
    }

    /// The clock sweep's pass [ORB-13892], for every owner: deliver what is
    /// recorded and record what a leaf that ended implies, without ending
    /// anything unlaunched — only a drain, a cancel or a stop decides that.
    /// This is what retries a settlement once the leaf's worker and its
    /// drain are gone, such as a forced release the owner was down for, or
    /// the failure of a leaf whose worker died. A workspace that never
    /// pulled reads one empty table.
    pub fn deliver_recorded_pull_settlements(&self) -> Vec<PullSettlementEntry> {
        match self.stores().jobs().unsettled_local_pull_admissions() {
            Ok(records) => {
                self.carry_settlements_scoped(records, "", false, RefusedDelivery::WhenDue)
            }
            Err(error) => {
                tracing::warn!(target: "orbit.core.pull", %error, "pull admissions unreadable; nothing delivered");
                Vec::new()
            }
        }
    }

    /// An operator's settle-only pass over `records` whose releases carry
    /// `cause` — a forced cancel names itself to the owner. `records` should
    /// belong to a drain that is no longer live, or nothing unlaunched is
    /// released. A settlement the owner refused is delivered again at once.
    pub(crate) fn carry_settlements_for(
        &self,
        records: Vec<LocalPullAdmission>,
        cause: &str,
    ) -> Vec<PullSettlementEntry> {
        self.carry_settlements_scoped(records, cause, true, RefusedDelivery::Now)
    }

    /// [`Self::carry_settlements_for`]; with `may_abandon` false, nothing
    /// unlaunched is ever ended, whether or not a drain carries it, and
    /// `refused_delivery` says whether a refused settlement waits out its
    /// backoff.
    fn carry_settlements_scoped(
        &self,
        records: Vec<LocalPullAdmission>,
        cause: &str,
        may_abandon: bool,
        refused_delivery: RefusedDelivery,
    ) -> Vec<PullSettlementEntry> {
        if records.is_empty() {
            return Vec::new();
        }
        let Some(transport) = self.drain_owner_transport().cloned() else {
            return records
                .iter()
                .map(|record| {
                    entry(
                        record,
                        "no_owner_route",
                        Some(
                            "this runtime has no federated owner route; add the owner to \
                             ~/.orbit/mcp-destinations.toml"
                                .into(),
                        ),
                    )
                })
                .collect();
        };
        let peer = RoutedPullPeer { transport };
        let launcher = LeafPullLauncher { runtime: self };
        let drain = PullDrain {
            jobs: self.stores().jobs(),
            peer: &peer,
            launcher: &launcher,
            refused_delivery,
        };
        // One unreachable owner costs one delivery timeout per pass, not one
        // per admission. Only a transport failure says the owner is
        // unreachable: a local error (a store read, a missing binding, a failed
        // cancel) belongs to that one admission and the others still go out.
        let mut unreachable = BTreeSet::new();
        // Read again per admission, and again before anything is abandoned:
        // delivery can block, and a drain may start meanwhile.
        let drain_live = |record: &LocalPullAdmission| {
            self.live_drain_destinations()
                .as_ref()
                .is_none_or(|live| live.contains(&record.destination))
                || self.drain_run_live(&record.request.run_context.run_id)
        };
        let mut entries = Vec::with_capacity(records.len());
        for mut record in records {
            if unreachable.contains(&record.destination.selector) {
                entries.push(entry(&record, "owner_unreachable", None));
                continue;
            }
            let scope = if !may_abandon || drain_live(&record) {
                SettleScope::Deliver
            } else {
                SettleScope::Abandon
            };
            let error = drain
                .carry_settlement(&mut record, scope, &|record| !drain_live(record), cause)
                .err();
            if let Some(error) = &error {
                if is_owner_transport_failure(error) {
                    unreachable.insert(record.destination.selector.clone());
                }
                tracing::warn!(
                    target: "orbit.core.pull",
                    owner = %record.destination.selector,
                    request_id = %record.request.request_id,
                    leaf = record.leaf_run_id.as_deref().unwrap_or("-"),
                    %error,
                    "pull settlement did not complete; it stays recorded for the next pass",
                );
            }
            let carried = scope == SettleScope::Deliver || drain_live(&record);
            if error.is_none() && drain.release_held(&record).unwrap_or(false) {
                entries.push(entry(&record, "release_held", None));
                continue;
            }
            entries.push(classify(&record, carried, error));
        }
        entries
    }

    /// Reclaim the `target/` build output each settled claimed leaf left in
    /// its worktree, and return the bytes freed [ORB-13920].
    ///
    /// A claimed leaf's Cargo `target/` runs to gigabytes, and a follower's
    /// disk must not wait on an external GC schedule for it: once a claim is
    /// settled, the owner holds the leaf's delivery and the build output is
    /// only a cache. The drain calls this every pass rather than at the
    /// moment of settlement, because the leaf's own worker usually delivers
    /// its settlement while it is still alive, and target-only collection
    /// keeps a live worker's output; the next pass finds it exited. The
    /// checkout itself stays for worktree GC. Best-effort: a failure is
    /// logged and the next pass tries again.
    pub(crate) fn reclaim_settled_leaf_build_output(&self) -> u64 {
        let jobs = self.stores().jobs();
        let admissions = match jobs.local_pull_admissions() {
            Ok(admissions) => admissions,
            Err(error) => {
                tracing::warn!(target: "orbit.core.pull", %error, "pull admissions unreadable; no build output reclaimed");
                return 0;
            }
        };
        let repo_root = &self.paths().repo_root;
        // Cheap probes first, so a pass over a long settled history costs a
        // row read and a stat per leaf, and Git runs only where there is
        // something to reclaim.
        let leaves = admissions
            .into_iter()
            .filter(|record| record.phase == LocalPullPhase::Settled)
            .filter_map(|record| record.leaf_run_id)
            .filter(|leaf| {
                jobs.get_job_run(leaf).ok().flatten().is_some_and(|run| {
                    run.state.is_terminal() && run_worktree_has_build_output(repo_root, &run)
                })
            })
            .collect::<Vec<_>>();
        if leaves.is_empty() {
            return 0;
        }
        // Every run, so the collector still sees a path two runs share.
        let runs = match jobs.list_job_runs_filtered(&JobRunQuery {
            include_steps: false,
            ..JobRunQuery::default()
        }) {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!(target: "orbit.core.pull", %error, "job runs unreadable; no build output reclaimed");
                return 0;
            }
        };
        let mut reclaimed = 0u64;
        for leaf in leaves {
            match self.reclaim_run_build_output(&runs, &leaf) {
                Ok(result) => {
                    reclaimed = reclaimed.saturating_add(result.bytes_reclaimed);
                    for report in result.reports {
                        tracing::info!(
                            target: "orbit.core.pull",
                            leaf = %leaf,
                            path = %report.path.display(),
                            action = %report.action,
                            bytes = report.bytes_reclaimed,
                            "settled claimed leaf build output",
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "orbit.core.pull", leaf = %leaf, %error, "could not reclaim a settled leaf's build output; the next pass retries");
                }
            }
        }
        reclaimed
    }

    /// Destinations a live pull drain is carrying. A drain's refill carries
    /// every admission for its owner, whichever drain made it, so an
    /// admission there is left to that drain rather than abandoned — a pass
    /// must never cancel a queued leaf a live drain is about to launch.
    /// `None` when the drains cannot be read: then nothing is abandoned.
    fn live_drain_destinations(&self) -> Option<Vec<PullDestination>> {
        let runs = self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs(PULL_DRAIN_JOB)
            .ok()?;
        let mut destinations = Vec::with_capacity(runs.len());
        for run in runs {
            let destination = run
                .input
                .as_ref()
                .and_then(|input| input.get("destination"))
                .cloned()
                .map(serde_json::from_value::<PullDestination>);
            match destination {
                Some(Ok(destination)) => destinations.push(destination),
                // A live drain whose owner cannot be read could be carrying
                // any admission.
                _ => return None,
            }
        }
        Some(destinations)
    }

    /// Whether the drain that admitted a claim can still carry it forward.
    /// A run that is gone or unreadable counts as not live: nothing else will
    /// ever carry its unlaunched admissions.
    fn drain_run_live(&self, run_id: &str) -> bool {
        self.stores()
            .jobs()
            .get_job_run(run_id)
            .ok()
            .flatten()
            .is_some_and(|run| !run.state.is_terminal())
    }
}

fn classify(
    record: &LocalPullAdmission,
    drain_live: bool,
    error: Option<OrbitError>,
) -> PullSettlementEntry {
    let outcome = match record.phase {
        LocalPullPhase::Settled if record.refusal.is_some() => "closed_obsolete",
        LocalPullPhase::Settled => "settled",
        LocalPullPhase::Settling if error.is_none() && record.settlement_refusal.is_some() => {
            "settlement_refused"
        }
        LocalPullPhase::Settling => "pending_delivery",
        LocalPullPhase::Launched if error.is_none() => "leaf_running",
        LocalPullPhase::Launching => "launch_uncertain",
        LocalPullPhase::Requested if !drain_live && error.is_none() => "unanswered_request",
        LocalPullPhase::Idle => "idle",
        LocalPullPhase::Refused => "refused",
        _ if drain_live && error.is_none() => "awaiting_drain",
        _ => "pending",
    };
    let detail = error
        .map(|error| error.to_string())
        .or_else(|| match record.phase {
            LocalPullPhase::Settled => record.refusal.clone(),
            LocalPullPhase::Settling => record.settlement_refusal.as_ref().map(|refusal| {
                format!(
                    "{}; refused {} time(s), next automatic attempt after {}",
                    refusal.reason,
                    refusal.refusals,
                    refusal.retry_after.to_rfc3339()
                )
            }),
            _ => None,
        });
    entry(record, outcome, detail)
}

fn entry(
    record: &LocalPullAdmission,
    outcome: &str,
    detail: Option<String>,
) -> PullSettlementEntry {
    PullSettlementEntry {
        owner: record.destination.selector.clone(),
        drain_run_id: record.request.run_context.run_id.clone(),
        request_id: record.request.request_id.clone(),
        task_id: record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref())
            .map(|claim| claim.task_id.clone()),
        leaf_run_id: record.leaf_run_id.clone(),
        outcome: outcome.to_string(),
        detail,
    }
}
