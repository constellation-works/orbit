//! `pull_refill`: one iteration of a follower's pull drain [ORB-13625].
//!
//! Each call carries every earlier admission forward — retry, bind, launch,
//! settle — and, while the window is open and the owner would admit this
//! executor, tops the free slots up with new pull requests. Three consecutive
//! failed passes latch a durable degraded warning and stop new admissions.
//! Protocol skew fails immediately with its typed code and preserves the
//! durable settlement outbox. Other settlement retries keep flowing. If run state cannot be read or recorded,
//! the activity fails visibly rather than retrying without health evidence.
//!
//! Each request declares the crews this window can run [ORB-13941]: the
//! provider preflight taken on the window's first pass, minus every crew a
//! claimed leaf has since found unusable, and within the drain's
//! `--allow-crew` restriction when it has one [ORB-14174]. The owner skips a
//! task whose crew is not among them, so a follower never burns a claim it
//! cannot run. The owner's before-PR or before-landing reviewer is checked
//! against the window without that restriction. Each request also declares
//! this host's OS, and the owner skips a task whose `os:` tags that OS does
//! not satisfy.
//!
//! A drain submitted without a window (`for_seconds` zero) is authorized for
//! one admission pass [ORB-14174]. Its window is expired from the start, so
//! the pass is not read off the window: the first pass that finds no stop or
//! cancel takes it, recording it in run state before probing or requesting,
//! and requests up to the free slots. Every later pass — the same run's,
//! a retry's, or a resumed run's — finds it taken and only settles. A pass
//! held by a throttle, a shutdown or an owner refusal is still that pass.
//! A timed window that has expired never gains one.
//!
//! A stop or cancel recorded while a pass is requesting ends it before the
//! next request.
//!
//! The free slots are the store's allocation reading [ORB-15194]: a claimed
//! leaf in its before-landing review still holds its slot until it settles,
//! but admits a replacement beside it, up to `max_active_leaf_runs` such
//! leaves, so a one-to-two-hour review does not idle the slot.
//!
//! The drain outlives its window. `unsettled` counts admissions that still hold
//! a slot, and the job loop runs until the window has closed *and* that count
//! is zero, so a leaf that finishes after the window still has its handoff
//! delivered: by the leaf's own bound worker as it ends, by any later settle
//! pass, or by this drain's next iteration, whichever gets there first.
//!
//! Each pass reads the window from the run's own state rather than trusting
//! the answer the job forwarded, so an `orbit run extend` accepted after that
//! answer reopens requests [ORB-15417]. The pass that would end the drain
//! records the close in the transaction that rereads the deadline, the one an
//! extension checks: an extension accepted first keeps the drain admitting,
//! and one requested after the close is refused.
//!
//! A settlement the owner refuses while it still holds the claim holds new
//! requests too [ORB-13979]: the owner answered, and will answer the same
//! until an operator changes it. The refusal is recorded on the admission,
//! logged once and listed by `orbit run show`; delivery backs off (doubling to
//! at most 15 minutes) instead of asking on every pass, and is not a failed
//! pass. Requests resume on the pass after the owner accepts it.
//!
//! Sustained host resource pressure holds requests the same way a pending
//! host shutdown does [ORB-13901]: the pass requests nothing, keeps settling,
//! records the throttle on the drain's last pass and polls for recovery. The
//! pressure is this follower's own, because a claimed leaf runs here; the
//! owner's throttle gates only the owner's own drain and discovery, and its
//! pull admission never reads it [ORB-15637].
//!
//! Every other reason a pass requests nothing although its window is open —
//! an owner probe refusal, a refused settlement, the failure breaker, a
//! shutdown inside `machine.host_shutdown_hold_minutes`
//! (`host_shutdown_scheduled`), or no runnable crew — is recorded on the
//! drain's last pass as `admission_refusal`, so `orbit run show` and
//! readiness name it [ORB-15637]. A schedule further away is reported and
//! does not hold requests. Host pressure keeps its own field and leaves
//! `admission_refusal` unset.
//!
//! The failure breaker opens after three claims in a row settle as failures
//! and half-opens after `breaker_cooldown_seconds` to admit one probe claim
//! ([`super::drain::settle_breaker`]) [ORB-15639]. Each pass reads it with one
//! clock reading, records it in run state as `pull_breaker`, and asks it again
//! before every request, after reconciliation, so a leaf the pass just
//! settled as failed already counts.
//!
//! Under `machine.auto_concurrency` `enforce`, each request also carries the
//! work this host's load forecast has room for, computed under the host
//! admission lock just before it, and the owner admits only a task inside it.
//! When nothing fits the pass requests nothing and records
//! `forecast_no_headroom`. Pending request capacity is reserved in the fit's
//! transaction, refreshed before carried re-sends, and atomically exchanged
//! for precise task capacity before leaf creation. Shadow mode records the list as
//! `pull_admission_fit` and requests as with `off`; with `off`, or before the
//! host has resource profiles, requests carry no list. Every computed fit
//! records one accuracy forecast with the request sent/no-headroom decision
//! and a returned claim priced as its reservation.
//!
//! Each pass sends at most the drain's live ceiling of requests, an operator's
//! `orbit run concurrency` included, not the ceiling it was submitted with.
//!
//! A graceful `orbit run cancel` puts the drain into its cancelling mode:
//! each pass then requests nothing, releases every admission that never
//! launched back to the owner's backlog, and waits for the launched leaves to
//! finish and settle. The pass that finds nothing left unsettled ends the
//! drain `cancelled`. Cancellation is read again after orphaned leaves are
//! reconciled, so a request recorded during that work is observed before any
//! probe, pull request, or leaf launch in the same pass. If that state cannot
//! be read, the entire pass waits and fails before probing or advancing
//! pending requests: neither new work nor an earlier unanswered request may
//! be admitted without knowing whether the drain is cancelling.

