use super::guard::active_guard;
use super::{request, store, *};
use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle};
use orbit_exec::{ExecRequest, NoSandbox, run_process};
use orbit_types::tool::ExecutionResult;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub(super) fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The instant `request`'s finite timeout budget runs out, counted from
/// `started`; `None` for a request without one. Every wait the guard makes for
/// the request (record locks, the rate-limit probe, the child itself) is cut
/// off at this one instant. A caller that runs the child itself takes the
/// instant before admission and hands it to [`observe_gh_result_by_deadline`]
/// or [`observe_gh_failure_by_deadline`], so the post-child accounting stays
/// inside the same budget.
pub fn gh_request_deadline(request: &ExecRequest, started: Instant) -> Option<Instant> {
    request
        .timeout_ms
        .and_then(|budget_ms| started.checked_add(Duration::from_millis(budget_ms)))
}

/// Whole milliseconds left before `deadline`; 0 once it has passed.
pub(super) fn remaining_millis(deadline: Instant) -> u64 {
    u64::try_from(
        deadline
            .saturating_duration_since(store::monotonic_now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// Admit `request` for `caller`, or refuse it locally with a typed throttle.
/// A request that spends no API quota, and every request while the guard is
/// off, is admitted. Waits for the shared record no longer than `request`'s
/// own timeout budget. A request admitted once that budget is spent is not
/// charged: the caller returns its typed budget error without spawning `gh`.
///
/// The outcome agrees with the host record: a refusal decided with budget left
/// is counted and returned as the typed throttle, and a caller returns it as
/// is, whatever its own clock reads afterwards. A refusal decided once the
/// budget is spent is not counted and comes back as the typed
/// [`OrbitError::ProcessTimeout`], the same budget error a caller returns for
/// a request admitted too late; a caller maps it to its own timeout outcome.
pub fn admit_gh_request(caller: GithubCaller, request: &ExecRequest) -> Result<(), OrbitError> {
    let deadline = gh_request_deadline(request, store::monotonic_now());
    admit_by_deadline(caller, request, deadline).map_err(|decided| {
        if deadline.is_some_and(store::budget_spent) {
            return OrbitError::ProcessTimeout {
                timeout_ms: request.timeout_ms.unwrap_or_default(),
                detail: "the request's timeout budget ran out before the process could start"
                    .to_string(),
            };
        }
        decided.deliver(request, true)
    })
}

/// [`admit_gh_request`] for a caller that spawns the child itself: returns
/// `request` with only the timeout budget left after admission (lock waits and
/// the rate-limit probe included), so the child cannot outlive the original
/// budget. A finite budget exhausted before the child could start is the typed
/// [`OrbitError::ProcessTimeout`], and the caller must not spawn. A request
/// without a finite budget comes back unchanged.
pub fn admit_gh_request_within_budget(
    caller: GithubCaller,
    request: &ExecRequest,
) -> Result<ExecRequest, OrbitError> {
    let exhausted = || OrbitError::ProcessTimeout {
        timeout_ms: request.timeout_ms.unwrap_or_default(),
        detail: "the request's timeout budget ran out before the process could start".to_string(),
    };
    if request.timeout_ms == Some(0) {
        return Err(exhausted());
    }
    let deadline = gh_request_deadline(request, store::monotonic_now());
    let admitted = admit_by_deadline(caller, request, deadline);
    // As in `run_gh_request`, a spent budget wins over the refusal it caused,
    // and a refusal is counted only once it is certain to be delivered.
    let bounded = remaining_request_budget(request, deadline).ok_or_else(exhausted)?;
    admitted.map_err(|decided| decided.deliver(request, true))?;
    Ok(bounded)
}

/// A local refusal decided under the record lock and not yet counted: counting
/// waits until the caller knows its budget survived to deliver the typed
/// throttle, so a refusal that becomes a timeout leaves the count unchanged.
struct DecidedRefusal {
    refusal: QuotaRefusal,
    store: GithubQuotaStore,
    account: String,
    now: u64,
}

impl DecidedRefusal {
    /// The typed throttle for `request`, counting the refusal first when
    /// `counted`.
    fn deliver(self, request: &ExecRequest, counted: bool) -> OrbitError {
        if counted {
            self.store.record_refusal(
                &self.account,
                self.refusal.caller,
                &self.refusal.resource,
                self.now,
            );
        }
        refusal_error(&self.refusal, request)
    }
}

/// Admit `request` against `deadline`. A read admitted once the deadline has
/// passed is not charged: every caller checks its own budget after admission
/// and returns instead of spawning `gh`, so no protected call follows it. A
/// refusal comes back uncounted for the caller to [`DecidedRefusal::deliver`].
fn admit_by_deadline(
    caller: GithubCaller,
    request: &ExecRequest,
    deadline: Option<Instant>,
) -> Result<(), Box<DecidedRefusal>> {
    let Some((store, policy)) = active_guard() else {
        return Ok(());
    };
    let Some(resource) = request::request_resource(request) else {
        return Ok(());
    };
    let account = request::request_account(request);
    let store = store.bounded_by(deadline);
    let probe = || request::read_rate_limit(request, deadline);
    let now = now_epoch_seconds();
    store
        .admit_unless_expired(&policy, &account, caller, resource, now, &probe)
        .map_err(|refusal| {
            Box::new(DecidedRefusal {
                refusal,
                store,
                account,
                now,
            })
        })
}

/// Record what `result` of an admitted `request` says about the quota: an
/// authenticated primary rate-limit answer holds every read of its resource
/// on the host until the reset. Waits for the shared record no longer than
/// `deadline` (from [`gh_request_deadline`]), so post-child accounting stays
/// inside the caller's remaining budget. `None` leaves each record-lock wait
/// at the shared file-lock default of 30 seconds.
pub fn observe_gh_result_by_deadline(
    request: &ExecRequest,
    result: &ExecutionResult,
    deadline: Option<Instant>,
) {
    if !result.success {
        observe_gh_failure_by_deadline(request, &result.stderr, deadline);
    }
}

/// [`observe_gh_result_by_deadline`] for a failed `request` whose diagnostic is
/// `stderr`.
pub fn observe_gh_failure_by_deadline(
    request: &ExecRequest,
    stderr: &str,
    deadline: Option<Instant>,
) {
    let Some((store, policy)) = active_guard() else {
        return;
    };
    let Some(resource) = request::request_resource(request) else {
        return;
    };
    let Some(throttle) = crate::gh_request::read_throttle(stderr) else {
        return;
    };
    // A secondary limit names no account; it throttles one request pattern,
    // not the host's bucket.
    if throttle.principal.is_none() {
        return;
    }
    let resource = throttle
        .resource
        .as_deref()
        .filter(|named| *named == RESOURCE_CORE || *named == RESOURCE_GRAPHQL)
        .unwrap_or(resource);
    store.bounded_by(deadline).record_throttle(
        &policy,
        &request::request_account(request),
        resource,
        &throttle,
        now_epoch_seconds(),
    );
}

/// `request` with the budget left before `deadline`, or `None` when its finite
/// budget is exhausted.
fn remaining_request_budget(
    request: &ExecRequest,
    deadline: Option<Instant>,
) -> Option<ExecRequest> {
    let Some(deadline) = deadline else {
        return Some(request.clone());
    };
    let remaining_ms = remaining_millis(deadline);
    if remaining_ms == 0 {
        return None;
    }
    let mut bounded = request.clone();
    bounded.timeout_ms = Some(remaining_ms);
    Some(bounded)
}

fn timed_out_before_start(duration_ms: u64) -> ExecutionResult {
    ExecutionResult {
        success: false,
        timed_out: true,
        stdout: String::new(),
        stderr: "process timed out before starting".to_string(),
        exit_code: None,
        duration_ms,
        output: None,
    }
}

/// [`admit_gh_request`], run `request` without a sandbox with the remaining
/// timeout budget, then [`observe_gh_failure_by_deadline`]. One deadline,
/// taken when this starts, bounds the admission's lock waits and probe, the
/// child and the post-call accounting.
pub fn run_gh_request(
    caller: GithubCaller,
    request: &ExecRequest,
) -> Result<ExecutionResult, OrbitError> {
    if request.timeout_ms == Some(0) {
        return Ok(timed_out_before_start(0));
    }
    let started = store::monotonic_now();
    let deadline = gh_request_deadline(request, started);
    let admitted = admit_by_deadline(caller, request, deadline);
    // The budget is checked before a refusal propagates: a refusal decided
    // once the budget is spent is the timeout, not a throttle to wait out, and
    // only a refusal that survives this check is counted.
    let Some(bounded_request) = remaining_request_budget(request, deadline) else {
        return Ok(timed_out_before_start(
            u64::try_from(
                store::monotonic_now()
                    .saturating_duration_since(started)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
        ));
    };
    admitted.map_err(|decided| decided.deliver(request, true))?;
    let result = run_process(&bounded_request, &NoSandbox)?;
    if !result.success {
        observe_gh_failure_by_deadline(&bounded_request, &result.stderr, deadline);
    }
    Ok(result)
}

/// The typed throttle a local refusal carries: the shape of GitHub's own
/// answer, so a caller that waits for a reset keeps working.
fn refusal_error(refusal: &QuotaRefusal, request: &ExecRequest) -> OrbitError {
    let resume = store::rfc3339(refusal.resume_epoch_seconds)
        .unwrap_or_else(|| refusal.resume_epoch_seconds.to_string());
    let why = match refusal.reason {
        QuotaRefusalReason::Exhausted => format!(
            "GitHub's `{}` API quota for this host's account is exhausted",
            refusal.resource
        ),
        QuotaRefusalReason::Reserve => format!(
            "GitHub's `{}` API quota for this host's account is down to {} of {} calls, \
             within the {} reserve of {} calls kept for delivery",
            refusal.resource,
            refusal.remaining.unwrap_or_default(),
            refusal.limit.unwrap_or_default(),
            refusal.caller.as_str(),
            refusal.reserve
        ),
    };
    let operation = request
        .args
        .iter()
        .take(2)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let operation = format!("gh {operation}");
    let now = now_epoch_seconds();
    OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
        message: format!(
            "{operation} refused locally without calling GitHub: {why}. Reads of `{}` may \
             resume at {resume}; do not retry before then.",
            refusal.resource
        ),
        operation,
        class: PrivateVcsFailureClass::Other,
        read_throttle: Some(ProviderReadThrottle {
            retry_after_seconds: Some(refusal.resume_epoch_seconds.saturating_sub(now)),
            reset_epoch_seconds: Some(refusal.resume_epoch_seconds),
            resource: Some(refusal.resource.clone()),
            principal: None,
            request_id: None,
        }),
    }))
}
