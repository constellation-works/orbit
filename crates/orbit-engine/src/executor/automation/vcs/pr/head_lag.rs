//! Bounded settling of PR metadata after a verified replacement push.
//!
//! Only the exact previous published head can lag. The remote PR head ref
//! must already name the candidate; metadata must catch up before completion
//! applies its unchanged delivery and review checks [ORB-14315].

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::Value;

use super::super::git::git_output;
use super::delivery::DeliveryPin;

const MAX_STALE_HEAD_REREADS: u32 = 3;
const MAX_STALE_HEAD_WAIT_SECONDS: u64 = 60;

pub(super) struct HeadLag {
    pub(super) observations: u32,
    waited_seconds: u64,
    wait_budget_seconds: u64,
    poll_interval_seconds: u64,
}

impl HeadLag {
    pub(super) fn new(max_wait_seconds: u64, poll_interval_seconds: u64) -> Self {
        Self {
            observations: 0,
            waited_seconds: 0,
            wait_budget_seconds: MAX_STALE_HEAD_WAIT_SECONDS.min(max_wait_seconds / 4),
            poll_interval_seconds,
        }
    }

    /// Return a poll delay only for the known previous head. `None` leaves
    /// the caller's exact SHA checks in charge, including on exhaustion.
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
        let remote = git_output(
            Path::new(workspace_path),
            &["ls-remote", "origin", &reference],
        )
        .map_err(|error| {
            OrbitError::Execution(format!(
                "{error_code}: cannot confirm pull request #{pr_number} remote head: {error}; \
                 the task stays in review"
            ))
        })?;
        let remote_sha = remote.lines().find_map(|line| {
            let (sha, remote_ref) = line.split_once('\t')?;
            (remote_ref == reference).then_some(sha)
        });
        if remote_sha != Some(candidate) {
            return Err(OrbitError::Execution(format!(
                "{error_code}: pull request #{pr_number} remote head {remote_sha:?} is not \
                 the pinned candidate '{candidate}'; the task stays in review"
            )));
        }
        if !stale {
            return Ok(None);
        }
        self.observations += 1;
        let delay = self
            .poll_interval_seconds
            .min(self.wait_budget_seconds.saturating_sub(self.waited_seconds))
            .min(remaining_seconds);
        if self.observations > MAX_STALE_HEAD_REREADS || delay == 0 {
            return Ok(None);
        }
        self.waited_seconds += delay;
        tracing::info!(
            pr_number,
            observations = self.observations,
            delay,
            "waiting for PR head metadata after publication"
        );
        Ok(Some(delay))
    }
}
