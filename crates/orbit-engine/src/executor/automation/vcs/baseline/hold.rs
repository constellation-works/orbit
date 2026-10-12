use super::super::base_claim::failing_tests_selection;
use super::super::git::{fetch_remote_base, git_output};
use super::cache::{cache_paths, cached_base_check, read_cached};
use super::compare::{
    BaseCommandResult, BaselineCheck, compare_with_base, hold_failures, not_comparable,
};
use crate::context::RuntimeHost;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_types::workflow::BaselineRedHold;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often one process may refresh a held base from `origin`.
const HOLD_FETCH_INTERVAL: Duration = Duration::from_secs(120);

/// An inconclusive hold check may retry after this delay, or immediately
/// when its base moves. This marker is separate from conclusive results so
/// candidate validation never treats it as a cached command result.
const HOLD_INCONCLUSIVE_BACKOFF: chrono::Duration = chrono::Duration::minutes(15);

/// Whether a held task's base has moved to a commit where its command may
/// pass, observed in `repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineHoldStatus {
    /// The base still fails the command; why.
    Holding(String),
    /// The hold no longer applies; why.
    Lifted(String),
}

/// Decide whether a [`BaselineRedHold`] still stands in `repo`.
///
/// The hold stands while its base ref still points at the red commit. When
/// that ref advances, first replay its recorded failing tests when the hold
/// has a runnable test list and selection. A comparable failure keeps the hold
/// without a full run. Otherwise run the full recorded selection, and lift the
/// hold only after that comparable run passes. Both runs use the shared cache.
/// A failed, inconclusive or not-comparable check keeps the task held. An inconclusive attempt is recorded
/// separately and suppresses further hold checks for 15 minutes on that tip
/// and command; a conclusive cached result always takes precedence.
/// A remote-tracking ref is refreshed
/// from `origin` at most every `HOLD_FETCH_INTERVAL` per repository and
/// branch, so a held backlog does not wait on some other delivery to fetch.
///
/// A cache miss may run the whole command, for up to the validation timeout, so
/// only a background run may ask: the owner's clock tick reads
/// [`recorded_baseline_hold_status`] and leaves a miss to a detached refresh
/// run [ORB-14739, ORB-14823].
pub fn baseline_hold_status<H: RuntimeHost + ?Sized>(
    host: &H,
    repo: &Path,
    hold: &BaselineRedHold,
) -> BaselineHoldStatus {
    match moved_base_tip(repo, hold) {
        Ok(tip) => {
            if let Some(status) = recorded_moved_base_status(repo, hold, &tip) {
                return status;
            }
            if let Some(selection) = hold_failing_tests_selection(hold) {
                let check = compare_with_base(host, repo, &tip, &hold.command, Some(&selection));
                if let Some(status) = failed_replay_status(hold, &tip, &selection, check) {
                    return status;
                }
                // A passing replay covers only the recorded failures. An inconclusive
                // or non-comparable one says nothing about them. In each case the
                // full selection must run before this tip can lift the hold.
            }
            let check = compare_with_base(host, repo, &tip, &hold.command, hold.selection.as_ref());
            if let Err(reason) = &check.result {
                record_inconclusive_hold(repo, &tip, hold, reason);
            }
            judge_moved_base(hold, &tip, hold.selection.as_ref(), check.result)
        }
        Err(status) => status,
    }
}

/// [`baseline_hold_status`] from what is already recorded, never running the
/// command: `None` when the base moved to a tip with neither a conclusive
/// result nor an inconclusive hold attempt within the 15-minute back-off,
/// so deciding needs a replay or full run.
///
/// It still reads the base ref, refreshing it from `origin` as
/// [`baseline_hold_status`] does, so it fits a short caller such as the clock
/// tick, which holds the host's sweep lock.
pub fn recorded_baseline_hold_status(
    repo: &Path,
    hold: &BaselineRedHold,
) -> Option<BaselineHoldStatus> {
    let tip = match moved_base_tip(repo, hold) {
        Ok(tip) => tip,
        Err(status) => return Some(status),
    };
    recorded_moved_base_status(repo, hold, &tip)
}

/// A recent failed attempt to judge a hold, keyed by tip and command using
/// the cache path. It never represents a conclusive validation result.
#[derive(Serialize, Deserialize)]
struct InconclusiveHoldAttempt {
    recorded_at: chrono::DateTime<Utc>,
    reason: String,
}

fn record_inconclusive_hold(repo: &Path, tip: &str, hold: &BaselineRedHold, reason: &str) {
    let command = hold.command.as_str();
    let record = || -> Result<(), OrbitError> {
        let cache = cache_paths(repo, tip, command, hold.selection.as_ref())?;
        let bytes = serde_json::to_vec(&InconclusiveHoldAttempt {
            recorded_at: Utc::now(),
            reason: reason.to_string(),
        })
        .map_err(|error| OrbitError::Execution(format!("encode inconclusive hold: {error}")))?;
        atomic_write_bytes(&cache.inconclusive_hold, &bytes)?;
        Ok(())
    };
    if let Err(error) = record() {
        tracing::warn!(
            tip,
            command,
            "could not record inconclusive hold attempt: {error}"
        );
    }
}

