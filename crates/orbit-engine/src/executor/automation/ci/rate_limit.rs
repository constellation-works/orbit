//! Deferring a CI sweep to GitHub's rate-limit reset.
//!
//! The REST quota belongs to the host's account, so once GitHub
//! rate-limits one read every other read in the sweep is refused too, and
//! the errors they leave would fail the sweep as retryable gaps that are
//! about the quota, not about any run. [`ThrottleWatch`] wraps the sweep's
//! queries: the first read GitHub answers with an authenticated rate limit
//! stops every later GitHub read, and collection then reports
//! [`OUTCOME_RATE_LIMITED`] with the not-before instead of a partial
//! snapshot. Filing turns that into a `deferred` sweep, not a failed run.
//!
//! The not-before is kept in
//! `<data root>/state/ci_failure_sweep/rate_limit.json`, so a sweep that
//! starts before it reads nothing from GitHub and defers again.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use orbit_common::{OrbitError, ProviderReadThrottle};
use serde_json::{Value, json};

use super::super::read_throttle::{read_not_before, rfc3339_ms};
use super::query::{AuthStatus, CiQueries, LogScope, RemoteBranchHeads, RunLog};

/// GitHub rate-limited the sweep's reads; the snapshot carries `rate_limit`
/// with the time the sweep may read again.
pub(super) const OUTCOME_RATE_LIMITED: &str = "rate_limited";
const MAX_REASON_CHARS: usize = 1_000;

/// The sweep's queries, stopped at the first rate-limited GitHub read.
pub(super) struct ThrottleWatch<'a, Q: CiQueries + ?Sized> {
    inner: &'a Q,
    seen: RefCell<Option<(ProviderReadThrottle, String)>>,
}

impl<'a, Q: CiQueries + ?Sized> ThrottleWatch<'a, Q> {
    pub(super) fn new(inner: &'a Q) -> Self {
        Self {
            inner,
            seen: RefCell::new(None),
        }
    }

    /// The first rate limit a read reported, with its diagnostic.
    pub(super) fn throttled(&self) -> Option<(ProviderReadThrottle, String)> {
        self.seen.borrow().clone()
    }

    fn read<T>(&self, run: impl FnOnce() -> Result<T, OrbitError>) -> Result<T, OrbitError> {
        if self.seen.borrow().is_some() {
            return Err(OrbitError::Execution(
                "not read: GitHub rate-limited an earlier read in this sweep".to_string(),
            ));
        }
        let result = run();
        if let Err(error) = &result
            && let Some(throttle) = error
                .private_vcs_failure()
                .and_then(|failure| failure.read_throttle.clone())
        {
            self.note(throttle, error.to_string());
        }
        result
    }

    fn note(&self, throttle: ProviderReadThrottle, reason: String) {
        self.seen.borrow_mut().get_or_insert((throttle, reason));
    }
}

