//! Why a pull pass requested no new claim.
//!
//! The leading token of each string (`settlement_refused:`, `circuit_open:`,
//! `forecast_no_headroom:`) is the refusal code readiness and `refusal_code`
//! parse. The strings stay byte-identical with the pass that used to build
//! them inline, and nothing here reads or writes the store.

use chrono::{DateTime, Utc};
use orbit_types::workflow::PullBreaker;

use super::super::drain::BreakerGate;
use super::record::breaker_refusal;
use crate::application::distributed::PullCrewWindow;
use crate::runtime::host_signal::ScheduledShutdown;

/// Facts a pass has already computed before it chooses a refusal.
///
/// The order of [`pass_refusal`] is the precedence an operator sees: an
/// earlier `Some` wins, and a later fact is not formatted.
pub(super) struct RefusalInputs<'a> {
    /// Probe refusal already chosen, when the owner probe returned one.
    pub(super) prior: Option<&'a str>,
    pub(super) run_id: &'a str,
    /// Settlements the owner refused while still holding their claims.
    pub(super) holding_refused: usize,
    /// Delivery-evidence refusals, which do not hold new admissions.
    pub(super) delivery_refused: usize,
    pub(super) admitted: usize,
    pub(super) breaker: Option<&'a PullBreaker>,
    pub(super) gate: BreakerGate,
    pub(super) probe_waits_on: usize,
    pub(super) now: DateTime<Utc>,
    /// Host pressure is already recorded on `resource_throttle`.
    pub(super) throttled: bool,
    pub(super) host_shutdown_hold: Option<&'a ScheduledShutdown>,
    pub(super) runs_nothing: bool,
    /// [`no_runnable_crew_refusal`] for this pass's crew window.
    pub(super) crew_refusal: &'a str,
    pub(super) no_headroom: bool,
    /// A carried pull admission could not reacquire forecast capacity.
    pub(super) capacity_held: bool,
}

/// The message recorded when the window can run no crew.
///
/// `None` — the window could not be read — uses the same final branch as a
/// window whose every crew is excluded.
pub(super) fn no_runnable_crew_refusal(crews: Option<&PullCrewWindow>) -> String {
    let host_suppressed = crews.and_then(|window| window.host_suppressed.as_deref());
    let owner_busy_backoff = crews.and_then(|window| window.owner_busy_backoff.as_ref());
    let by_busy_owner = crews.is_some_and(|window| window.host_suppressed_by_busy_owner);
    if let Some(reason) = host_suppressed.filter(|_| by_busy_owner) {
        format!(
            "host_suppressed: this drain claims no more work on this host for its window \
             because {reason}. This host is not at fault: find what holds the owner's store \
             (the owner's `orbit.jsonl` names each advisory lock holder in its `still waiting \
             for advisory file lock` and `advisory file lock held past its threshold` \
             warnings, and its store errors name SQLite contention, such as `database is \
             locked` or `remained locked during`), then start a new drain"
        )
    } else if let Some(reason) = host_suppressed {
        format!(
            "host_suppressed: this drain claims no more work on this host for its window \
             because {reason}; see `crews.host_suppressed`, fix the host, and start a new drain"
        )
    } else if let Some(backoff) = owner_busy_backoff {
        format!(
            "owner_busy_backoff: a claimed leaf was released because the owner's store stayed \
             locked ({}); this drain requests no claim until {}, when requests resume by \
             themselves. This is busy-owner episode {} in this window; episode {} suppresses \
             this host for the window instead; see `crews.owner_busy_backoff`",
            backoff.reason,
            backoff.until.to_rfc3339(),
            backoff.episodes,
            crate::application::distributed::OWNER_BUSY_EPISODE_LIMIT,
        )
    } else if crews.is_some_and(PullCrewWindow::held_until_lift) {
        // [ORB-14697, ORB-14902, ORB-15791] A usage limit and a capacity
        // backoff lift by themselves, so this drain admits again once one does.
        "no_runnable_crew: no crew this drain may run is free on this host right now, and each \
         is held by a provider usage limit or a capacity backoff; see `crews.excluded`: each \
         exclusion with an `until` lifts then and this drain admits again"
            .to_string()
    } else if crews.is_some_and(|window| window.allowed.is_some()) {
        "no_runnable_crew: no crew this drain's --allow-crew permits can run on this host for \
         this window; see `crews.allowed` and `crews.excluded`, then start a new drain with \
         crews that run here"
            .to_string()
    } else {
        "no_runnable_crew: every configured crew is excluded on this host for this window; see \
         `crews.excluded`, fix the providers, and start a new drain"
            .to_string()
    }
}

/// The refusal this pass records, in precedence order, or `None` when the
/// pass has another recorded reason to request nothing (pressure, a full
/// ceiling, an idle owner).
pub(super) fn pass_refusal(inputs: &RefusalInputs<'_>) -> Option<String> {
    inputs
        .prior
        .map(str::to_string)
        .or_else(|| {
            (inputs.holding_refused > 0).then(|| {
                format!(
                    "settlement_refused: the owner refused {} recorded settlement(s) while \
                     holding their claims, so no new claim is requested; `orbit run show \
                     {run_id}` names the reason and the remedy",
                    inputs.holding_refused,
                    run_id = inputs.run_id,
                )
            })
        })
        .or_else(|| {
            (inputs.delivery_refused > 0 && inputs.admitted == 0).then(|| {
                format!(
                    "delivery_settlement_refused: the owner refused {delivery_refused} \
                     delivery-evidence settlement(s) while holding their claims; those \
                     settlements back off and close once the owner ends their claims, \
                     and other work is still admitted; `orbit run show {run_id}` names \
                     the reason and the remedy",
                    delivery_refused = inputs.delivery_refused,
                    run_id = inputs.run_id,
                )
            })
        })
        .or_else(|| {
            breaker_refusal(
                inputs.breaker,
                inputs.gate,
                inputs.probe_waits_on,
                inputs.now,
            )
        })
        .or_else(|| {
            // Pressure already has `resource_throttle`. A shutdown hold would
            // otherwise look like an idle owner (`queued=0`, no refusal).
            (!inputs.throttled).then(|| {
                inputs
                    .host_shutdown_hold
                    .map(ScheduledShutdown::hold_reason)
            })?
        })
        .or_else(|| inputs.runs_nothing.then(|| inputs.crew_refusal.to_string()))
        .or_else(|| {
            inputs.no_headroom.then(|| {
                if inputs.capacity_held {
                    return "forecast_no_headroom: a carried pull admission cannot reacquire \
                            this host's forecast capacity; it remains unsent or unlaunched \
                            until the requested work fits"
                        .to_string();
                }
                "forecast_no_headroom: this host's load forecast has room for no work the owner \
                 could grant under `machine.auto_concurrency` enforce; see the drain's \
                 `pull_admission_fit`, which names the minute and resource each kind of work \
                 would exceed. The drain requests again once its forecast has room"
                    .to_string()
            })
        })
}
