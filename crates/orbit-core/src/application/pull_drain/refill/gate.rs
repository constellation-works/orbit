//! What decides whether a pass may request new work: the drain's window, the
//! host's shutdown and load, the failure breaker and the settlements the
//! owner refused.

use std::fmt::Display;

use orbit_store::contract::{AdmissionReceipt, PullFailureStreak};
use orbit_types::workflow::{PipelineState, PullBreaker};

use super::super::drain::{BreakerGate, ReconciliationHold, breaker_gate, settle_breaker};
use super::pass::{Pass, deterministic_failure};
use super::record::refused_settlements;
use crate::application::job::pull_window_closed;
use crate::runtime::host_resource::ResourceAdmission;
use crate::runtime::host_signal::ScheduledShutdown;
use orbit_engine::DispatchError;

/// What a pass has found out so far, written by each phase and read by the
/// ones after it. `error` keeps the first failure a phase could not act on:
/// the pass reports it and the drain tries again next iteration.
#[derive(Default)]
pub(super) struct PassLedger {
    pub(super) error: Option<String>,
    pub(super) admitting: bool,
    pub(super) admitted: usize,
    pub(super) refusal: Option<String>,
    /// What the owner said to the requests this pass sent [ORB-14475].
    pub(super) idle_receipt: Option<Box<AdmissionReceipt>>,
    /// Whether the refill stopped on a full ceiling.
    pub(super) ceiling_full: bool,
    pub(super) reconciliation_hold: Option<ReconciliationHold>,
    /// Whether `refill` ran, and so already reconciled this pass.
    pub(super) refilled: bool,
}

impl PassLedger {
    /// Keep `failure` as the pass's error unless an earlier one is kept.
    pub(super) fn note(&mut self, failure: impl Display) {
        self.error.get_or_insert(failure.to_string());
    }
}

/// Whether the drain's window lets this pass request.
pub(super) struct PassWindow {
    pub(super) expired: bool,
    pub(super) open: bool,
}

/// What admission was judged against before the pass requested anything.
pub(super) struct AdmissionGate {
    pub(super) host_shutdown: Option<ScheduledShutdown>,
    pub(super) host_shutdown_hold: Option<ScheduledShutdown>,
    pub(super) resource: ResourceAdmission,
    pub(super) previous_breaker: Option<PullBreaker>,
    /// `None` when the streak could not be read.
    pub(super) streak: Option<PullFailureStreak>,
    /// Leaves already settled before this pass reconciles. Reclaim marks only
    /// these when they have nothing to give back: a leaf settled in this pass
    /// can still grow a checkout afterwards, and the next pass reclaims it.
    pub(super) settled_before_pass: Vec<String>,
}

impl Pass<'_, '_> {
    /// The window as run state has it now: an extension accepted since the
    /// job's window answer reopens it [ORB-15417].
    pub(super) fn window(
        &self,
        pass_state: Option<&PipelineState>,
    ) -> Result<PassWindow, DispatchError> {
        let forwarded_expired = self.refill.forwarded_expired;
        let expired = pass_state.map_or(forwarded_expired, |state| {
            pull_window_closed(state, forwarded_expired, chrono::Utc::now())
        });
        // A windowless drain's only admission pass is this one if nothing has
        // taken it yet; a timed window is open until it expires.
        let open = if self.refill.single_pass {
            self.runtime
                .take_pull_single_pass(&self.refill.run_id)
                .map_err(|error| {
                    deterministic_failure(
                        self.action,
                        format!("pull drain could not record its single admission pass: {error}"),
                    )
                })?
        } else {
            !expired
        };
        Ok(PassWindow { expired, open })
    }

