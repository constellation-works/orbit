use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_engine::DispatchError;
use orbit_types::workflow::PipelineState;
use serde_json::Value;

use super::super::drain::{PullDrain, PullTransportPort, RefusedDelivery};
use super::cancel::cancelling_pass;
use super::gate::PassLedger;
use super::input::RefillInput;
use super::request::RequestTrail;
use crate::OrbitRuntime;

/// What one pass works against, fixed when it starts: the runtime, the
/// drain's inputs, the owner it talks to and the one clock reading its
/// breaker checks and records.
pub(super) struct Pass<'a, 'd> {
    pub(super) runtime: &'a OrbitRuntime,
    pub(super) action: &'a str,
    pub(super) input: &'a Value,
    pub(super) refill: &'a RefillInput,
    pub(super) drain: &'a PullDrain<'d>,
    pub(super) transport: &'a Arc<dyn orbit_tools::DrainOwnerTransport>,
    pub(super) now: DateTime<Utc>,
}

impl Pass<'_, '_> {
    /// One pass, phase by phase: refuse on skew, honour a cancellation, open
    /// the window, gate admission, request, settle and report.
    fn run(&self) -> Result<Value, DispatchError> {
        let degraded = self.degraded_before_pass()?;
        // A launched leaf whose worker died is reconciled first, so this pass
        // records and delivers its failure rather than waiting on it. Cancellation
        // is read again afterwards: an operator can record it while reconciliation
        // runs, and the value from before that work must not admit new work.
        self.runtime
            .reconcile_orphaned_claimed_leaves(&self.refill.destination);
        let pass_state = self.read_state()?;
        if let Some(cancel) = pass_state
            .as_ref()
            .and_then(|state| state.pull_drain.drain_cancel.as_ref())
        {
            return cancelling_pass(
                self.runtime,
                self.drain,
                &self.refill.run_id,
                &self.refill.destination,
                cancel,
                self.refill.poll,
            )
            .map_err(|error| deterministic_failure(self.action, error.to_string()));
        }
        let window = self.window(pass_state.as_ref())?;
        let mut ledger = PassLedger::default();
        let trail = RequestTrail::default();
        let gate = self.admission_gate(&window, degraded, pass_state.as_ref(), &mut ledger);
        self.request_work(&gate, pass_state.as_ref(), &trail, &mut ledger)?;
        let settled = self.settle(&gate, &window, &trail, &mut ledger);
        self.report(&gate, &window, &trail, &settled, ledger, degraded)
    }

    pub(super) fn read_state(&self) -> Result<Option<PipelineState>, DispatchError> {
        self.runtime
            .read_run_state(&self.refill.run_id)
            .map_err(|error| {
                deterministic_failure(
                    self.action,
                    format!("pull drain could not read cancellation and pass health: {error}"),
                )
            })
    }

    /// Whether the drain's last pass latched degraded. A pass the drain's
    /// last protocol skew degraded fails again with the same typed cause.
    fn degraded_before_pass(&self) -> Result<bool, DispatchError> {
        let state = self.read_state()?;
        let Some(pass) = state
            .as_ref()
            .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
            .filter(|pass| pass.degraded)
        else {
            return Ok(false);
        };
        if pass.last_pass_error_code.as_deref() == Some("protocol_skew")
            || pass
                .last_pass_error
                .as_deref()
                .is_some_and(|message| message.starts_with("protocol_mismatch:"))
        {
            return Err(DispatchError::ProtocolSkew(
                pass.last_pass_error
                    .clone()
                    .unwrap_or_else(|| "pull request schema mismatch".into()),
            ));
        }
        Ok(true)
    }
}

pub(super) fn deterministic_failure(action: &str, message: String) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    }
}

pub(crate) fn pull_refill(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let snapshot = runtime.admission_policy_snapshot();
    let runtime = &snapshot;
    let refill = RefillInput::parse(action, input)?;
    // One clock reading for the whole pass: the breaker it checks before each
    // request is the one it records.
    let now = chrono::Utc::now();

    let transport = runtime.drain_owner_transport().cloned().ok_or_else(|| {
        deterministic_failure(
            action,
            format!(
                "no route to owner '{}': this runtime was composed without a federated owner \
                 transport, and a pull drain never falls back to its own store",
                refill.destination.owner_machine_id
            ),
        )
    })?;
    let peer = runtime.follower_pull_peer(transport.clone());
    let launcher = runtime.bound_leaf_launcher();
    let claimed_review = |run_id: &str| runtime.claimed_review_carry(run_id);
    let drain = PullDrain {
        jobs: runtime.stores().jobs(),
        peer: peer.as_ref(),
        launcher: launcher.as_ref(),
        refused_delivery: RefusedDelivery::WhenDue,
        claimed_review: &claimed_review,
    };
    Pass {
        runtime,
        action,
        input,
        refill: &refill,
        drain: &drain,
        transport: &transport,
        now,
    }
    .run()
}
