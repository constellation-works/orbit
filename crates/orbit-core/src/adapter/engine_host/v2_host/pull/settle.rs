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
//!   the bound worker then delivers its own settlement. The worker is an
//!   unsandboxed Orbit process with the host's federated owner route; only the
//!   agent subprocess inside a leaf runs under the sandbox, which is why the
//!   agent itself never talks to the owner (ORB-13642).
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

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_engine::run_worktree_has_build_output;
use orbit_store::contracts::{
    JobRunQuery, LocalPullAdmission, LocalPullMutation, LocalPullPhase, PullDestination,
};

use super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::drain::{PullDrain, SettleScope, leaf_failure_settlement};
use crate::OrbitRuntime;
use crate::application::distributed::{
    PULL_DRAIN_JOB, PendingPullSettlements, PullSettlementEntry, is_owner_transport_failure,
};

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
        let final_recovery = jobs
            .read_run_state(run_id)?
            .and_then(|state| state.final_recovery);
        let settlement =
            leaf_failure_settlement(record.phase, &run, diagnostic, final_recovery.as_ref());
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
        self.carry_settlements(vec![record]).pop()
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
    pub(crate) fn settle_pending_pulls(&self) -> Vec<PullSettlementEntry> {
        match self.stores().jobs().unsettled_local_pull_admissions() {
            Ok(records) => self.carry_settlements(records),
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

    fn carry_settlements(&self, records: Vec<LocalPullAdmission>) -> Vec<PullSettlementEntry> {
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
            let scope = if drain_live(&record) {
                SettleScope::Deliver
            } else {
                SettleScope::Abandon
            };
            let error = drain
                .carry_settlement(&mut record, scope, &|record| !drain_live(record))
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
        LocalPullPhase::Settling => "pending_delivery",
        LocalPullPhase::Launched if error.is_none() => "leaf_running",
        LocalPullPhase::Launching => "launch_uncertain",
        LocalPullPhase::Requested if !drain_live && error.is_none() => "unanswered_request",
        LocalPullPhase::Idle => "idle",
        LocalPullPhase::Refused => "refused",
        _ if drain_live && error.is_none() => "awaiting_drain",
        _ => "pending",
    };
    let detail = error.map(|error| error.to_string()).or_else(|| {
        (record.phase == LocalPullPhase::Settled)
            .then(|| record.refusal.clone())
            .flatten()
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