impl<Q: CiQueries + ?Sized> CiQueries for ThrottleWatch<'_, Q> {
    fn auth_status(&self) -> AuthStatus {
        self.inner.auth_status()
    }
    fn repo_view(&self) -> Result<Value, OrbitError> {
        self.read(|| self.inner.repo_view())
    }
    fn open_pull_requests(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        self.read(|| self.inner.open_pull_requests(limit))
    }
    fn closed_pull_requests(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        self.read(|| self.inner.closed_pull_requests(limit))
    }
    fn closed_pull_requests_for_branch(&self, branch: &str) -> Result<Vec<Value>, OrbitError> {
        self.read(|| self.inner.closed_pull_requests_for_branch(branch))
    }
    fn open_pull_requests_for_branch(&self, branch: &str) -> Result<Vec<Value>, OrbitError> {
        self.read(|| self.inner.open_pull_requests_for_branch(branch))
    }
    fn repository_runs(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        self.read(|| self.inner.repository_runs(limit))
    }
    fn run_view(&self, run_id: &str) -> Result<Value, OrbitError> {
        self.read(|| self.inner.run_view(run_id))
    }
    fn run_logs(
        &self,
        run_id: &str,
        job_id: u64,
        scope: LogScope,
        max_bytes: usize,
        cached_view: Option<&Value>,
    ) -> Result<RunLog, OrbitError> {
        let log = self.read(|| {
            self.inner
                .run_logs(run_id, job_id, scope, max_bytes, cached_view)
        })?;
        if let Some(throttle) = log.read_throttle.clone() {
            self.note(throttle, log.fallback_error.clone().unwrap_or_default());
        }
        Ok(log)
    }
    fn job_concurrency_cancellation(&self, job_id: u64) -> Result<Option<String>, OrbitError> {
        self.read(|| self.inner.job_concurrency_cancellation(job_id))
    }
    // Git reads of origin spend no GitHub API quota.
    fn remote_branch_heads(&self) -> Result<RemoteBranchHeads, OrbitError> {
        self.inner.remote_branch_heads()
    }
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, OrbitError> {
        self.inner.is_ancestor(ancestor, descendant)
    }
}

/// The `rate_limit` record of a sweep GitHub rate-limited at `now_ms`.
pub(super) fn rate_limit_record(
    throttle: &ProviderReadThrottle,
    reason: &str,
    now_ms: u64,
) -> Value {
    let not_before = read_not_before(throttle, now_ms, 1);
    let reason: String = orbit_common::security::redaction::redact_all(reason)
        .chars()
        .take(MAX_REASON_CHARS)
        .collect();
    json!({
        "not_before_ms": not_before.at_ms,
        "not_before": rfc3339_ms(not_before.at_ms),
        "reset": if not_before.provider_named { "provider" } else { "unknown" },
        "retry_after_seconds": throttle.retry_after_seconds,
        "reset_epoch_seconds": throttle.reset_epoch_seconds,
        "resource": throttle.resource,
        "principal": throttle.principal,
        "observed_at_ms": now_ms,
        "reason": reason,
    })
}

/// The snapshot of a sweep deferred to `rate_limit`'s not-before.
pub(super) fn rate_limited_snapshot(
    schema_version: u64,
    capability: Value,
    rate_limit: Value,
    collected_at: &str,
) -> Value {
    let detail = format!(
        "GitHub rate-limited this host's reads, so the sweep is deferred until {}; nothing \
         was collected and nothing here is a CI pass",
        rate_limit["not_before"].as_str().unwrap_or("the reset")
    );
    json!({
        "schema_version": schema_version,
        "collected": false,
        "outcome_hint": OUTCOME_RATE_LIMITED,
        "capability": capability,
        "rate_limit": rate_limit,
        "detail": detail,
        "collected_at": collected_at,
    })
}

pub(super) fn rate_limit_path(data_root: &Path) -> Option<PathBuf> {
    (!data_root.as_os_str().is_empty()).then(|| {
        data_root
            .join("state")
            .join("ci_failure_sweep")
            .join("rate_limit.json")
    })
}

/// The recorded rate limit whose not-before is still after `now_ms`.
pub(super) fn pending_rate_limit(path: &Path, now_ms: u64) -> Option<Value> {
    let record: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let not_before = record.get("not_before_ms")?.as_u64()?;
    (not_before > now_ms).then_some(record)
}

/// Keep `record` for the next sweep; a failure only costs that sweep its
/// reads, so it is logged and otherwise ignored.
pub(super) fn save_rate_limit(path: &Path, record: &Value) {
    let saved = path
        .parent()
        .map_or(Ok(()), orbit_common::fs::io::create_private_dir_all)
        .and_then(|()| orbit_common::fs::io::atomic_write_text(path, &record.to_string()));
    if let Err(error) = saved {
        tracing::warn!(path = %path.display(), %error, "could not save the CI sweep rate limit");
    }
}
