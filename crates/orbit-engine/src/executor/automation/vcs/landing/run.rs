//! Running one handoff landing: bounded transport passes and failure settlement.

use std::cell::Cell;
use std::ops::Deref;
use std::time::{Duration, Instant};

use orbit_common::{OrbitError, PrivateVcsFailureClass};
use orbit_types::workflow::handoff::HandoffDelivery;
use serde_json::Value;

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::ci::bounded_u64;
use super::super::super::input::input_string_field;
use super::super::remote_probe::fixed_wait as sleep;
use super::intent::{LandingView, reconcile_intent};
use super::local::{land_already_landed, land_local_candidate, land_no_diff};
use super::pull_request::land_pull_request;
use super::record::{record_stop, stop};

/// Budget for waiting out required checks on a pull request before the attempt
/// stops. The same shape `pr_complete` uses, so operators tune one thing.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;

/// Landing passes one run makes when a provider transport failure interrupts
/// a pass before any external write, including the first. Each later pass
/// waits out the matching backoff first.
const TRANSPORT_PASSES: u32 = 3;
const TRANSPORT_BACKOFF: [Duration; 2] = [Duration::from_secs(20), Duration::from_secs(60)];

pub(in crate::executor::automation) fn handoff_land<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let handoff_id = input_string_field(input, "handoff_id").ok_or_else(|| {
        OrbitError::InvalidInput("handoff_land: handoff_id is required".to_string())
    })?;
    // One budget for the whole run: a retried pass does not restart the wait.
    let budget = LandingBudget::from_input(input)?;
    let mut passes = 1;
    loop {
        let pass = LandingPass::new(host.handoff_landing_context(&handoff_id)?);
        let error = match land(host, &pass, input, &budget) {
            Ok(landed) => return Ok(landed),
            Err(error) => error,
        };
        if pass.settled.get() {
            return Err(error);
        }
        if passes < TRANSPORT_PASSES
            && !pass.wrote.get()
            && transport_failure(&error)
            && let Some(backoff) = TRANSPORT_BACKOFF.get((passes - 1) as usize).copied()
            && budget.allows(backoff)?
        {
            tracing::warn!(
                %error,
                handoff_id,
                pass = passes,
                "handoff_land: provider transport failure before any external write; retrying \
                 the same handoff"
            );
            sleep(backoff);
            passes += 1;
            continue;
        }
        return Err(settle_failure(host, &pass, passes, error));
    }
}

/// A provider failure the provider boundary classified as a transport
/// failure: no credential, permission, rate-limit or permanent-HTTP signal.
pub(super) fn transport_failure(error: &OrbitError) -> bool {
    error
        .private_vcs_failure()
        .is_some_and(|failure| failure.class == PrivateVcsFailureClass::TransientObservation)
}

/// Record the stop a failed run left unrecorded, and return the error the run
/// fails with. An intent this owner published and could not resolve is left
/// as it is: the store refuses a stop past it, and the next attempt
/// reconciles it against the provider before anything else.
fn settle_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    pass: &LandingPass,
    passes: u32,
    error: OrbitError,
) -> OrbitError {
    if pass.intent_open.get() {
        return error;
    }
    let message = error.to_string();
    let message = message.strip_prefix("handoff_land: ").unwrap_or(&message);
    let tries = if passes > 1 {
        format!(" after {passes} tries")
    } else {
        String::new()
    };
    let reason = format!("the landing run failed{tries}: {message}; the candidate stays in review");
    match record_stop(host, pass, &reason, false) {
        Ok(stopped) => stopped,
        Err(refused) => {
            tracing::warn!(
                %refused,
                %error,
                handoff_id = %pass.handoff_id,
                "handoff_land: the failed run's stop could not be recorded"
            );
            error
        }
    }
}

/// The check-wait budget of one landing run, shared by every pass it makes.
pub(super) struct LandingBudget {
    pub(super) max_wait_seconds: u64,
    pub(super) poll_interval_seconds: u64,
    pub(super) started: Instant,
    pub(super) deadline_ms: u64,
}