    /// Judge whether the pass may request, and set `ledger.admitting`.
    pub(super) fn admission_gate(
        &self,
        window: &PassWindow,
        degraded: bool,
        pass_state: Option<&PipelineState>,
        ledger: &mut PassLedger,
    ) -> AdmissionGate {
        let run_id = &self.refill.run_id;
        // Stop admitting when a host shutdown is inside the hold horizon:
        // anything started now would be killed by it [ORB-12968]. A schedule
        // further away is named below and does not hold requests. Settlement
        // still runs either way.
        let host_shutdown = self.runtime.scheduled_host_shutdown();
        let host_shutdown_hold = self.runtime.host_shutdown_admission_hold();
        // Sustained host pressure holds new requests too [ORB-13901]; unknown
        // telemetry admits.
        self.runtime.reclaim_worktrees_on_admission();
        let resource = self.runtime.resource_admission();
        // An open breaker skips the probe, as does a half-open one still waiting
        // for this drain's claims to settle; the refill asks it again before each
        // request, since settling a newly failed leaf can open it mid-pass. A
        // streak that cannot be read admits nothing: the drain reports it and
        // tries again next iteration.
        let previous_breaker = pass_state.and_then(|state| state.pull_drain.pull_breaker.clone());
        let streak = match self
            .drain
            .consecutive_failed_settlements(&self.refill.destination, run_id)
        {
            Ok(streak) => Some(streak),
            Err(failure) => {
                ledger.note(failure);
                None
            }
        };
        let breaker_holds = self.breaker_holds(previous_breaker.as_ref(), streak.as_ref(), ledger);
        let settlement_held = self.settlement_held(ledger);
        ledger.admitting = !degraded
            && window.open
            && host_shutdown_hold.is_none()
            && resource.throttle.is_none()
            && !breaker_holds
            && !settlement_held
            && streak.is_some();
        let settled_before_pass = self.settled_before_pass();
        if ledger.admitting
            && let Err(failure) = self.runtime.recover_pull_auth_exclusions(run_id)
        {
            ledger.note(failure);
            ledger.admitting = false;
        }
        AdmissionGate {
            host_shutdown,
            host_shutdown_hold,
            resource,
            previous_breaker,
            streak,
            settled_before_pass,
        }
    }

    /// The breaker's gate for `streak` at this pass's clock reading.
    pub(super) fn gate_for(
        &self,
        previous: Option<&PullBreaker>,
        streak: &PullFailureStreak,
    ) -> BreakerGate {
        breaker_gate(
            settle_breaker(previous, streak, self.now, self.refill.cooldown).as_ref(),
            self.now,
        )
    }

    fn breaker_holds(
        &self,
        previous: Option<&PullBreaker>,
        streak: Option<&PullFailureStreak>,
        ledger: &mut PassLedger,
    ) -> bool {
        match streak.map(|streak| self.gate_for(previous, streak)) {
            None | Some(BreakerGate::Closed) => false,
            Some(BreakerGate::Open) => true,
            Some(BreakerGate::HalfOpen) => {
                match self.runtime.pull_drain_admissions(&self.refill.run_id) {
                    Ok(carried) => !carried.is_empty(),
                    Err(failure) => {
                        ledger.note(failure);
                        true
                    }
                }
            }
        }
    }

    /// A settlement the owner refused holds new requests until it is
    /// accepted, except a delivery-evidence refusal, which holds only that
    /// settlement; this pass still delivers refused settlements once due.
    fn settlement_held(&self, ledger: &mut PassLedger) -> bool {
        match refused_settlements(self.drain, &self.refill.destination) {
            Ok(refused) => refused.iter().any(|refused| refused.holds_admission()),
            Err(failure) => {
                ledger.note(failure);
                true
            }
        }
    }

    fn settled_before_pass(&self) -> Vec<String> {
        self.runtime
            .stores()
            .jobs()
            .settled_leaf_run_ids_pending_build_reclaim()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    target: "orbit.core.pull",
                    %error,
                    "could not list settled leaves before reconciliation; this pass will not record them clear"
                );
                Vec::new()
            })
    }
}
