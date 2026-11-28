//! The settlement phase of a pass and its report: deliver what is owed, read
//! back the breaker, the window's crews, the host forecast and the owner's
//! refusals, then record the pass's health and answer the engine.

use orbit_engine::DispatchError;
use orbit_types::workflow::{DrainAdmissionPass, PullAdmissionFit, PullBreaker};
use serde_json::{Value, json};

use super::super::drain::{BreakerGate, ReconciliationHold, breaker_gate, settle_breaker};
use super::crews::crew_window;
use super::gate::{AdmissionGate, PassLedger, PassWindow};
use super::pass::{Pass, deterministic_failure};
use super::record::{PullPass, record_admission_fit, record_breaker, refused_settlements};
use super::refusal::{RefusalInputs, no_runnable_crew_refusal, pass_refusal};
use super::request::RequestTrail;
use crate::application::distributed::{OwnerAnswer, PullCrewWindow, PullPassRecord};
use crate::unit::pull_drain::settle::reclaim_settled_leaf_build_output;

/// What the settlement phase worked out, for the report.
pub(super) struct Settled {
    breaker: Option<PullBreaker>,
    breaker_gate: BreakerGate,
    consecutive_failures: usize,
    probe_waits_on: usize,
    reclaimed_build_bytes: u64,
    crews: Option<PullCrewWindow>,
    runs_nothing: bool,
    admission_fit: Option<PullAdmissionFit>,
    capacity_held: bool,
    no_headroom: bool,
    settlement_refused: usize,
    settlement_refused_holding: usize,
    /// `None` when the count could not be read.
    unsettled: Option<usize>,
    unsettled_holding: bool,
    done: bool,
}

/// The failure breaker as this pass leaves it.
struct BreakerOutcome {
    breaker: Option<PullBreaker>,
    gate: BreakerGate,
    consecutive_failures: usize,
    probe_waits_on: usize,
}

/// What the pass's last request forecast left to record.
struct FitOutcome {
    admission_fit: Option<PullAdmissionFit>,
    capacity_held: bool,
    no_headroom: bool,
}

/// The settlements this pass still owes the owner, or the owner refused.
struct OutboxOutcome {
    refused: usize,
    holding: usize,
    unsettled: Option<usize>,
}

impl Pass<'_, '_> {
    /// Deliver and count what the pass owes, in the order the report needs it.
    pub(super) fn settle(
        &self,
        gate: &AdmissionGate,
        window: &PassWindow,
        trail: &RequestTrail,
        ledger: &mut PassLedger,
    ) -> Settled {
        let reclaimed_build_bytes = self.deliver_and_reclaim(gate, ledger);
        let breaker = self.settle_failure_breaker(gate, ledger);
        let (crews, runs_nothing) = self.settle_crews(trail, ledger);
        let fit = self.settle_fit(trail, ledger);
        let outbox = self.settle_outbox(ledger);
        let unsettled_holding = outbox.unsettled.is_none_or(|count| count > 0);
        let done = self.close_window(window, unsettled_holding, ledger);
        Settled {
            breaker: breaker.breaker,
            breaker_gate: breaker.gate,
            consecutive_failures: breaker.consecutive_failures,
            probe_waits_on: breaker.probe_waits_on,
            reclaimed_build_bytes,
            crews,
            runs_nothing,
            admission_fit: fit.admission_fit,
            capacity_held: fit.capacity_held,
            no_headroom: fit.no_headroom,
            settlement_refused: outbox.refused,
            settlement_refused_holding: outbox.holding,
            unsettled: outbox.unsettled,
            unsettled_holding,
            done,
        }
    }

    /// Refill reconciles first, so when it ran it has already tried every
    /// pending admission, failed or not. Otherwise reconcile here so
    /// settlements reach the owner even while it refuses new work. Settled
    /// leaves then give back their build output here, not on an external GC
    /// schedule: a follower's disk would otherwise grow by a leaf's
    /// `target/` per claim [ORB-13920].
    fn deliver_and_reclaim(&self, gate: &AdmissionGate, ledger: &mut PassLedger) -> u64 {
        if !ledger.refilled
            && let Err(failure) = self.drain.reconcile_pending(&self.refill.destination)
        {
            ledger.note(failure);
        }
        reclaim_settled_leaf_build_output(self.runtime, &gate.settled_before_pass)
    }