impl LandingBudget {
    fn from_input(input: &Value) -> Result<Self, OrbitError> {
        let max_wait_seconds = bounded_u64(
            input,
            "max_wait_seconds",
            DEFAULT_MAX_WAIT_SECONDS,
            MAX_WAIT_SECONDS,
        )?;
        let poll_interval_seconds = bounded_u64(
            input,
            "poll_interval_seconds",
            DEFAULT_POLL_INTERVAL_SECONDS,
            MAX_POLL_INTERVAL_SECONDS,
        )?
        .max(MIN_POLL_INTERVAL_SECONDS);
        let deadline_ms =
            super::super::completion_deadline::now_ms()?.saturating_add(if max_wait_seconds == 0 {
                // Zero disables poll waits; synchronous provider operations still
                // receive a finite recovery deadline.
                60_000
            } else {
                max_wait_seconds.saturating_mul(1000)
            });
        Ok(Self {
            max_wait_seconds,
            poll_interval_seconds,
            started: Instant::now(),
            deadline_ms,
        })
    }

    /// Whether a retry that first waits `backoff` still starts within budget.
    fn allows(&self, backoff: Duration) -> Result<bool, OrbitError> {
        let backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
        Ok(
            super::super::completion_deadline::now_ms()?.saturating_add(backoff_ms)
                < self.deadline_ms,
        )
    }
}

/// One pass over the owner's current landing state, and what it recorded.
///
/// Every durable landing step goes through [`record`](super::record::record),
/// which keeps these facts, so a failed run knows whether it still owes its
/// attempt a stop.
pub(super) struct LandingPass {
    context: HandoffLandingContext,
    /// A stop or completion is recorded: the attempt is settled.
    pub(super) settled: Cell<bool>,
    /// A merge intent was recorded or a check recovery requested, so an
    /// external write may have happened and the pass is not retried.
    pub(super) wrote: Cell<bool>,
    /// A merge intent stands unresolved.
    pub(super) intent_open: Cell<bool>,
}

impl LandingPass {
    fn new(context: HandoffLandingContext) -> Self {
        let intent_open = context.unresolved_merge_intent.is_some();
        Self {
            context,
            settled: Cell::new(false),
            wrote: Cell::new(false),
            intent_open: Cell::new(intent_open),
        }
    }
}

impl Deref for LandingPass {
    type Target = HandoffLandingContext;

    fn deref(&self) -> &HandoffLandingContext {
        &self.context
    }
}

fn land<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    input: &Value,
    budget: &LandingBudget,
) -> Result<Value, OrbitError> {
    // Every containment judgement below reads the landing ref, so the owner's
    // view of it is refreshed once before any of them run.
    let view = LandingView::refresh(
        &context.workspace_path,
        &context.candidate.landing_branch,
        &context.candidate.delivery,
    );

    // An intent recorded but never resolved is this owner's own uncertainty
    // about an external write. Nothing else may happen until real state says
    // what became of it.
    if let Some(intent_id) = context.unresolved_merge_intent.clone()
        && let Some(landed) = reconcile_intent(host, context, &intent_id, &view)?
    {
        return Ok(landed);
    }

    match context.candidate.delivery.clone() {
        HandoffDelivery::PullRequest { number } => {
            land_pull_request(host, context, number, budget, &view)
        }
        // [ORB-15677] Only a pull request's hosted CI runs the checks the
        // review owed it; a local candidate has none to wait for.
        HandoffDelivery::LocalCandidate if !context.owed_hosted_checks.is_empty() => Err(stop(
            host,
            context,
            &format!(
                "the review owed hosted checks ({}) to a pull request, but the candidate is \
                 delivered locally; the candidate stays in review",
                context.owed_hosted_checks.join(", ")
            ),
        )?),
        HandoffDelivery::LocalCandidate => land_local_candidate(host, context, input, &view),
        HandoffDelivery::NoDiff { .. } => land_no_diff(host, context, &view),
        HandoffDelivery::AlreadyLanded {
            covering_commit, ..
        } => land_already_landed(host, context, &covering_commit, &view),
    }
}
