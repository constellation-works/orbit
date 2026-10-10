//! Bounded settling of PR metadata and the PR head ref after a verified
//! replacement push.
//!
//! Only the exact previous published head can lag. PR metadata (`headRefOid`)
//! may report it [ORB-14315]. The remote PR head ref (`refs/pull/<N>/head`)
//! may too, but only while the task branch ref on origin already names the
//! candidate, which proves the push landed [ORB-14663]. Both share one budget
//! of re-reads and waiting. Completion then applies its unchanged delivery and
//! review checks, so waiting never authorizes another head.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::Value;

use super::super::git::git_output;
use super::delivery::DeliveryPin;

const MAX_STALE_HEAD_REREADS: u32 = 3;
const MAX_STALE_HEAD_WAIT_SECONDS: u64 = 60;

pub(super) struct HeadLag {
    pub(super) observations: u32,
    /// The candidate the PR head ref was last seen at; once settled there it
    /// cannot lag again, so a later regression is not treated as lag.
    pull_ref_settled_at: Option<String>,
    waited_seconds: u64,
    wait_budget_seconds: u64,
    poll_interval_seconds: u64,
}

impl HeadLag {
    pub(super) fn new(max_wait_seconds: u64, poll_interval_seconds: u64) -> Self {
        Self {
            observations: 0,
            pull_ref_settled_at: None,
            waited_seconds: 0,
            wait_budget_seconds: MAX_STALE_HEAD_WAIT_SECONDS.min(max_wait_seconds / 4),
            poll_interval_seconds,
        }
    }

    /// Return a poll delay only for the known previous head. `None` leaves
    /// the caller's exact SHA checks in charge, including on exhaustion of
    /// metadata lag. A PR head ref still at the previous head when the bound
    /// ends is refused here, because the caller does not read that ref.
    pub(super) fn poll_delay(
        &mut self,
        status: &Value,
        pin: &DeliveryPin,
        reviewed_head_sha: Option<&str>,
        workspace_path: &str,
        pr_number: &str,
        remaining_seconds: u64,
    ) -> Result<Option<u64>, OrbitError> {
        let Some(candidate) = pin.candidate_sha().or(reviewed_head_sha) else {
            return Ok(None);
        };
        // A contradictory review pin is never metadata lag.
        if reviewed_head_sha.is_some_and(|reviewed| reviewed != candidate) {
            return Ok(None);
        }
        let reported = status
            .get("headRefOid")
            .and_then(Value::as_str)
            .map(str::trim);
        let previous = pin.previous_candidate_sha();
        let stale = previous.is_some_and(|previous| {
            !previous.is_empty() && previous != candidate && reported == Some(previous)
        });
        if !(stale || self.observations > 0 && reported == Some(candidate)) {
            return Ok(None);
        }

        // Verify each stale read and the read that catches up. A changed or
        // absent ref cannot authorize waiting on the provider's metadata.
        let reference = format!("refs/pull/{pr_number}/head");
        let error_code = if pin.candidate_sha().is_some() {
            "delivery_evidence_stale"
        } else {
            "review_gate_stale"
        };
        let remote_sha = remote_ref_sha(workspace_path, &reference, pr_number, error_code)?;
        let ref_lags = remote_sha.as_deref() != Some(candidate);
        if !ref_lags {
            self.pull_ref_settled_at = Some(candidate.to_string());
        }
        if ref_lags {
            // Only the known previous head may lag, and only once the task
            // branch independently shows the replacement push landed.
            let replaced = previous.is_some_and(|previous| {
                !previous.is_empty()
                    && previous != candidate
                    && remote_sha.as_deref() == Some(previous)
            });
            if !replaced || self.pull_ref_settled_at.as_deref() == Some(candidate) {
                return Err(OrbitError::Execution(format!(
                    "{error_code}: pull request #{pr_number} remote head {remote_sha:?} is not \
                     the pinned candidate '{candidate}'; the task stays in review"
                )));
            }
            let branch = pin
                .head()
                .or_else(|| status.get("headRefName").and_then(Value::as_str))
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
                .ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "{error_code}: pull request #{pr_number} remote head still names the \
                         previous head and the task branch is unknown; the task stays in review"
                    ))
                })?;
            let branch_ref = format!("refs/heads/{branch}");
            let branch_sha = remote_ref_sha(workspace_path, &branch_ref, pr_number, error_code)?;
            if branch_sha.as_deref() != Some(candidate) {
                return Err(OrbitError::Execution(format!(
                    "{error_code}: pull request #{pr_number} remote head still names the previous \
                     head and branch '{branch}' {branch_sha:?} is not the pinned candidate \
                     '{candidate}'; the task stays in review"
                )));
            }
        }
        if !stale && !ref_lags {
            return Ok(None);
        }
        self.observations += 1;
        let delay = self
            .poll_interval_seconds
            .min(self.wait_budget_seconds.saturating_sub(self.waited_seconds))
            .min(remaining_seconds);
        if self.observations > MAX_STALE_HEAD_REREADS || delay == 0 {
            if ref_lags {
                return Err(OrbitError::Execution(format!(
                    "{error_code}: pull request #{pr_number} remote head still names the previous \
                     head after the bounded wait; the task stays in review"
                )));
            }
            return Ok(None);
        }
        self.waited_seconds += delay;
        tracing::info!(
            pr_number,
            observations = self.observations,
            delay,
            "waiting for PR head after publication"
        );
        Ok(Some(delay))
    }
}

/// The SHA `origin` reports for exactly `reference`; `None` when it is absent.
/// A failed `ls-remote` is an error, never lag.
fn remote_ref_sha(
    workspace_path: &str,
    reference: &str,
    pr_number: &str,
    error_code: &str,
) -> Result<Option<String>, OrbitError> {
    let remote = git_output(
        Path::new(workspace_path),
        &["ls-remote", "origin", reference],
    )
    .map_err(|error| {
        OrbitError::Execution(format!(
            "{error_code}: cannot confirm pull request #{pr_number} remote ref \
                 {reference}: {error}; the task stays in review"
        ))
    })?;
    Ok(remote.lines().find_map(|line| {
        let (sha, remote_ref) = line.split_once('\t')?;
        (remote_ref == reference).then(|| sha.to_string())
    }))
}
