//! The request phase of a pass: probe the owner, then top the free slots up
//! with pull requests until the ceiling, a stop or the breaker ends it.

use std::cell::{Cell, RefCell};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contract::{
    AdmissionReceipt, AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
};
use orbit_types::workflow::PipelineState;

use super::crews::crew_window;
use super::gate::{AdmissionGate, PassLedger};
use super::owner::{ProbeVerdict, captured_before_pr, probe};
use super::pass::Pass;
use super::record::protocol_skew_failure;
use crate::application::contracts::PULL_DRAIN_JOB;
use crate::application::distributed::{PullCrewWindow, PullWorkFit, owner_binary_version};
use crate::unit::pull_drain::drain::{BreakerGate, RefillPass, admits_request};

/// Each request's forecast, the receipt the owner answered it with, and
/// whether it was sent.
type Forecasts = RefCell<Vec<(PullWorkFit, bool, Option<AdmissionReceipt>)>>;

/// What the requests of a pass left behind for the phases after it.
#[derive(Default)]
pub(super) struct RequestTrail {
    /// What this window can run: its preflight, minus every crew a leaf has
    /// since found unusable [ORB-13941]. Read by the refill after it has
    /// reconciled, so a leaf this pass settles already counts.
    pub(super) crews: RefCell<Option<PullCrewWindow>>,
    /// What the host load forecast had room for when this pass last built a
    /// request: `None` until it builds one, `Some(None)` when nothing was
    /// forecast (`off`, no profiles, an unreadable host).
    pub(super) work_fit: RefCell<Option<Option<PullWorkFit>>>,
    pub(super) admissions_stopped: Cell<bool>,
}

impl Pass<'_, '_> {
    /// Request new work when the gate admits, recording what the owner said.
    pub(super) fn request_work(
        &self,
        gate: &AdmissionGate,
        pass_state: Option<&PipelineState>,
        trail: &RequestTrail,
        ledger: &mut PassLedger,
    ) -> Result<(), DispatchError> {
        if !ledger.admitting {
            return Ok(());
        }
        match probe(
            self.runtime,
            &self.refill.run_id,
            self.transport,
            &self.refill.destination,
        ) {
            Ok(ProbeVerdict {
                ship: Some(ship),
                refusal: None,
            }) => self.request_up_to_ceiling(&ship, gate, pass_state, trail, ledger),
            Ok(verdict) => {
                ledger.refusal = Some(
                    verdict
                        .refusal
                        .unwrap_or_else(|| "owner probe returned no ship contract".into()),
                );
                Ok(())
            }
            Err(OrbitError::ProtocolSkew(message)) => Err(self.skew_failure(gate, message)),
            Err(failure) => {
                ledger.error = Some(failure.to_string());
                Ok(())
            }
        }
    }

    fn skew_failure(&self, gate: &AdmissionGate, message: String) -> DispatchError {
        protocol_skew_failure(
            self.runtime,
            self.action,
            &self.refill.run_id,
            gate.resource.throttle.clone(),
            message,
        )
    }

    fn request_up_to_ceiling(
        &self,
        ship: &AdmissionShipContract,
        gate: &AdmissionGate,
        pass_state: Option<&PipelineState>,
        trail: &RequestTrail,
        ledger: &mut PassLedger,
    ) -> Result<(), DispatchError> {
        let forecasts: Forecasts = RefCell::new(Vec::new());
        let template =
            |request_id: &str| self.admission_request(request_id, ship, trail, &forecasts);
        // Capacity is exchanged before CreateLeaf by the peer's
        // reconciliation hooks; this hook records accuracy only.
        let requested = |receipt: Option<&AdmissionReceipt>, _requested_at: DateTime<Utc>| {
            if let Some((_, sent, returned)) = forecasts.borrow_mut().last_mut() {
                *sent = true;
                *returned = receipt.cloned();
            }
            Ok(())
        };
        let probed = Cell::new(false);
        let still_admitting = || self.still_admitting(gate, trail, &probed);
        let pass = self.drain.refill_pass_requesting(
            &self.refill.destination,
            &template,
            &still_admitting,
            Some(&requested),
            self.live_ceiling(pass_state),
        );
        self.record_forecasts(forecasts.into_inner());
        self.take_refill_pass(pass, gate, ledger)
    }

    /// The live ceiling: an operator's resize since submission bounds this
    /// pass, not only each allocation.
    fn live_ceiling(&self, pass_state: Option<&PipelineState>) -> usize {
        let ceiling = self.refill.ceiling;
        let ceiling = pass_state.map_or(ceiling, |state| {
            u64::from(
                state.effective_max_active_leaf_runs(u32::try_from(ceiling).unwrap_or(u32::MAX)),
            )
        });
        usize::try_from(ceiling).unwrap_or(usize::MAX)
    }