    /// Count after this pass's settlements, so a breaker that opened during it
    /// is reported now rather than on the next poll, and record it when it
    /// changed.
    fn settle_failure_breaker(
        &self,
        gate: &AdmissionGate,
        ledger: &mut PassLedger,
    ) -> BreakerOutcome {
        let run_id = &self.refill.run_id;
        let read = match self
            .drain
            .consecutive_failed_settlements(&self.refill.destination, run_id)
        {
            Ok(read) => Some(read),
            Err(failure) => {
                ledger.note(failure);
                None
            }
        };
        // A streak that could not be read leaves the breaker as recorded.
        let streak = read.as_ref().or(gate.streak.as_ref());
        let previous = gate.previous_breaker.as_ref();
        let breaker = match streak {
            Some(streak) => settle_breaker(previous, streak, self.now, self.refill.cooldown),
            None => previous.cloned(),
        };
        if breaker.as_ref() != previous
            && let Err(failure) = record_breaker(self.runtime, run_id, breaker.as_ref())
        {
            ledger.note(failure);
        }
        let state = breaker_gate(breaker.as_ref(), self.now);
        // A half-open breaker that took no probe this pass names what it waits
        // for: the claims this drain carries that have not settled.
        let probe_waits_on = if state == BreakerGate::HalfOpen && ledger.admitted == 0 {
            match self.runtime.pull_drain_admissions(run_id) {
                Ok(carried) => carried.len(),
                Err(failure) => {
                    ledger.note(failure);
                    0
                }
            }
        } else {
            0
        };
        ledger.admitting &= match state {
            BreakerGate::Closed => true,
            BreakerGate::Open => false,
            BreakerGate::HalfOpen => ledger.admitted == 0 && probe_waits_on == 0,
        };
        BreakerOutcome {
            breaker,
            gate: state,
            consecutive_failures: streak.map_or(0, |streak| streak.failures()),
            probe_waits_on,
        }
    }

    /// The window as this pass left it, read again when the refill did not
    /// run or stopped before building a request.
    fn settle_crews(
        &self,
        trail: &RequestTrail,
        ledger: &mut PassLedger,
    ) -> (Option<PullCrewWindow>, bool) {
        let run_id = &self.refill.run_id;
        let crews = match trail.crews.take() {
            Some(window) => Some(window),
            None => match crew_window(self.runtime, run_id) {
                Ok(window) => Some(window),
                Err(failure) => {
                    ledger.note(failure);
                    None
                }
            },
        };
        if let Some(window) = crews.as_ref() {
            super::super::crew_health::record_pool_crew_health(
                self.runtime,
                run_id,
                self.input,
                None,
                &window.excluded,
            );
        }
        let runs_nothing = crews.as_ref().is_some_and(PullCrewWindow::runs_nothing);
        ledger.admitting &= !runs_nothing;
        (crews, runs_nothing)
    }

