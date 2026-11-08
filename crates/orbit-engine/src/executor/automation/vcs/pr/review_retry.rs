//! Review-watch access to the existing completion retry and read-deferral path.
use super::super::landing::checks::rerun_diagnostic::CiRerunDiagnostic;
use super::super::remote_probe::fixed_wait;
use super::answers::PullRequestStatus;
use super::auto_fix::{
    AutoFix, CiRerunRecovery, defer_throttled_read, owed_read_wait, recover_ci_rerun,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use serde_json::{Value, json};
use std::time::Duration;

pub(in crate::executor::automation::vcs) enum ReviewCiRetryStep {
    Observe,
    PollAgain,
    Hold(String),
}

/// Retry CI only. This does not call repair admission, merge or change status.
pub(in crate::executor::automation::vcs) fn recover_review_ci(
    host: &(impl RuntimeHost + ?Sized),
    input: &Value,
    task: &str,
    pr: &str,
    status: &PullRequestStatus,
    deadline_ms: u64,
) -> Result<ReviewCiRetryStep, OrbitError> {
    let mut retry_input = input.clone();
    retry_input["auto_fix"] = json!(true);
    retry_input["completion"] = json!("review");
    if let Some(base) = status.base_branch.as_ref() {
        retry_input["base"] = json!(base);
    }
    let execution = format!("review-ci-{task}-{pr}");
    let Some(auto_fix) = AutoFix::from_input(host, &retry_input, &[task.to_string()], &execution)?
    else {
        return Ok(ReviewCiRetryStep::Observe);
    };
    let Some(head) = status.head_sha.as_deref() else {
        return Ok(ReviewCiRetryStep::Observe);
    };
    if let Some(wait_ms) = owed_read_wait(host, &auto_fix, pr, head, Some(deadline_ms))? {
        fixed_wait(Duration::from_millis(wait_ms));
        return Ok(ReviewCiRetryStep::PollAgain);
    }
    let mut diagnostic: Option<CiRerunDiagnostic> = None;
    match recover_ci_rerun(
        host,
        &auto_fix,
        &retry_input,
        input["workspace_path"].as_str().unwrap_or_default(),
        pr,
        status,
        deadline_ms,
        &mut diagnostic,
    ) {
        Ok(CiRerunRecovery::Retried | CiRerunRecovery::BackingOff(_)) => {
            Ok(ReviewCiRetryStep::PollAgain)
        }
        Ok(CiRerunRecovery::Stopped(reason)) => Ok(ReviewCiRetryStep::Hold(reason)),
        Ok(CiRerunRecovery::Waiting(reason)) if reason.starts_with("CI retry was reserved") => {
            Ok(ReviewCiRetryStep::PollAgain)
        }
        Ok(CiRerunRecovery::NotEligible | CiRerunRecovery::Waiting(_)) => {
            Ok(ReviewCiRetryStep::Observe)
        }
        Err(error) => {
            if let Some(deferred) = diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.throttled_log_read(&error))
            {
                let wait_ms =
                    defer_throttled_read(host, &auto_fix, pr, &deferred, Some(deadline_ms), false)?;
                fixed_wait(Duration::from_millis(wait_ms));
                Ok(ReviewCiRetryStep::PollAgain)
            } else {
                // Observation keeps its existing inconclusive/read-gap report;
                // an unavailable retry plan grants no mutation authority.
                tracing::debug!(%error, "review CI retry evidence unavailable");
                Ok(ReviewCiRetryStep::Observe)
            }
        }
    }
}