    /// A stop or cancel recorded mid-pass ends it before the next request;
    /// one that cannot be read ends it too. So does the breaker: open, or
    /// half-open once this pass took its probe or while this drain carries an
    /// unsettled claim.
    fn still_admitting(
        &self,
        gate: &AdmissionGate,
        trail: &RequestTrail,
        probed: &Cell<bool>,
    ) -> Result<bool, OrbitError> {
        let run_id = &self.refill.run_id;
        let stopped = self.runtime.read_run_state(run_id)?.is_some_and(|state| {
            state.pull_drain.drain_admissions_stop.is_some()
                || state.pull_drain.drain_cancel.is_some()
        });
        if stopped {
            trail.admissions_stopped.set(true);
            return Ok(false);
        }
        let streak = self
            .drain
            .consecutive_failed_settlements(&self.refill.destination, run_id)?;
        let breaker = self.gate_for(gate.previous_breaker.as_ref(), &streak);
        let unsettled = if breaker == BreakerGate::HalfOpen {
            self.runtime.pull_drain_admissions(run_id)?.len()
        } else {
            0
        };
        let admits = admits_request(breaker, probed.get(), unsettled);
        if admits && breaker == BreakerGate::HalfOpen {
            probed.set(true);
        }
        Ok(admits)
    }

    /// The request the next claim carries, or `None` when this window can
    /// run nothing or the host has no room for it.
    fn admission_request(
        &self,
        request_id: &str,
        ship: &AdmissionShipContract,
        trail: &RequestTrail,
        forecasts: &Forecasts,
    ) -> Result<Option<AdmissionRequest>, OrbitError> {
        let run_id = &self.refill.run_id;
        let caller_before_pr = captured_before_pr(self.runtime, run_id)?;
        let window = crew_window(self.runtime, run_id)?;
        let capability = (!window.runs_nothing()).then(|| window.capability());
        *trail.crews.borrow_mut() = Some(window);
        let Some(capability) = capability else {
            return Ok(None);
        };
        // Forecast afresh for every request, after the claim
        // before it was reserved.
        let fit = self.runtime.pull_work_fit(request_id)?;
        let admissible = fit.as_ref().and_then(PullWorkFit::admissible);
        let requests = fit.as_ref().is_none_or(PullWorkFit::requests);
        if let Some(fit) = fit.as_ref() {
            forecasts.borrow_mut().push((fit.clone(), false, None));
        }
        *trail.work_fit.borrow_mut() = Some(fit);
        if !requests {
            return Ok(None);
        }
        Ok(Some(AdmissionRequest {
            request_id: request_id.to_string(),
            caller_version: owner_binary_version().to_string(),
            caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_fingerprint: Some(
                orbit_store::contract::distributed_drain_protocol_fingerprint().to_string(),
            ),
            caller_before_pr,
            // The claimed PR leaf runs the before-PR gate the
            // ship contract captures [ORB-13908].
            review_gate: true,
            selective_review: false,
            visual_resolution: 0,
            run_context: AdmissionRunContext {
                run_id: run_id.clone(),
                // The job that runs this action, so the owner's
                // claim names the drain that holds it.
                job_name: PULL_DRAIN_JOB.to_string(),
                machine_name: self
                    .runtime
                    .automation_execution_location()
                    .and_then(|location| location.machine_name.clone()),
            },
            ship: ship.clone(),
            crews: Some(capability),
            os: self.runtime.host_os(),
            // Read for each request, so a kit verified
            // mid-window is declared from the next pull.
            browser_kit: self.runtime.browser_kit_verified(),
            // The owner checks each candidate's
            // `requires_labels` against these plus `os`.
            labels: self.runtime.machine_labels().to_vec(),
            admissible,
        }))
    }

    fn record_forecasts(&self, forecasts: Vec<(PullWorkFit, bool, Option<AdmissionReceipt>)>) {
        for (fit, sent, receipt) in forecasts {
            if let Err(error) =
                fit.record_forecast(self.runtime, &self.refill.run_id, sent, receipt.as_ref())
            {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    run_id = %self.refill.run_id, %error,
                    "pull drain could not record its admission forecast; the accuracy report will miss this request"
                );
            }
        }
    }

    /// Take what the refill pass did into the ledger; a protocol skew ends
    /// the whole pass.
    fn take_refill_pass(
        &self,
        pass: RefillPass,
        gate: &AdmissionGate,
        ledger: &mut PassLedger,
    ) -> Result<(), DispatchError> {
        ledger.refilled = true;
        ledger.admitted = pass.admitted;
        ledger.idle_receipt = pass.answer;
        ledger.ceiling_full = pass.ceiling_full;
        ledger.reconciliation_hold = pass.hold;
        if let Some(failure) = pass.error {
            if let OrbitError::ProtocolSkew(message) = failure {
                return Err(self.skew_failure(gate, message));
            }
            ledger.error = Some(failure.to_string());
        }
        Ok(())
    }
}
