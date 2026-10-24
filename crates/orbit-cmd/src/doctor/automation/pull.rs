//! Pull-drain health: protocol skew on the latest pull, validation gates that
//! drifted from the owner's, and follower settlements not yet delivered.

use orbit_core::application::distributed::PendingPullSettlements;

use crate::doctor::*;

/// The latest pull window remains actionable after it ends on wire skew.
/// A newer healthy window supersedes that diagnosis, avoiding stale warnings.
pub(in crate::doctor) fn doctor_check_pull_protocol(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    const CHECK: &str = "pull-protocol";
    let inspect = || -> Result<Option<String>, OrbitError> {
        let runs = runtime.list_job_runs(orbit_core::application::job::JobRunListParams {
            job_id: Some(orbit_core::application::contracts::PULL_DRAIN_JOB.to_string()),
            limit: Some(1),
            ..Default::default()
        })?;
        let Some(run) = runs.first() else {
            return Ok(None);
        };
        let state = runtime.read_run_state(&run.run_id)?;
        let pass = state
            .as_ref()
            .and_then(|state| state.pull_drain.drain_last_pass.as_ref());
        let failure = run
            .steps
            .iter()
            .find(|step| step.error_code.as_deref() == Some("protocol_skew"));
        if failure.is_some()
            || pass
                .is_some_and(|pass| pass.last_pass_error_code.as_deref() == Some("protocol_skew"))
        {
            let detail = failure
                .and_then(|step| step.error_message.as_deref())
                .or_else(|| pass.and_then(|pass| pass.last_pass_error.as_deref()))
                .unwrap_or("pull request schema mismatch");
            return Ok(Some(format!(
                "{} ended {} with protocol_skew: {detail}",
                run.run_id, run.state
            )));
        }
        Ok(None)
    };
    match inspect() {
        Ok(Some(message)) => actionable_check(CHECK, WorkspaceDoctorStatus::Warning, message,
            "Deploy matching Orbit builds on the owner and follower, restart their long-lived processes, then start a new pull drain.".to_string()),
        Ok(None) => check(CHECK, WorkspaceDoctorStatus::Ok, "no protocol skew recorded on the latest pull drain".to_string()),
        Err(error) => check(CHECK, WorkspaceDoctorStatus::Warning, format!("cannot inspect pull protocol: {error}")),
    }
}

/// [ORB-15622] Whether this replica's `workflow.required_validation_commands`
/// and `review.baseline_commands` differ from the owner's, as the latest pull
/// drain recorded the owner's from its probe at submission. Claimed leaves
/// validate against the owner's gates each claim captures, but a leaf of an
/// older owner falls back to this copy, and untracked config drifts silently.
pub(in crate::doctor) fn doctor_check_pull_gates(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "pull-gates";
    let inspect = || -> Result<Option<Option<String>>, OrbitError> {
        let runs = runtime.list_job_runs(orbit_core::application::job::JobRunListParams {
            job_id: Some(orbit_core::application::contracts::PULL_DRAIN_JOB.to_string()),
            limit: Some(1),
            ..Default::default()
        })?;
        let Some(run) = runs.first() else {
            return Ok(None);
        };
        runtime.pull_gate_drift(&run.run_id).map(Some)
    };
    match inspect() {
        Ok(None) => check(
            CHECK,
            WorkspaceDoctorStatus::Skipped,
            "no pull drain has run from this checkout".to_string(),
        ),
        Ok(Some(None)) => check(
            CHECK,
            WorkspaceDoctorStatus::Ok,
            "validation gates match the owner's as the latest pull drain recorded them".to_string(),
        ),
        Ok(Some(Some(drift))) => actionable_check(
            CHECK,
            WorkspaceDoctorStatus::Warning,
            drift,
            "Copy the owner's `workflow.required_validation_commands` and \
             `review.baseline_commands` into this replica's `.orbit/config.toml` (`orbit config \
             get` on the owner shows them), then start a new pull drain."
                .to_string(),
        ),
        Err(error) => check(
            CHECK,
            WorkspaceDoctorStatus::Warning,
            format!("cannot inspect pull validation gates: {error}"),
        ),
    }
}

/// Follower pull settlements recorded locally but never delivered to the
/// owner. Nothing retries delivery on a timer, so the owner keeps the claim
/// `running` until an operator runs a settle-only pass; this row is what makes
/// that wait visible. A workspace that never pulled reports none.
pub(in crate::doctor) fn doctor_check_pull_settlements(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    pull_settlements_finding(runtime.pending_pull_settlements(), chrono::Utc::now())
}

fn pull_settlements_finding(
    pending: Result<PendingPullSettlements, OrbitError>,
    now: chrono::DateTime<chrono::Utc>,
) -> WorkspaceDoctorResult {
    const CHECK: &str = "pull-settlements";
    let pending = match pending {
        Ok(pending) => pending,
        Err(error) => {
            return check(
                CHECK,
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect pull settlements: {error}"),
            );
        }
    };
    if pending.count == 0 {
        return check(
            CHECK,
            WorkspaceDoctorStatus::Ok,
            "no pull settlements are waiting to be delivered to an owner".to_string(),
        );
    }
    let oldest = pending
        .oldest_age(now)
        .map(|age| format!(", the oldest waiting {}", format_age(age)))
        .unwrap_or_default();
    actionable_check(
        CHECK,
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} pull settlement(s) recorded here but not delivered to the owner{oldest}; the \
             owner still shows those claims as running and nothing retries on a timer",
            pending.count
        ),
        "Run `orbit run auto --stop` once the owner is reachable to deliver them, then rerun \
         `orbit doctor`."
            .to_string(),
    )
}

/// Coarse age for a diagnostic line: the two most significant units.
pub(super) fn format_age(age: chrono::Duration) -> String {
    let seconds = age.num_seconds().max(0);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}