mod cancel;
mod crews;
mod input;
mod owner;
mod record;

use std::cell::{Cell, RefCell};

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::{
    AdmissionRequest, AdmissionRunContext, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, PullDestination,
};
use serde_json::{Value, json};

use self::cancel::cancelling_pass;
use self::crews::crew_window;
use self::input::{bool_input, single_pass_input, u64_input};
use self::owner::{ProbeVerdict, captured_before_pr, probe};
use self::record::{
    PullPass, breaker_refusal, protocol_skew_failure, record_admission_fit, record_breaker,
    refused_settlements,
};
use super::drain::{
    BreakerGate, DEFAULT_BREAKER_COOLDOWN_SECONDS, PullDrain, RefusedDelivery, admits_request,
    breaker_gate, settle_breaker,
};
use crate::OrbitRuntime;
use crate::application::distributed::{
    OwnerAnswer, PullCrewWindow, PullWorkFit, owner_binary_version,
};
use crate::application::job::pull_window_closed;

/// The job that runs this action. Recorded in each request's run context, so
/// the owner's claim names the drain that holds it.
pub(crate) const PULL_DRAIN_JOB_NAME: &str = "workspace_pull_pipeline";

const DEFAULT_MAX_ACTIVE_LEAF_RUNS: u64 = 5;
const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;
const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