fn recorded_moved_base_status(
    repo: &Path,
    hold: &BaselineRedHold,
    tip: &str,
) -> Option<BaselineHoldStatus> {
    let result = match cache_paths(repo, tip, &hold.command, hold.selection.as_ref()) {
        Ok(cache) => match read_cached(&cache.result) {
            Some(result) => Ok(result),
            None => {
                if let Some(selection) = hold_failing_tests_selection(hold)
                    && let Some(check) =
                        cached_base_check(repo, tip, &hold.command, Some(&selection))
                    && let Some(status) = failed_replay_status(hold, tip, &selection, check)
                {
                    return Some(status);
                }
                let bytes = std::fs::read(&cache.inconclusive_hold).ok()?;
                let attempt: InconclusiveHoldAttempt = serde_json::from_slice(&bytes).ok()?;
                let age = Utc::now().signed_duration_since(attempt.recorded_at);
                if age >= HOLD_INCONCLUSIVE_BACKOFF {
                    return None;
                }
                Err(attempt.reason)
            }
        },
        // A full run could not locate the cache either.
        Err(error) => Err(format!("locate the base result cache: {error}")),
    };
    Some(judge_moved_base(hold, tip, hold.selection.as_ref(), result))
}

/// Use only runnable test identities from the hold's persisted failure record.
/// Older holds, build failures and shortened identities keep the full-run path.
fn hold_failing_tests_selection(hold: &BaselineRedHold) -> Option<Value> {
    let failures = hold.failures.identities.iter().cloned().collect();
    failing_tests_selection(hold.selection.as_ref(), &failures, &[])
}

/// A comparable failed replay is enough to keep a tip red; it can never lift it.
fn failed_replay_status(
    hold: &BaselineRedHold,
    tip: &str,
    selection: &Value,
    check: BaselineCheck,
) -> Option<BaselineHoldStatus> {
    if check.not_comparable.is_none() && check.result.as_ref().is_ok_and(|result| !result.passed) {
        Some(judge_moved_base(hold, tip, Some(selection), check.result))
    } else {
        None
    }
}

/// The commit the hold's base ref now names, when it moved off the red
/// commit; otherwise the status that keeps the hold.
fn moved_base_tip(repo: &Path, hold: &BaselineRedHold) -> Result<String, BaselineHoldStatus> {
    let base_ref = hold.base_ref.trim();
    if base_ref.is_empty() {
        return Err(BaselineHoldStatus::Holding(
            "the hold names no base ref".to_string(),
        ));
    }
    if let Some(branch) = base_ref.strip_prefix("origin/")
        && fetch_due(repo, branch)
        && let Err(error) = fetch_remote_base(repo, branch)
    {
        tracing::debug!(branch, "baseline hold could not refresh its base: {error}");
    }
    let tip = git_output(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base_ref}^{{commit}}"),
        ],
    )
    .map_err(|error| {
        BaselineHoldStatus::Holding(format!(
            "`{base_ref}` cannot be read, so the new base cannot be checked: {error}"
        ))
    })?;
    if tip == hold.base_sha {
        return Err(BaselineHoldStatus::Holding(format!(
            "`{base_ref}` is still at {tip}, where required validation `{}` fails{}",
            hold.command,
            hold.failures_clause()
        )));
    }
    Ok(tip)
}

/// The hold's status once its base moved to `tip`, from the command's result
/// there or why there is none.
fn judge_moved_base(
    hold: &BaselineRedHold,
    tip: &str,
    selection: Option<&Value>,
    result: Result<BaseCommandResult, String>,
) -> BaselineHoldStatus {
    let base_ref = hold.base_ref.trim();
    // A hold recorded before holds kept a selection names nothing to compare
    // with, so its tip is judged by exit status as before; the next delivery
    // holds again, with the selection, if the base is still red.
    let unlike = selection.and_then(|selection| {
        result
            .as_ref()
            .ok()
            .and_then(|result| not_comparable(Some(selection), result))
    });
    // [ORB-15374] A verdict that keeps the hold names what the held base
    // failed, so the readiness detail it becomes says what is red.
    let held = hold.failures_clause();
    match (result, unlike) {
        (Ok(_), Some(reason)) => BaselineHoldStatus::Holding(format!(
            "`{base_ref}` moved to {tip}, but the run of required validation `{}` there is not \
             comparable with the held one: {reason}{held}",
            hold.command
        )),
        (Ok(result), None) if result.passed => BaselineHoldStatus::Lifted(format!(
            "`{base_ref}` moved from {} to {tip}, where required validation `{}` passes",
            hold.base_sha, hold.command
        )),
        (Ok(result), None) => {
            let now = hold_failures(result.failures, &result.output);
            BaselineHoldStatus::Holding(format!(
                "`{base_ref}` moved to {tip}, where required validation `{}` still fails{}",
                hold.command,
                now.describe()
                    .map(|described| format!("; {described}"))
                    .unwrap_or(held)
            ))
        }
        (Err(reason), _) => BaselineHoldStatus::Holding(format!(
            "`{base_ref}` moved to {tip}, but required validation `{}` could not be checked: \
             {reason}{held}",
            hold.command
        )),
    }
}

/// Whether `branch` in `repo` may be fetched again for a hold check.
fn fetch_due(repo: &Path, branch: &str) -> bool {
    static LAST: OnceLock<Mutex<HashMap<(PathBuf, String), Instant>>> = OnceLock::new();
    let Ok(mut last) = LAST.get_or_init(Default::default).lock() else {
        return false;
    };
    let key = (repo.to_path_buf(), branch.to_string());
    let now = Instant::now();
    if last
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < HOLD_FETCH_INTERVAL)
    {
        return false;
    }
    last.insert(key, now);
    true
}