    /// The forecast this pass last requested against, recorded for `run
    /// show`; a pass that built no request leaves the previous one.
    fn settle_fit(&self, trail: &RequestTrail, ledger: &mut PassLedger) -> FitOutcome {
        let work_fit = trail.work_fit.take();
        let admission_fit = work_fit
            .as_ref()
            .map(|fit| fit.as_ref().map(|fit| fit.record.clone()));
        if let Some(fit) = &admission_fit
            && let Err(failure) =
                record_admission_fit(self.runtime, &self.refill.run_id, fit.as_ref())
        {
            ledger.note(failure);
        }
        let capacity_held =
            ledger.reconciliation_hold == Some(ReconciliationHold::ForecastCapacity);
        let no_headroom = ledger.admitted == 0
            && (capacity_held
                || work_fit
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|fit| !fit.requests()));
        FitOutcome {
            admission_fit: admission_fit.flatten(),
            capacity_held,
            no_headroom,
        }
    }

    /// The refused and unsettled settlements as this pass left them: one it
    /// just delivered no longer holds requests. An unreadable count is not
    /// zero: the drain must not finish while it cannot tell whether a
    /// settlement is still owed.
    fn settle_outbox(&self, ledger: &mut PassLedger) -> OutboxOutcome {
        let refused =
            refused_settlements(self.drain, &self.refill.destination).unwrap_or_else(|failure| {
                ledger.note(failure);
                Vec::new()
            });
        let holding = refused
            .iter()
            .filter(|refused| refused.holds_admission())
            .count();
        ledger.admitting &= holding == 0;
        let unsettled = match self.drain.unsettled(&self.refill.destination) {
            Ok(count) => Some(count),
            Err(failure) => {
                ledger.note(failure);
                None
            }
        };
        OutboxOutcome {
            refused: refused.len(),
            holding,
            unsettled,
        }
    }

    /// Whether the drain ends with this pass. A windowless drain admits on no
    /// later pass, whether or not this one did. A timed one ends only if its
    /// window is still closed when the close is recorded, so an extension
    /// accepted during this pass keeps it running.
    fn close_window(
        &self,
        window: &PassWindow,
        unsettled_holding: bool,
        ledger: &mut PassLedger,
    ) -> bool {
        let single_pass = self.refill.single_pass;
        let done = (single_pass || window.expired) && !unsettled_holding;
        if !done || single_pass {
            return done;
        }
        match self
            .runtime
            .close_pull_window(&self.refill.run_id, self.refill.forwarded_expired)
        {
            Ok(closed) => closed,
            Err(failure) => {
                ledger.note(failure);
                false
            }
        }
    }

    /// Why this pass requested no new claim, when it did not.
    fn refusal_reason(
        &self,
        gate: &AdmissionGate,
        settled: &Settled,
        ledger: &PassLedger,
    ) -> Option<String> {
        let no_runnable_crew = no_runnable_crew_refusal(settled.crews.as_ref());
        pass_refusal(&RefusalInputs {
            prior: ledger.refusal.as_deref(),
            run_id: &self.refill.run_id,
            holding_refused: settled.settlement_refused_holding,
            delivery_refused: settled.settlement_refused - settled.settlement_refused_holding,
            admitted: ledger.admitted,
            breaker: settled.breaker.as_ref(),
            gate: settled.breaker_gate,
            probe_waits_on: settled.probe_waits_on,
            now: self.now,
            throttled: gate.resource.throttle.is_some(),
            host_shutdown_hold: gate.host_shutdown_hold.as_ref(),
            runs_nothing: settled.runs_nothing,
            crew_refusal: &no_runnable_crew,
            no_headroom: settled.no_headroom,
            capacity_held: settled.capacity_held,
        })
    }

    /// Seconds the drain sleeps before its next pass.
    fn sleep_seconds(&self, gate: &AdmissionGate, settled: &Settled, ledger: &PassLedger) -> u64 {
        if ledger.admitted > 0 {
            0
        } else if settled.unsettled_holding
            || ledger.error.is_some()
            || gate.resource.throttle.is_some()
            || settled.no_headroom
        {
            self.refill.poll
        } else {
            self.refill.idle
        }
    }

    /// Record the pass's health and answer the engine with what it did.
    pub(super) fn report(
        &self,
        gate: &AdmissionGate,
        window: &PassWindow,
        trail: &RequestTrail,
        settled: &Settled,
        ledger: PassLedger,
        degraded: bool,
    ) -> Result<Value, DispatchError> {
        let sleep_seconds = self.sleep_seconds(gate, settled, &ledger);
        self.warn_incomplete(&ledger, settled.done);
        let refusal = self.refusal_reason(gate, settled, &ledger);
        let owner_answer = match ledger.idle_receipt.as_deref() {
            Some(receipt) => OwnerAnswer::Idle(receipt),
            None if ledger.admitted > 0 => OwnerAnswer::Claimed,
            None => OwnerAnswer::None,
        };
        // A pass whose window is closed only settles, so it is not counted.
        let observation = window.open.then(|| {
            PullPass {
                admitted: ledger.admitted > 0,
                failed: ledger.error.is_some(),
                degraded,
                refusal: refusal.as_deref(),
                throttle: gate.resource.throttle.as_ref(),
                ceiling_full: ledger.ceiling_full,
                owner_idle: ledger.idle_receipt.is_some(),
                admissions_stopped: trail.admissions_stopped.get(),
            }
            .observe()
        });
        // [ORB-15637] The refusal is recorded on the pass, so `orbit run show`
        // and readiness name it: an open breaker otherwise looks like an idle
        // owner.
        let health = self
            .runtime
            .record_pull_pass(
                &self.refill.run_id,
                PullPassRecord {
                    resource_throttle: gate.resource.throttle.clone(),
                    admission_refusal: refusal.as_deref(),
                    error: ledger.error.as_deref(),
                    error_code: None,
                    owner_answer,
                    observation,
                },
            )
            .map_err(|error| {
                deterministic_failure(
                    self.action,
                    format!("pull drain could not record pass health: {error}"),
                )
            })?;
        // A single-pass drain (including an owner repair-only pull) has no next
        // iteration to retry on: ending with zero admissions and a pass error
        // must fail the run rather than report success [ORB-29594].
        if self.refill.single_pass
            && window.open
            && settled.done
            && ledger.admitted == 0
            && let Some(error) = ledger.error.as_deref()
        {
            return Err(deterministic_failure(
                self.action,
                format!("single-pass pull drain failed without admitting work: {error}"),
            ));
        }
        let refusal = refusal.or_else(|| degraded_refusal(&health));
        Ok(settled.answer(gate, &ledger, &health, refusal, sleep_seconds))
    }

    fn warn_incomplete(&self, ledger: &PassLedger, done: bool) {
        if let Some(message) = ledger.error.as_deref() {
            let summary = if done && self.refill.single_pass && ledger.admitted == 0 {
                "single-pass pull drain failed without admitting work"
            } else {
                "pull drain pass did not complete; retrying next iteration"
            };
            tracing::warn!(
                target: "orbit.core.pull",
                owner = %self.refill.destination.owner_machine_id,
                selector = %self.refill.destination.selector,
                %message,
                "{summary}",
            );
        }
    }
}

