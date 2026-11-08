//! Bounded waits for a throttled failed-check log read [ORB-20178].
//!
//! A provider adapter types an authenticated throttling answer
//! ([`orbit_common::ProviderReadThrottle`]); this policy decides whether the
//! same pinned read may repeat, and when. It waits for the provider's
//! not-before (`Retry-After` or the rate-limit reset), or, when the answer
//! named neither, for a conservative backoff recorded as an unknown reset.
//! Every wait must end before the completion's original absolute deadline,
//! which it never extends, and one exact read (pull request, head, run, job
//! and run attempt) earns at most [`MAX_THROTTLED_READ_WAITS`] waits. Each
//! wait is recorded on the watched tasks first, so a resumed completion of
//! the same run lineage honors it before reading again.
//!
//! Waiting is read-only: it admits no repair, spends no repair, rerun,
//! base-update or merge budget, and the poll after it re-reads the pull
//! request's head and required checks before any evidence is used. Anything
//! this policy refuses keeps the held stop an unreadable log always had,
//! with the provider's original diagnostic. Each wait is a
//! [`PrAutoFixReadDeferral`]: the producer builds that record, and every
//! reader takes it from [`PrAutoFixReadDeferral::from_entry`].

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_common::text::floor_char_boundary;
use orbit_types::workflow::{DeliveryRefusal, PrAutoFixReadDeferral, PrAutoFixReadReset};

use crate::context::RuntimeHost;
use crate::executor::automation::read_throttle::read_not_before;

use super::super::super::completion_deadline::now_ms;
use super::super::super::landing::checks::DeferredLogRead;
use super::{admission, ledger, verdict};

/// Waits one exact read may earn before completion stops holding.
const MAX_THROTTLED_READ_WAITS: u64 = 3;
const MAX_DIAGNOSTIC_BYTES: usize = 2_048;

/// Record the wait a throttled read earns and return it in milliseconds, or
/// the held stop when no wait fits: a completion mutation already ran, the
/// read stayed throttled through every wait, or the not-before is past the
/// original `deadline_ms` (`None` when completion has no wait budget).
pub(in crate::executor::automation::vcs::pr) fn defer_throttled_read<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    deferred: &DeferredLogRead,
    deadline_ms: Option<u64>,
    mutated: bool,
) -> Result<u64, OrbitError> {
    let held = |why: &str| {
        verdict::hold_auto_fix_stop(
            verdict::stopped(pr_number, &format!("{}; {why}", deferred.reason)),
            DeliveryRefusal::CheckEvidenceInconclusive,
        )
    };
    if mutated {
        return Err(held(
            "a completion mutation already ran, so the read is not repeated",
        ));
    }
    let Some(deadline_ms) = deadline_ms else {
        return Err(held("completion has no wait budget to repeat the read"));
    };
    let attempt = recorded(host, auto_fix, pr_number, &deferred.head)?
        .iter()
        .filter(|record| same_read(record, deferred))
        .map(|record| record.attempt)
        .max()
        .unwrap_or(0)
        + 1;
    if attempt > MAX_THROTTLED_READ_WAITS {
        return Err(held(&format!(
            "the provider kept throttling the read after {MAX_THROTTLED_READ_WAITS} bounded waits"
        )));
    }
    let now = now_ms()?;
    let throttle = &deferred.throttle;
    let not_before = read_not_before(throttle, now, attempt);
    let provider_named = not_before.provider_named;
    let not_before = not_before.at_ms;
    if not_before >= deadline_ms {
        return Err(held(&format!(
            "the provider's not-before ({not_before} ms) is not before the completion deadline \
             ({deadline_ms} ms), which is never extended"
        )));
    }
    let mut diagnostic = redact_all(&deferred.reason);
    diagnostic.truncate(floor_char_boundary(&diagnostic, MAX_DIAGNOSTIC_BYTES));
    let unavailable =
        |value: &Option<String>| value.clone().unwrap_or_else(|| "unavailable".to_owned());
    let record = PrAutoFixReadDeferral {
        execution: auto_fix.run_id.clone(),
        pr: pr_number.to_owned(),
        head_sha: deferred.head.clone(),
        repository: deferred.repository.clone(),
        run_id: deferred.run_id,
        job_id: deferred.job_id,
        run_attempt: deferred.run_attempt,
        attempt,
        observed_at_ms: now,
        not_before_ms: Some(not_before),
        deadline_ms,
        reset: if provider_named {
            PrAutoFixReadReset::Provider
        } else {
            PrAutoFixReadReset::Unknown
        },
        retry_after_seconds: throttle.retry_after_seconds,
        reset_epoch_seconds: throttle.reset_epoch_seconds,
        resource: unavailable(&throttle.resource),
        principal: unavailable(&throttle.principal),
        request_id: unavailable(&throttle.request_id),
        diagnostic: Some(diagnostic),
    };
    // An unrecorded wait could not bind a resume, so it is not taken.
    ledger::record_read_deferral(host, auto_fix, &record)
        .map_err(|error| held(&format!("the read wait could not be recorded: {error}")))?;
    Ok(not_before - now)
}

/// The wait, in milliseconds, still owed to a wait this run lineage recorded
/// for `head` before a poll reads its logs again, or `None` when none is
/// owed. A recorded not-before at or past `deadline_ms` holds instead.
pub(in crate::executor::automation::vcs::pr) fn owed_read_wait<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    head: &str,
    deadline_ms: Option<u64>,
) -> Result<Option<u64>, OrbitError> {
    let Some(record) = recorded(host, auto_fix, pr_number, head)?
        .into_iter()
        .max_by_key(|record| record.not_before_ms)
    else {
        return Ok(None);
    };
    let not_before = record.effective_not_before_ms();
    let now = now_ms()?;
    if not_before <= now {
        return Ok(None);
    }
    if deadline_ms.is_none_or(|deadline| not_before >= deadline) {
        return Err(verdict::hold_auto_fix_stop(
            verdict::stopped(
                pr_number,
                &format!(
                    "{}; the recorded read wait does not end before the completion deadline",
                    record.diagnostic_text()
                ),
            ),
            DeliveryRefusal::CheckEvidenceInconclusive,
        ));
    }
    Ok(Some(not_before - now))
}

/// This run lineage's recorded waits for `head` on the pull request.
fn recorded<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    head: &str,
) -> Result<Vec<PrAutoFixReadDeferral>, OrbitError> {
    let mut records = Vec::new();
    for task in &auto_fix.task_ids {
        for entry in host.get_task_history(task)? {
            let Some(record) = PrAutoFixReadDeferral::from_entry(&entry) else {
                continue;
            };
            if record.pr == pr_number
                && record.head_sha == head
                && auto_fix.ledger_runs.contains(&record.execution)
            {
                records.push(record);
            }
        }
    }
    Ok(records)
}

fn same_read(record: &PrAutoFixReadDeferral, deferred: &DeferredLogRead) -> bool {
    record.run_id == deferred.run_id
        && record.job_id == deferred.job_id
        && record.run_attempt == deferred.run_attempt
}