pub(crate) fn pull_refill(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let snapshot = runtime.admission_policy_snapshot();
    let runtime = &snapshot;
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failed("the executing drain run id was not supplied".into()))?
        .to_string();
    let destination: PullDestination = input
        .get("destination")
        .cloned()
        .ok_or_else(|| failed("`destination` is required".into()))
        .and_then(|value| {
            serde_json::from_value(value).map_err(|error| failed(format!("`destination`: {error}")))
        })?;
    let forwarded_expired = bool_input(input, "window_expired");
    let single_pass = single_pass_input(input);
    let ceiling = u64_input(input, "max_active_leaf_runs", DEFAULT_MAX_ACTIVE_LEAF_RUNS);
    let poll = u64_input(input, "poll_sleep_seconds", DEFAULT_POLL_SLEEP_SECONDS);
    let idle = u64_input(input, "idle_sleep_seconds", DEFAULT_IDLE_SLEEP_SECONDS);
    let cooldown = u64_input(
        input,
        "breaker_cooldown_seconds",
        DEFAULT_BREAKER_COOLDOWN_SECONDS,
    );
    let cooldown = i64::try_from(cooldown)
        .ok()
        .and_then(chrono::Duration::try_seconds)
        .unwrap_or(chrono::Duration::MAX);
    // One clock reading for the whole pass: the breaker it checks before each
    // request is the one it records.
    let now = chrono::Utc::now();

    let transport = runtime.drain_owner_transport().cloned().ok_or_else(|| {
        failed(format!(
            "no route to owner '{}': this runtime was composed without a federated owner \
                 transport, and a pull drain never falls back to its own store",
            destination.owner_machine_id
        ))
    })?;
    let peer = runtime.routed_pull_peer(transport.clone());
    let launcher = runtime.leaf_pull_launcher();
    let drain = PullDrain {
        jobs: runtime.stores().jobs(),
        peer: peer.as_ref(),
        launcher: launcher.as_ref(),
        refused_delivery: RefusedDelivery::WhenDue,
    };
    let read_pass_state = || {
        runtime.read_run_state(&run_id).map_err(|error| {
            failed(format!(
                "pull drain could not read cancellation and pass health: {error}"
            ))
        })
    };
    let state = read_pass_state()?;
    let degraded = state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
        .is_some_and(|pass| pass.degraded);
    if let Some(pass) = state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
        && pass.degraded
        && (pass.last_pass_error_code.as_deref() == Some("protocol_skew")
            || pass
                .last_pass_error
                .as_deref()
                .is_some_and(|message| message.starts_with("protocol_mismatch:")))
    {
        return Err(DispatchError::ProtocolSkew(
            pass.last_pass_error
                .clone()
                .unwrap_or_else(|| "pull request schema mismatch".into()),
        ));
    }
    // A launched leaf whose worker died is reconciled first, so this pass
    // records and delivers its failure rather than waiting on it. Cancellation
    // is read again afterwards: an operator can record it while reconciliation
    // runs, and the value from before that work must not admit new work.
    runtime.reconcile_orphaned_claimed_leaves(&destination);
    let pass_state = read_pass_state()?;
    if let Some(cancel) = pass_state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_cancel.as_ref())
    {
        return cancelling_pass(runtime, &drain, &run_id, &destination, cancel, poll)
            .map_err(|error| failed(error.to_string()));
    }
    // The window as run state has it now: an extension accepted since the
    // job's window answer reopens it [ORB-15417].
    let window_expired = pass_state.as_ref().map_or(forwarded_expired, |state| {
        pull_window_closed(state, forwarded_expired, chrono::Utc::now())
    });

    // A windowless drain's only admission pass is this one if nothing has
    // taken it yet; a timed window is open until it expires.
    let window_open = if single_pass {
        runtime.take_pull_single_pass(&run_id).map_err(|error| {
            failed(format!(
                "pull drain could not record its single admission pass: {error}"
            ))
        })?
    } else {
        !window_expired
    };
    // Stop admitting when a host shutdown is inside the hold horizon:
    // anything started now would be killed by it [ORB-12968]. A schedule
    // further away is named below and does not hold requests. Settlement
    // still runs either way.
    let host_shutdown = runtime.scheduled_host_shutdown();
    let host_shutdown_hold = runtime.host_shutdown_admission_hold();
    // Sustained host pressure holds new requests too [ORB-13901]; unknown
    // telemetry admits.
    runtime.reclaim_worktrees_on_admission();
    let resource = runtime.resource_admission();
    let mut admitted = 0;
    let mut refusal = None;
    let mut error: Option<String> = None;
    // What the owner said to the requests this pass sent [ORB-14475].
    let mut idle_receipt = None;
    // Whether the refill stopped on a full ceiling.
    let mut ceiling_full = false;
    // What this window can run: its preflight, minus every crew a leaf has
    // since found unusable [ORB-13941]. Read by the refill after it has
    // reconciled, so a leaf this pass settles already counts.
    let crews: RefCell<Option<PullCrewWindow>> = RefCell::new(None);
    // What the host load forecast had room for when this pass last built a
    // request: `None` until it builds one, `Some(None)` when nothing was
    // forecast (`off`, no profiles, an unreadable host).
    let work_fit: RefCell<Option<Option<PullWorkFit>>> = RefCell::new(None);
    // An open breaker skips the probe, as does a half-open one still waiting
    // for this drain's claims to settle; the refill asks it again before each
    // request, since settling a newly failed leaf can open it mid-pass. A
    // streak that cannot be read admits nothing: the drain reports it and
    // tries again next iteration.
    let previous_breaker = pass_state
        .as_ref()
        .and_then(|state| state.pull_drain.pull_breaker.clone());
    let mut streak = match drain.consecutive_failed_settlements(&destination, &run_id) {
        Ok(streak) => Some(streak),
        Err(failure) => {
            error = Some(failure.to_string());
            None
        }
    };
    let breaker_holds = match streak.as_ref().map(|streak| {
        breaker_gate(
            settle_breaker(previous_breaker.as_ref(), streak, now, cooldown).as_ref(),
            now,
        )
    }) {
        None | Some(BreakerGate::Closed) => false,
        Some(BreakerGate::Open) => true,
        Some(BreakerGate::HalfOpen) => match runtime.pull_drain_admissions(&run_id) {
            Ok(carried) => !carried.is_empty(),
            Err(failure) => {
                error.get_or_insert(failure.to_string());
                true
            }
        },
    };
    // A settlement the owner refused holds new requests until it is
    // accepted; this pass still delivers it once its backoff is due.
    let settlement_held = match refused_settlements(&drain, &destination) {
        Ok(refused) => !refused.is_empty(),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
            true
        }
    };
    let mut admitting = !degraded
        && window_open
        && host_shutdown_hold.is_none()
        && resource.throttle.is_none()
        && !breaker_holds
        && !settlement_held
        && streak.is_some();
    // Whether `refill` ran, and so already reconciled this pass.
    let mut refilled = false;
    if admitting && let Err(failure) = runtime.recover_pull_auth_exclusions(&run_id) {
        error.get_or_insert(failure.to_string());
        admitting = false;
    }
    if admitting {
        match probe(runtime, &run_id, &transport, &destination) {
            Ok(ProbeVerdict {
                ship: Some(ship),
                refusal: None,
            }) => {
                let forecasts = RefCell::new(Vec::new());
                let template = |request_id: &str| {
                    let caller_before_pr = captured_before_pr(runtime, &run_id)?;
                    let window = crew_window(runtime, &run_id)?;
                    let capability = (!window.runs_nothing()).then(|| window.capability());
                    *crews.borrow_mut() = Some(window);
                    let Some(capability) = capability else {
                        return Ok(None);
                    };
                    // Forecast afresh for every request, after the claim
                    // before it was reserved.
                    let fit = runtime.pull_work_fit(request_id)?;
                    let admissible = fit.as_ref().and_then(PullWorkFit::admissible);
                    let requests = fit.as_ref().is_none_or(PullWorkFit::requests);
                    if let Some(fit) = fit.as_ref() {
                        forecasts.borrow_mut().push((fit.clone(), false, None));
                    }
                    *work_fit.borrow_mut() = Some(fit);
                    if !requests {
                        return Ok(None);
                    }
                    Ok(Some(AdmissionRequest {
                        request_id: request_id.to_string(),
                        caller_version: owner_binary_version().to_string(),
                        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                        caller_fingerprint: Some(
                            orbit_store::contracts::distributed_drain_protocol_fingerprint()
                                .to_string(),
                        ),
                        caller_before_pr,
                        // The claimed PR leaf runs the before-PR gate the
                        // ship contract captures [ORB-13908].
                        review_gate: true,
                        run_context: AdmissionRunContext {
                            run_id: run_id.clone(),
                            job_name: PULL_DRAIN_JOB_NAME.to_string(),
                            machine_name: runtime
                                .automation_execution_location()
                                .and_then(|location| location.machine_name.clone()),
                        },
                        ship: ship.clone(),
                        crews: Some(capability),
                        os: runtime.host_os(),
                        // Read for each request, so a kit verified
                        // mid-window is declared from the next pull.
                        browser_kit: runtime.browser_kit_verified(),
                        admissible,
                    }))
                };
                // Capacity is exchanged before CreateLeaf by the peer's
                // reconciliation hooks; this hook records accuracy only.
                let requested =
                    |receipt: Option<&orbit_store::contracts::AdmissionReceipt>,
                     _requested_at: chrono::DateTime<chrono::Utc>| {
                        if let Some((_, sent, returned)) = forecasts.borrow_mut().last_mut() {
                            *sent = true;
                            *returned = receipt.cloned();
                        }
                        Ok(())
                    };
                // A stop or cancel recorded mid-pass ends it before the next
                // request; one that cannot be read ends it too. So does the
                // breaker: open, or half-open once this pass took its probe
                // or while this drain carries an unsettled claim.
                let probed = Cell::new(false);
                let still_admitting = || {
                    let stopped = runtime.read_run_state(&run_id)?.is_some_and(|state| {
                        state.pull_drain.drain_admissions_stop.is_some()
                            || state.pull_drain.drain_cancel.is_some()
                    });
                    if stopped {
                        return Ok(false);
                    }
                    let streak = drain.consecutive_failed_settlements(&destination, &run_id)?;
                    let gate = breaker_gate(
                        settle_breaker(previous_breaker.as_ref(), &streak, now, cooldown).as_ref(),
                        now,
                    );
                    let unsettled = if gate == BreakerGate::HalfOpen {
                        runtime.pull_drain_admissions(&run_id)?.len()
                    } else {
                        0
                    };
                    let admits = admits_request(gate, probed.get(), unsettled);
                    if admits && gate == BreakerGate::HalfOpen {
                        probed.set(true);
                    }
                    Ok(admits)
                };
                // The live ceiling: an operator's resize since submission
                // bounds this pass, not only each allocation.
                let ceiling =
                    pass_state.as_ref().map_or(ceiling, |state| {
                        u64::from(state.effective_max_active_leaf_runs(
                            u32::try_from(ceiling).unwrap_or(u32::MAX),
                        ))
                    });
                let ceiling = usize::try_from(ceiling).unwrap_or(usize::MAX);
                let pass = drain.refill_pass_requesting(
                    &destination,
                    &template,
                    &still_admitting,
                    Some(&requested),
                    ceiling,
                );
                for (fit, sent, receipt) in forecasts.into_inner() {
                    if let Err(error) =
                        fit.record_forecast(runtime, &run_id, sent, receipt.as_ref())
                    {
                        tracing::warn!(
                            target: "orbit.core.job_run",
                            %run_id, %error,
                            "pull drain could not record its admission forecast; the accuracy report will miss this request"
                        );
                    }
                }
                refilled = true;
                admitted = pass.admitted;
                idle_receipt = pass.answer;
                ceiling_full = pass.ceiling_full;
                if let Some(failure) = pass.error {
                    if let OrbitError::ProtocolSkew(message) = failure {
                        return Err(protocol_skew_failure(
                            runtime,
                            action,
                            &run_id,
                            resource.throttle.clone(),
                            message,
                        ));
                    }
                    error = Some(failure.to_string());
                }
            }
            Ok(verdict) => {
                refusal = Some(
                    verdict
                        .refusal
                        .unwrap_or_else(|| "owner probe returned no ship contract".into()),
                );
            }
            Err(OrbitError::ProtocolSkew(message)) => {
                return Err(protocol_skew_failure(
                    runtime,
                    action,
                    &run_id,
                    resource.throttle.clone(),
                    message,
                ));
            }
            Err(failure) => error = Some(failure.to_string()),
        }
    }
    // Refill reconciles first, so when it ran it has already tried every
    // pending admission, failed or not. Otherwise reconcile here so
    // settlements reach the owner even while it refuses new work.
    if !refilled && let Err(failure) = drain.reconcile_pending(&destination) {
        error.get_or_insert(failure.to_string());
    }
    // Settled leaves give back their build output here, not on an external
    // GC schedule: a follower's disk would otherwise grow by a leaf's
    // `target/` per claim [ORB-13920].
    let reclaimed_build_bytes = runtime.reclaim_settled_leaf_build_output();
    // Count after this pass's settlements, so a breaker that opened during it
    // is reported now rather than on the next poll.
    match drain.consecutive_failed_settlements(&destination, &run_id) {
        Ok(read) => streak = Some(read),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
        }
    }
    let consecutive_failures = streak.as_ref().map_or(0, |streak| streak.failures());
    // A streak that could not be read leaves the breaker as recorded.
    let breaker = match streak.as_ref() {
        Some(streak) => settle_breaker(previous_breaker.as_ref(), streak, now, cooldown),
        None => previous_breaker.clone(),
    };
    if breaker != previous_breaker
        && let Err(failure) = record_breaker(runtime, &run_id, breaker.as_ref())
    {
        error.get_or_insert(failure.to_string());
    }
    let gate = breaker_gate(breaker.as_ref(), now);
    // A half-open breaker that took no probe this pass names what it waits
    // for: the claims this drain carries that have not settled.
    let probe_waits_on = if gate == BreakerGate::HalfOpen && admitted == 0 {
        match runtime.pull_drain_admissions(&run_id) {
            Ok(carried) => carried.len(),
            Err(failure) => {
                error.get_or_insert(failure.to_string());
                0
            }
        }
    } else {
        0
    };
    admitting &= match gate {
        BreakerGate::Closed => true,
        BreakerGate::Open => false,
        BreakerGate::HalfOpen => admitted == 0 && probe_waits_on == 0,
    };
    // The window as this pass left it, read again when the refill did not
    // run or stopped before building a request.
    let crews = match crews.into_inner() {
        Some(window) => Some(window),
        None => match crew_window(runtime, &run_id) {
            Ok(window) => Some(window),
            Err(failure) => {
                error.get_or_insert(failure.to_string());
                None
            }
        },
    };
    let runs_nothing = crews.as_ref().is_some_and(PullCrewWindow::runs_nothing);
    admitting &= !runs_nothing;
    // The forecast this pass last requested against, recorded for `run
    // show`; a pass that built no request leaves the previous one.
    let work_fit = work_fit.into_inner();
    let admission_fit = work_fit
        .as_ref()
        .map(|fit| fit.as_ref().map(|fit| fit.record.clone()));
    if let Some(fit) = &admission_fit
        && let Err(failure) = record_admission_fit(runtime, &run_id, fit.as_ref())
    {
        error.get_or_insert(failure.to_string());
    }
    let no_headroom = admitted == 0
        && work_fit
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(|fit| !fit.requests());
    // As this pass left them: one it just delivered no longer holds requests.
    let refused = refused_settlements(&drain, &destination).unwrap_or_else(|failure| {
        error.get_or_insert(failure.to_string());
        Vec::new()
    });
    admitting &= refused.is_empty();
    // An unreadable count is not zero: the drain must not finish while it
    // cannot tell whether a settlement is still owed.
    let unsettled = match drain.unsettled(&destination) {
        Ok(count) => Some(count),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
            None
        }
    };
    let unsettled_holding = unsettled.is_none_or(|count| count > 0);
    // A windowless drain admits on no later pass, whether or not this one did.
    // A timed one ends only if its window is still closed when the close is
    // recorded, so an extension accepted during this pass keeps it running.
    let mut done = (single_pass || window_expired) && !unsettled_holding;
    if done && !single_pass {
        match runtime.close_pull_window(&run_id, forwarded_expired) {
            Ok(closed) => done = closed,
            Err(failure) => {
                error.get_or_insert(failure.to_string());
                done = false;
            }
        }
    }
    let sleep_seconds = if admitted > 0 {
        0
    } else if unsettled_holding || error.is_some() || resource.throttle.is_some() || no_headroom {
        poll
    } else {
        idle
    };
    if let Some(message) = error.as_deref() {
        tracing::warn!(
            target: "orbit.core.pull",
            owner = %destination.owner_machine_id,
            selector = %destination.selector,
            %message,
            "pull drain pass did not complete; retrying next iteration",
        );
    }
    let host_suppressed = crews
        .as_ref()
        .and_then(|window| window.host_suppressed.as_deref());
    let owner_busy_backoff = crews
        .as_ref()
        .and_then(|window| window.owner_busy_backoff.as_ref());
    let by_busy_owner = crews
        .as_ref()
        .is_some_and(|window| window.host_suppressed_by_busy_owner);
    let no_runnable_crew = if let Some(reason) = host_suppressed.filter(|_| by_busy_owner) {
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
    } else if crews.as_ref().is_some_and(PullCrewWindow::held_until_lift) {
        // [ORB-14697, ORB-14902, ORB-15791] A usage limit and a capacity
        // backoff lift by themselves, so this drain admits again once one does.
        "no_runnable_crew: no crew this drain may run is free on this host right now, and each \
         is held by a provider usage limit or a capacity backoff; see `crews.excluded`: each \
         exclusion with an `until` lifts then and this drain admits again"
            .to_string()
    } else if crews
        .as_ref()
        .is_some_and(|window| window.allowed.is_some())
    {
        "no_runnable_crew: no crew this drain's --allow-crew permits can run on this host for \
         this window; see `crews.allowed` and `crews.excluded`, then start a new drain with \
         crews that run here"
            .to_string()
    } else {
        "no_runnable_crew: every configured crew is excluded on this host for this window; see \
         `crews.excluded`, fix the providers, and start a new drain"
            .to_string()
    };
    let refusal = refusal
        .or_else(|| {
            (!refused.is_empty()).then(|| {
                format!(
                    "settlement_refused: the owner refused {} recorded settlement(s) while \
                     holding their claims, so no new claim is requested; `orbit run show \
                     {run_id}` names the reason and the remedy",
                    refused.len()
                )
            })
        })
        .or_else(|| breaker_refusal(breaker.as_ref(), gate, probe_waits_on, now))
        .or_else(|| {
            // Pressure already has `resource_throttle`. A shutdown hold would
            // otherwise look like an idle owner (`queued=0`, no refusal).
            resource.throttle.is_none().then(|| {
                host_shutdown_hold
                    .as_ref()
                    .map(crate::runtime::host_signal::ScheduledShutdown::hold_reason)
            })?
        })
        .or_else(|| runs_nothing.then_some(no_runnable_crew))
        .or_else(|| {
            no_headroom.then(|| {
                "forecast_no_headroom: this host's load forecast has room for no work the owner \
                 could grant under `machine.auto_concurrency` enforce; see the drain's \
                 `pull_admission_fit`, which names the minute and resource each kind of work \
                 would exceed. The drain requests again once its forecast has room"
                    .to_string()
            })
        });
    let owner_answer = match idle_receipt.as_deref() {
        Some(receipt) => OwnerAnswer::Idle(receipt),
        None if admitted > 0 => OwnerAnswer::Claimed,
        None => OwnerAnswer::None,
    };
    // A pass whose window is closed only settles, so it is not counted.
    let observation = window_open.then(|| {
        PullPass {
            admitted: admitted > 0,
            failed: error.is_some(),
            degraded,
            refusal: refusal.as_deref(),
            throttle: resource.throttle.as_ref(),
            ceiling_full,
            owner_idle: idle_receipt.is_some(),
        }
        .observe()
    });
    // [ORB-15637] The refusal is recorded on the pass, so `orbit run show`
    // and readiness name it: an open breaker otherwise looks like an idle
    // owner.
    let health = runtime
        .record_pull_pass(
            &run_id,
            resource.throttle.clone(),
            refusal.as_deref(),
            error.as_deref(),
            None,
            owner_answer,
            observation,
        )
        .map_err(|error| failed(format!("pull drain could not record pass health: {error}")))?;
    admitting &= !health.degraded;
    let refusal =
        refusal.or_else(|| {
            health.degraded.then(|| format!(
            "pass_failures: drain degraded after {} consecutive failed passes; fix the cause \
             run `orbit run auto --stop`, and start a new drain once this one ends; settlement retries continue",
            health.consecutive_pass_failures
        ))
        });
    Ok(json!({
        "admitted": admitted,
        "unsettled": unsettled,
        "admitting": admitting,
        "refusal": refusal,
        "settlement_refused": refused.len(),
        "crews": crews,
        "admission_fit": admission_fit.flatten(),
        "consecutive_failures": consecutive_failures,
        "breaker": breaker,
        "breaker_state": gate.as_str(),
        "reclaimed_build_bytes": reclaimed_build_bytes,
        "error": error,
        "last_pass_error": health.last_pass_error,
        "consecutive_pass_failures": health.consecutive_pass_failures,
        "degraded": health.degraded,
        "host_shutdown": host_shutdown.map(|shutdown| shutdown.describe()),
        "resource_throttle": resource.throttle,
        "resource_telemetry_unknown": resource.unknown,
        "cancelling": false,
        "done": done,
        "wait": !done && sleep_seconds > 0,
        "sleep_seconds": sleep_seconds,
    }))
}