impl Settled {
    /// What the engine reads of a finished pass.
    fn answer(
        &self,
        gate: &AdmissionGate,
        ledger: &PassLedger,
        health: &DrainAdmissionPass,
        refusal: Option<String>,
        sleep_seconds: u64,
    ) -> Value {
        json!({
            "admitted": ledger.admitted,
            "unsettled": self.unsettled,
            "admitting": ledger.admitting && !health.degraded,
            "refusal": refusal,
            "settlement_refused": self.settlement_refused,
            "settlement_refused_holding": self.settlement_refused_holding,
            "crews": self.crews,
            "admission_fit": self.admission_fit,
            "consecutive_failures": self.consecutive_failures,
            "breaker": self.breaker,
            "breaker_state": self.breaker_gate.as_str(),
            "reclaimed_build_bytes": self.reclaimed_build_bytes,
            "error": ledger.error,
            "last_pass_error": health.last_pass_error,
            "consecutive_pass_failures": health.consecutive_pass_failures,
            "degraded": health.degraded,
            "host_shutdown": gate.host_shutdown.as_ref().map(|shutdown| shutdown.describe()),
            "resource_throttle": gate.resource.throttle,
            "resource_telemetry_unknown": gate.resource.unknown,
            "cancelling": false,
            "done": self.done,
            "wait": !self.done && sleep_seconds > 0,
            "sleep_seconds": sleep_seconds,
        })
    }
}

/// The refusal a drain latched degraded reports when the pass named none.
fn degraded_refusal(health: &DrainAdmissionPass) -> Option<String> {
    health.degraded.then(|| {
        format!(
            "pass_failures: drain degraded after {} consecutive failed passes; fix the cause \
             run `orbit run auto --stop`, and start a new drain once this one ends; settlement retries continue",
            health.consecutive_pass_failures
        )
    })
}
